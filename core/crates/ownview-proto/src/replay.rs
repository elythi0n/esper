//! Replay protection for media datagrams.
//!
//! # Why datagrams need their own window
//!
//! QUIC protects its own packets against replay: each carries a packet number
//! and the AEAD nonce is derived from it, so a replayed QUIC packet fails
//! authentication. That is not the whole story for OwnView.
//!
//! A media datagram carries a *sequence number of its own*, because frames
//! arrive out of order and late ones must be discarded rather than displayed.
//! That sequence space is application-level, and the decision "is this frame
//! too old to use" is the same shape as "have I seen this frame before". One
//! window answers both:
//!
//! - **A duplicate** wastes a decode and, for a keyframe, can reset the decoder.
//! - **A stale frame** displayed after a newer one is visible corruption — the
//!   screen jumps backwards.
//!
//! # Why a bitmap and not a high-water mark
//!
//! A high-water mark ("accept anything above the highest seen") rejects every
//! legitimately reordered frame, and reordering is normal on any real network.
//! A bitmap of the recent past accepts out-of-order arrivals within the window
//! while still rejecting duplicates — the same construction IPsec and
//! WireGuard use, for the same reason.

use core::fmt;

/// How many sequence numbers behind the newest are tracked.
///
/// 1024 at 60 frames per second is about seventeen seconds of history, which is
/// far more reordering than any usable link produces. The cost is 128 bytes.
///
/// Sized generously on purpose: the failure mode of a window that is too small
/// is dropping legitimate frames on a jittery link, which looks like packet
/// loss and is very hard to diagnose from the outside.
pub const WINDOW: u64 = 1024;

/// Number of `u64` words in the bitmap.
const WORDS: usize = (WINDOW as usize) / 64;

/// Tracks which recent sequence numbers have been seen.
///
/// One per direction per session. Not `Sync`: a single reader owns the receive
/// path, and sharing it would require a lock in the per-frame path for no
/// benefit.
pub struct ReplayWindow {
    /// Highest sequence number accepted so far.
    newest: u64,
    /// Bitmap of the [`WINDOW`] numbers at or below `newest`.
    ///
    /// Bit 0 of word 0 is `newest`; bit `n` is `newest - n`.
    seen: [u64; WORDS],
    /// Whether anything has been accepted yet.
    started: bool,
    /// Counters, for the session HUD and the local log. A rising duplicate
    /// count on a healthy link is worth seeing.
    accepted: u64,
    duplicates: u64,
    too_old: u64,
}

impl fmt::Debug for ReplayWindow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReplayWindow")
            .field("newest", &self.newest)
            .field("accepted", &self.accepted)
            .field("duplicates", &self.duplicates)
            .field("too_old", &self.too_old)
            .finish()
    }
}

/// Why a datagram was not accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejected {
    /// Already seen. A duplicate, or a replay.
    Duplicate,
    /// Older than the window. Cannot be checked for duplication, so it is
    /// refused: accepting it would mean a replay of an old frame could reset
    /// the decoder or push the screen backwards.
    TooOld,
}

impl Default for ReplayWindow {
    fn default() -> Self {
        Self::new()
    }
}

impl ReplayWindow {
    /// An empty window.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            newest: 0,
            seen: [0; WORDS],
            started: false,
            accepted: 0,
            duplicates: 0,
            too_old: 0,
        }
    }

    /// Check and record a sequence number.
    ///
    /// Returns `Ok(())` if this is the first time it has been seen and it is
    /// within the window. **The number is recorded only when accepted**, so a
    /// rejected datagram cannot advance the window and squeeze out legitimate
    /// frames behind it.
    pub fn check(&mut self, seq: u64) -> Result<(), Rejected> {
        if !self.started {
            self.started = true;
            self.newest = seq;
            self.set(0);
            self.accepted = self.accepted.saturating_add(1);
            return Ok(());
        }

        if seq > self.newest {
            // A newer frame. Slide the window forward, clearing the bits that
            // fall off the front.
            let advance = seq - self.newest;
            self.slide(advance);
            self.newest = seq;
            self.set(0);
            self.accepted = self.accepted.saturating_add(1);
            return Ok(());
        }

        let behind = self.newest - seq;
        if behind >= WINDOW {
            self.too_old = self.too_old.saturating_add(1);
            return Err(Rejected::TooOld);
        }
        if self.get(behind) {
            self.duplicates = self.duplicates.saturating_add(1);
            return Err(Rejected::Duplicate);
        }
        self.set(behind);
        self.accepted = self.accepted.saturating_add(1);
        Ok(())
    }

    /// Highest sequence number accepted, or `None` before the first datagram.
    #[must_use]
    pub const fn newest(&self) -> Option<u64> {
        if self.started {
            Some(self.newest)
        } else {
            None
        }
    }

    /// How many datagrams were accepted.
    #[must_use]
    pub const fn accepted(&self) -> u64 {
        self.accepted
    }

    /// How many were rejected as duplicates.
    #[must_use]
    pub const fn duplicates(&self) -> u64 {
        self.duplicates
    }

    /// How many were rejected as older than the window.
    #[must_use]
    pub const fn too_old(&self) -> u64 {
        self.too_old
    }

    /// Shift the bitmap forward by `n` positions.
    fn slide(&mut self, n: u64) {
        if n >= WINDOW {
            // Everything in the window is now out of it.
            self.seen = [0; WORDS];
            return;
        }
        let words = (n / 64) as usize;
        let bits = (n % 64) as u32;

        if words > 0 {
            // Move whole words toward higher indices (further into the past).
            for i in (0..WORDS).rev() {
                let src = i.checked_sub(words);
                let v = src.and_then(|s| self.seen.get(s).copied()).unwrap_or(0);
                if let Some(slot) = self.seen.get_mut(i) {
                    *slot = v;
                }
            }
        }
        if bits > 0 {
            let mut carry = 0u64;
            for i in 0..WORDS {
                let Some(slot) = self.seen.get_mut(i) else {
                    break;
                };
                let v = *slot;
                *slot = (v << bits) | carry;
                carry = v >> (64 - bits);
            }
        }
    }

    /// Whether the bit `n` positions behind `newest` is set.
    fn get(&self, n: u64) -> bool {
        let (w, b) = ((n / 64) as usize, (n % 64) as u32);
        self.seen.get(w).is_some_and(|v| v & (1u64 << b) != 0)
    }

    /// Set the bit `n` positions behind `newest`.
    fn set(&mut self, n: u64) {
        let (w, b) = ((n / 64) as usize, (n % 64) as u32);
        if let Some(slot) = self.seen.get_mut(w) {
            *slot |= 1u64 << b;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_window_accepts_anything_once() {
        for first in [0u64, 1, 42, WINDOW, u64::MAX / 2] {
            let mut w = ReplayWindow::new();
            assert_eq!(w.check(first), Ok(()));
            assert_eq!(w.newest(), Some(first));
            assert_eq!(w.accepted(), 1);
        }
    }

    #[test]
    fn a_new_window_reports_nothing_seen() {
        let w = ReplayWindow::new();
        assert_eq!(w.newest(), None);
        assert_eq!(w.accepted(), 0);
    }

    #[test]
    fn an_increasing_sequence_is_all_accepted() {
        let mut w = ReplayWindow::new();
        for seq in 0..5000u64 {
            assert_eq!(w.check(seq), Ok(()), "seq {seq} should be accepted");
        }
        assert_eq!(w.accepted(), 5000);
        assert_eq!(w.duplicates(), 0);
        assert_eq!(w.newest(), Some(4999));
    }

    #[test]
    fn an_immediate_repeat_is_a_duplicate() {
        let mut w = ReplayWindow::new();
        assert_eq!(w.check(10), Ok(()));
        assert_eq!(w.check(10), Err(Rejected::Duplicate));
        assert_eq!(w.duplicates(), 1);
    }

    /// The whole point of a bitmap rather than a high-water mark: reordering is
    /// normal on any real network, and a window that rejected it would look
    /// like packet loss.
    #[test]
    fn reordered_frames_within_the_window_are_accepted() {
        let mut w = ReplayWindow::new();
        for seq in [100u64, 103, 101, 105, 102, 104] {
            assert_eq!(
                w.check(seq),
                Ok(()),
                "reordered seq {seq} should be accepted"
            );
        }
        assert_eq!(w.accepted(), 6);
        assert_eq!(w.duplicates(), 0);
        assert_eq!(w.newest(), Some(105));
    }

    #[test]
    fn a_duplicate_among_reordered_frames_is_still_caught() {
        let mut w = ReplayWindow::new();
        for seq in [100u64, 103, 101] {
            assert_eq!(w.check(seq), Ok(()));
        }
        assert_eq!(w.check(101), Err(Rejected::Duplicate));
        assert_eq!(w.check(103), Err(Rejected::Duplicate));
        assert_eq!(w.check(100), Err(Rejected::Duplicate));
    }

    /// Anything older than the window cannot be checked for duplication, so it
    /// is refused rather than trusted.
    #[test]
    fn a_frame_older_than_the_window_is_refused() {
        let mut w = ReplayWindow::new();
        assert_eq!(w.check(WINDOW * 2), Ok(()));
        assert_eq!(w.check(WINDOW * 2 - WINDOW), Err(Rejected::TooOld));
        assert_eq!(w.check(0), Err(Rejected::TooOld));
        assert_eq!(w.too_old(), 2);
    }

    /// The boundary. The oldest number still inside the window is accepted;
    /// one further back is not.
    #[test]
    fn the_window_boundary_is_exact() {
        let base = WINDOW * 4;
        let mut w = ReplayWindow::new();
        assert_eq!(w.check(base), Ok(()));

        // `base - (WINDOW - 1)` is the oldest still tracked.
        assert_eq!(w.check(base - (WINDOW - 1)), Ok(()), "oldest in window");

        let mut w2 = ReplayWindow::new();
        assert_eq!(w2.check(base), Ok(()));
        assert_eq!(
            w2.check(base - WINDOW),
            Err(Rejected::TooOld),
            "exactly one past the window"
        );
    }

    /// A rejected datagram must not advance the window, or an attacker could
    /// replay one old frame and push out every legitimate frame behind it.
    #[test]
    fn a_rejected_datagram_does_not_move_the_window() {
        // The base must be at least WINDOW above the stale number being tested,
        // or "stale" is simply inside the window and legitimately accepted —
        // which is what the first version of this test got wrong.
        let base = WINDOW * 4;
        let mut w = ReplayWindow::new();
        assert_eq!(w.check(base), Ok(()));
        assert_eq!(w.check(base - 1), Ok(()));

        assert_eq!(w.check(base - 1), Err(Rejected::Duplicate));
        assert_eq!(w.newest(), Some(base), "a duplicate must not move `newest`");

        assert_eq!(w.check(base - WINDOW), Err(Rejected::TooOld));
        assert_eq!(
            w.newest(),
            Some(base),
            "a stale frame must not move `newest`"
        );

        // And the frames just behind are still acceptable.
        assert_eq!(w.check(base - 2), Ok(()));
    }

    /// A large jump forward — a long stall, or a hostile peer sending a huge
    /// sequence number — must clear the window rather than leave stale bits
    /// that would reject legitimate frames afterwards.
    #[test]
    fn a_large_jump_clears_the_window_cleanly() {
        let mut w = ReplayWindow::new();
        for seq in 0..100u64 {
            assert_eq!(w.check(seq), Ok(()));
        }
        // Jump far beyond the window.
        assert_eq!(w.check(1_000_000), Ok(()));
        // Everything in the old window is now too old.
        assert_eq!(w.check(50), Err(Rejected::TooOld));
        // And the new neighbourhood is clean.
        for seq in 999_000..999_010u64 {
            assert_eq!(w.check(seq), Ok(()), "seq {seq} after a jump");
        }
    }

    /// The exact-window-size jump is the one a shift implementation gets wrong.
    #[test]
    fn a_jump_of_exactly_the_window_size_clears_everything() {
        let mut w = ReplayWindow::new();
        assert_eq!(w.check(0), Ok(()));
        assert_eq!(w.check(WINDOW), Ok(()));
        assert_eq!(w.check(0), Err(Rejected::TooOld));
        // The rest of the new window must be free.
        for seq in 1..WINDOW {
            assert_eq!(
                w.check(seq),
                Ok(()),
                "seq {seq} should be free after the slide"
            );
        }
    }

    /// Word-boundary shifts are where an off-by-one lives.
    #[test]
    fn shifts_across_word_boundaries_preserve_history() {
        for advance in [1u64, 63, 64, 65, 127, 128, 129, 511, 512, 513] {
            let mut w = ReplayWindow::new();
            let base = WINDOW * 8;
            assert_eq!(w.check(base), Ok(()));
            assert_eq!(w.check(base + advance), Ok(()));
            assert_eq!(
                w.check(base),
                Err(Rejected::Duplicate),
                "after advancing {advance}, the original must still be remembered"
            );
        }
    }

    /// Every position in the window must be independently tracked. If a shift
    /// dropped or smeared bits, this would report a duplicate or accept one.
    #[test]
    fn every_position_in_the_window_is_tracked_independently() {
        let base = WINDOW * 16;
        let mut w = ReplayWindow::new();
        assert_eq!(w.check(base), Ok(()));

        // Fill the whole window, newest-first.
        for behind in 1..WINDOW {
            assert_eq!(
                w.check(base - behind),
                Ok(()),
                "position {behind} behind should be free"
            );
        }
        // Now every one of them is a duplicate.
        for behind in 0..WINDOW {
            assert_eq!(
                w.check(base - behind),
                Err(Rejected::Duplicate),
                "position {behind} behind should now be remembered"
            );
        }
        assert_eq!(w.accepted(), WINDOW);
        assert_eq!(w.duplicates(), WINDOW);
    }

    /// A sequence number near the top of the range must not overflow.
    #[test]
    fn sequence_numbers_near_the_maximum_do_not_overflow() {
        let mut w = ReplayWindow::new();
        assert_eq!(w.check(u64::MAX - 1), Ok(()));
        assert_eq!(w.check(u64::MAX), Ok(()));
        assert_eq!(w.check(u64::MAX), Err(Rejected::Duplicate));
        assert_eq!(w.check(u64::MAX - 1), Err(Rejected::Duplicate));
        assert_eq!(w.check(0), Err(Rejected::TooOld));
    }

    /// The counters are shown in the session HUD, so they must actually count.
    #[test]
    fn the_counters_add_up() {
        let base = WINDOW * 4;
        let mut w = ReplayWindow::new();
        let _ = w.check(base);
        let _ = w.check(base + 1);
        let _ = w.check(base + 1); // duplicate
        let _ = w.check(base + 1 - WINDOW); // too old
        let _ = w.check(base + 2);
        assert_eq!(w.accepted(), 3);
        assert_eq!(w.duplicates(), 1);
        assert_eq!(w.too_old(), 1);
    }

    /// Arbitrary input must never panic — a peer chooses these numbers.
    #[test]
    fn arbitrary_sequences_never_panic() {
        let mut w = ReplayWindow::new();
        let mut x = 0x2545_F491_4F6C_DD1Du64;
        for _ in 0..20_000 {
            // xorshift, so the sequence is wild but reproducible.
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let _ = w.check(x);
        }
    }

    /// Debug must not be so large it is useless in a log.
    #[test]
    fn debug_is_a_summary_not_a_bitmap_dump() {
        let mut w = ReplayWindow::new();
        let _ = w.check(5);
        let rendered = format!("{w:?}");
        assert!(rendered.len() < 200, "Debug is {} chars", rendered.len());
        assert!(rendered.contains("newest"));
        assert!(rendered.contains("duplicates"));
    }
}
