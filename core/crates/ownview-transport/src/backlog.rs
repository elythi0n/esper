//! Output kept for a controller that is not attached right now.
//!
//! A parked terminal session must keep draining its pseudo-terminal or the
//! shell blocks on its next write — so the output has to go somewhere, and
//! that somewhere has to be bounded. A detached `yes` is not a hostile act,
//! but an unbounded buffer would turn it into an out-of-memory condition.
//!
//! # Loss is reported, never hidden
//!
//! When the cap is reached the oldest bytes go. Whoever reattaches is told how
//! many, and told *inline at the point of the loss*, because a gap in a
//! terminal transcript that is only mentioned in a status field is a gap
//! nobody notices — and someone reading a build log needs to know it is
//! incomplete.

use std::collections::VecDeque;

/// Bytes kept for a session with nobody attached.
///
/// 256 KiB is several screenfuls of scrollback and a small fraction of what a
/// chatty build produces. It is a deliberate compromise: large enough that a
/// brief network drop loses nothing, small enough that the per-device parked
/// session limit bounds total memory to something a small server does not
/// notice.
pub const DEFAULT_BACKLOG: usize = 256 * 1024;

/// A bounded, lossy-at-the-front record of terminal output.
#[derive(Debug)]
pub struct Backlog {
    buf: VecDeque<u8>,
    cap: usize,
    lost: u64,
    /// Whether the byte at the front might be the orphaned tail of a character
    /// whose start was dropped.
    ///
    /// A flag rather than a check inside `push`, because dropping the start of
    /// a character and receiving the rest of it are *different pushes*: with a
    /// small cap the continuation bytes can arrive after the truncating push
    /// has already returned. A per-push check would let them through, and the
    /// reader would see a replacement character that never resolves.
    truncated: bool,
}

impl Backlog {
    /// An empty backlog holding at most `cap` bytes.
    ///
    /// A `cap` of zero is allowed and means "keep nothing, count everything as
    /// lost" — useful to a caller that wants the shell kept alive but does not
    /// want its output retained.
    #[must_use]
    pub fn new(cap: usize) -> Self {
        Self {
            buf: VecDeque::new(),
            cap,
            lost: 0,
            truncated: false,
        }
    }

    /// Add output, dropping the oldest bytes if that exceeds the cap.
    pub fn push(&mut self, bytes: &[u8]) {
        if self.cap == 0 {
            self.lost = self.lost.saturating_add(bytes.len() as u64);
            return;
        }

        let dropped = if bytes.len() >= self.cap {
            // A push at least as large as the whole cap: only its tail can be
            // kept, and everything already held is gone. Handling this first
            // keeps the drain below from walking megabytes.
            let head = bytes.len().saturating_sub(self.cap);
            let dropped = self.buf.len().saturating_add(head);
            self.buf.clear();
            self.buf.extend(bytes.get(head..).unwrap_or(&[]));
            dropped
        } else {
            let over = self
                .buf
                .len()
                .saturating_add(bytes.len())
                .saturating_sub(self.cap);
            self.buf.drain(..over.min(self.buf.len()));
            self.buf.extend(bytes);
            over
        };

        self.lost = self.lost.saturating_add(dropped as u64);
        if dropped > 0 {
            self.truncated = true;
        }

        // Only while the front might be an orphan.
        //
        // Terminal output is not always text — a `cat` of a binary file is a
        // legitimate thing to do — and a chunk of binary data can begin with a
        // byte that happens to look like a UTF-8 continuation. Resynchronising
        // unconditionally would silently eat those bytes from output that was
        // never truncated at all.
        if self.truncated {
            self.resync();
        }
    }

    /// Advance past an orphaned partial UTF-8 sequence at the front.
    ///
    /// Called after the new bytes are in place, so the front is whatever the
    /// reader will actually see next rather than an intermediate state.
    ///
    /// Dropping at an arbitrary byte can cut a multi-byte character in half.
    /// The leftover continuation bytes render as a replacement character that
    /// never resolves, and worse, shift everything after them if the reader is
    /// counting characters. Skipping them costs at most three bytes — which
    /// are already part of a character that has been lost regardless.
    fn resync(&mut self) {
        // A continuation byte is 0b10xxxxxx. A start byte never is, so this
        // stops at the first whole character.
        let mut skipped = 0u64;
        while let Some(&b) = self.buf.front() {
            if b & 0b1100_0000 == 0b1000_0000 {
                let _ = self.buf.pop_front();
                skipped = skipped.saturating_add(1);
            } else {
                break;
            }
        }
        self.lost = self.lost.saturating_add(skipped);

        // A start byte at the front means the orphan is behind us. An empty
        // buffer does not: the rest of the broken character may still be on
        // its way, so stay armed.
        if !self.buf.is_empty() {
            self.truncated = false;
        }
    }

    /// Take everything held, and the number of bytes lost so far.
    ///
    /// Leaves the backlog empty with its loss counter reset: the count belongs
    /// to the handover it is reported in, and reporting it twice would tell a
    /// user about a gap they were already told about.
    pub fn take(&mut self) -> (Vec<u8>, u64) {
        let lost = self.lost;
        self.lost = 0;
        let bytes: Vec<u8> = self.buf.drain(..).collect();
        (bytes, lost)
    }

    /// Bytes currently held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// Whether nothing is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Bytes dropped since the last [`Backlog::take`].
    #[must_use]
    pub const fn lost(&self) -> u64 {
        self.lost
    }
}

/// Aborts a control sequence the receiving terminal may still be parsing.
///
/// `CAN` (0x18) tells a terminal to abandon the escape sequence in progress;
/// `ST` (ESC \) closes an operating-system command, which `CAN` does not.
/// Sent at the start of every reattach, because the link can drop in the
/// middle of any sequence — and if it dropped inside an OSC, the terminal will
/// swallow everything printed after it until a terminator arrives, so the
/// first thing a reattaching user sees would be nothing at all.
pub const RESYNC: &[u8] = b"\x18\x1b\\";

/// A one-line notice telling the user output is missing, and how much.
///
/// Deliberately plain text with no colour: this is written into whatever the
/// user's terminal is, and a session that reattaches into a pager or a
/// full-screen editor should not have escape codes injected into it beyond the
/// resync that is already unavoidable.
#[must_use]
pub fn loss_notice(lost: u64) -> Vec<u8> {
    format!("\r\n[ownview: {lost} bytes of output were lost while disconnected]\r\n").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(b: &Backlog) -> Vec<u8> {
        b.buf.iter().copied().collect()
    }

    #[test]
    fn output_under_the_cap_is_kept_whole() {
        let mut b = Backlog::new(64);
        b.push(b"hello ");
        b.push(b"world");
        assert_eq!(b.lost(), 0);
        let (bytes, lost) = b.take();
        assert_eq!(bytes, b"hello world");
        assert_eq!(lost, 0);
    }

    #[test]
    fn taking_twice_does_not_repeat_the_output() {
        let mut b = Backlog::new(64);
        b.push(b"once");
        assert_eq!(b.take().0, b"once");
        assert_eq!(b.take(), (Vec::new(), 0), "a second take has nothing left");
    }

    /// The loss count belongs to the handover that reports it. Reporting it
    /// again would tell a user twice about one gap.
    #[test]
    fn the_loss_count_resets_when_it_is_reported() {
        let mut b = Backlog::new(4);
        b.push(b"12345678");
        let (_, lost) = b.take();
        assert!(lost > 0);
        assert_eq!(b.lost(), 0);
        b.push(b"ok");
        assert_eq!(b.take(), (b"ok".to_vec(), 0));
    }

    #[test]
    fn the_oldest_bytes_go_first() {
        let mut b = Backlog::new(8);
        b.push(b"AAAABBBB");
        b.push(b"CCCC");
        let (bytes, lost) = b.take();
        assert_eq!(bytes, b"BBBBCCCC", "the tail is what a user wants to see");
        assert_eq!(lost, 4);
    }

    /// The invariant that makes this safe to run for hours.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "a randomised sweep: MIRI interprets rather than executes, so this costs minutes. The property it checks is fuzzed natively at hundreds of thousands of runs a minute, which is a better home for it; what MIRI is here for is undefined behavior, and the small deterministic tests exercise the same code paths"
    )]
    fn the_cap_is_never_exceeded_however_the_output_arrives() {
        for chunk in [1usize, 3, 7, 64, 1000, 4096] {
            let mut b = Backlog::new(1024);
            let data = vec![b'x'; chunk];
            for _ in 0..200 {
                b.push(&data);
                assert!(
                    b.len() <= 1024,
                    "cap exceeded with {chunk}-byte pushes: {}",
                    b.len()
                );
            }
        }
    }

    /// A single push bigger than the whole cap keeps its tail and nothing else.
    #[test]
    fn a_push_larger_than_the_cap_keeps_only_its_tail() {
        let mut b = Backlog::new(4);
        b.push(b"old");
        b.push(b"0123456789");
        let (bytes, lost) = b.take();
        assert_eq!(bytes, b"6789");
        assert_eq!(lost, 3 + 6, "the held bytes and the head of the push");
    }

    /// Exactly at the cap is not over it.
    #[test]
    fn a_push_exactly_the_size_of_the_cap_loses_only_what_was_held() {
        let mut b = Backlog::new(4);
        b.push(b"ab");
        b.push(b"WXYZ");
        assert_eq!(b.take(), (b"WXYZ".to_vec(), 2));
    }

    /// A cap of zero keeps the shell alive and the output not.
    #[test]
    fn a_zero_cap_keeps_nothing_and_counts_everything() {
        let mut b = Backlog::new(0);
        b.push(b"discarded");
        assert!(b.is_empty());
        assert_eq!(b.take(), (Vec::new(), 9));
    }

    /// The reason `resync` exists. Cutting a multi-byte character in half
    /// leaves continuation bytes that render as a replacement character
    /// forever; the fix is to drop the orphaned remainder too.
    #[test]
    fn a_split_utf8_character_does_not_leave_orphaned_continuation_bytes() {
        // "é" is 0xC3 0xA9; "€" is 0xE2 0x82 0xAC.
        let mut b = Backlog::new(4);
        b.push("aé€".as_bytes()); // 1 + 2 + 3 = 6 bytes, cap 4
        let (bytes, lost) = b.take();
        assert!(
            std::str::from_utf8(&bytes).is_ok(),
            "what is kept must be valid UTF-8, got {bytes:02x?}"
        );
        assert_eq!(bytes, "€".as_bytes(), "the whole euro sign survives");
        assert_eq!(lost, 3, "the 'a' and both bytes of the 'é'");
    }

    /// Whatever the cut point, the front is never garbage — this is the
    /// property, not the one example above.
    ///
    /// The tail is a different matter and is deliberately *not* required to be
    /// complete: a pseudo-terminal read can end in the middle of a character
    /// at any time, and the bytes that finish it arrive in the next read. An
    /// incomplete tail is normal and resolves itself; an orphaned front never
    /// does, because the bytes that would have completed it are gone.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "a randomised sweep: MIRI interprets rather than executes, so this costs minutes. The property it checks is fuzzed natively at hundreds of thousands of runs a minute, which is a better home for it; what MIRI is here for is undefined behavior, and the small deterministic tests exercise the same code paths"
    )]
    fn whatever_the_cut_point_the_front_is_never_orphaned() {
        let text = "aé€𐍈bcdé€𐍈".repeat(8);
        for cap in 1..=48 {
            let mut b = Backlog::new(cap);
            // A byte at a time, so the cut lands everywhere.
            for byte in text.as_bytes() {
                b.push(&[*byte]);
                let bytes = held(&b);
                if let Err(e) = std::str::from_utf8(&bytes) {
                    assert!(
                        // `None` means "ran out of input", i.e. a valid prefix
                        // of a character. Anything else is real corruption.
                        e.error_len().is_none(),
                        "cap {cap} left corrupt bytes, not merely an unfinished \
                         character: {bytes:02x?} ({e})"
                    );
                    assert!(
                        bytes.len().saturating_sub(e.valid_up_to()) <= 3,
                        "an unfinished character can only be the last 3 bytes, \
                         but {} are unaccounted for in {bytes:02x?}",
                        bytes.len() - e.valid_up_to()
                    );
                }
            }
        }
    }

    /// The bug the `truncated` flag exists for.
    ///
    /// With a small cap the push that drops a character's start byte can
    /// return before its continuation bytes arrive. A resynchronise that only
    /// looked at the truncating push would let them straight through.
    #[test]
    fn continuation_bytes_arriving_after_the_truncating_push_are_still_dropped() {
        let mut b = Backlog::new(1);
        for byte in "€".as_bytes() {
            b.push(&[*byte]); // e2, then 82, then ac
        }
        let (bytes, lost) = b.take();
        assert!(
            bytes.is_empty(),
            "every byte of the split character should be gone, kept {bytes:02x?}"
        );
        assert_eq!(lost, 3);
    }

    /// And the flag must disarm, or the first real character after a
    /// truncation would be eaten as well.
    #[test]
    fn a_start_byte_at_the_front_disarms_the_resynchronise() {
        let mut b = Backlog::new(4);
        b.push("aé".as_bytes());
        b.push("€".as_bytes()); // truncates
        b.push("xyz".as_bytes());
        let (bytes, _) = b.take();
        assert_eq!(
            std::str::from_utf8(&bytes).expect("valid"),
            "xyz",
            "ordinary text after a truncation must survive"
        );
    }

    /// Binary output that was never truncated must come back byte for byte,
    /// even when it begins with a byte that looks like a UTF-8 continuation.
    /// An unconditional resynchronise would eat it.
    #[test]
    fn untruncated_binary_beginning_with_a_continuation_byte_is_untouched() {
        let data = [0x80u8, 0x81, 0xBF, 0x00, 0xFF];
        let mut b = Backlog::new(64);
        b.push(&data);
        assert_eq!(b.take(), (data.to_vec(), 0));
    }

    /// Losses accumulate rather than being overwritten by the latest one.
    #[test]
    fn losses_accumulate_across_pushes() {
        let mut b = Backlog::new(4);
        b.push(b"AAAA");
        b.push(b"BB"); // loses 2
        b.push(b"CC"); // loses 2 more
        assert_eq!(b.lost(), 4);
    }

    /// Binary output — a `cat` of a JPEG — must not be corrupted beyond the
    /// documented front truncation. In particular the resynchronise must not
    /// eat bytes from the middle.
    #[test]
    fn binary_output_is_kept_byte_for_byte_apart_from_the_front() {
        let mut b = Backlog::new(256);
        let data: Vec<u8> = (0u8..=255).collect();
        b.push(&data);
        let (bytes, lost) = b.take();
        assert_eq!(lost, 0);
        assert_eq!(bytes, data, "no cap pressure means no change at all");
    }

    /// Arbitrary pushes must never panic and must always respect the cap.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "a randomised sweep: MIRI interprets rather than executes, so this costs minutes. The property it checks is fuzzed natively at hundreds of thousands of runs a minute, which is a better home for it; what MIRI is here for is undefined behavior, and the small deterministic tests exercise the same code paths"
    )]
    fn arbitrary_pushes_never_panic_and_never_exceed_the_cap() {
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        for cap in [0usize, 1, 2, 5, 97, 1024] {
            let mut b = Backlog::new(cap);
            for _ in 0..2000 {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                let n = (x % 300) as usize;
                let chunk: Vec<u8> = (0..n).map(|i| ((x >> (i % 8)) & 0xFF) as u8).collect();
                b.push(&chunk);
                assert!(b.len() <= cap, "cap {cap} exceeded: {}", b.len());
            }
        }
    }

    #[test]
    fn the_resync_prefix_aborts_both_a_control_sequence_and_an_os_command() {
        assert_eq!(RESYNC, b"\x18\x1b\\");
    }

    /// The notice must be readable on a terminal in raw mode, where a bare
    /// newline moves down without returning to column zero — so it needs the
    /// carriage return, or it prints as a staircase.
    #[test]
    fn the_loss_notice_says_how_much_and_uses_crlf() {
        let n = loss_notice(4096);
        let s = String::from_utf8(n).expect("utf8");
        assert!(s.contains("4096"), "{s:?}");
        assert!(s.starts_with("\r\n") && s.ends_with("\r\n"), "{s:?}");
        assert!(s.contains("lost while disconnected"), "{s:?}");
    }
}
