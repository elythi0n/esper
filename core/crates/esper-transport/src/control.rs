//! Framed control streams.
//!
//! A control stream is long-lived and carries many messages, so it cannot be
//! read with `read_to_end`: that waits for the sender to *finish* the stream,
//! which a stream still in use never does. Frames are read one at a time —
//! length prefix, then exactly that many bytes.
//!
//! The length is validated **before** any of the payload is kept. A peer
//! claiming a four-gigabyte frame must cost a rejected header, not four
//! gigabytes, and at this layer the peer may not yet be authenticated.
//!
//! # Reading must survive being cancelled
//!
//! [`Control::recv`] is called inside `tokio::select!` — that is how a terminal
//! session watches its pseudo-terminal and its peer at the same time — so its
//! future is dropped whenever another branch wins the race. Every byte read so
//! far therefore has to live somewhere that outlives the future.
//!
//! `quinn`'s own `read` is cancel-safe: dropping it loses nothing. A **loop**
//! over it is not. Bytes that a completed inner read already took are gone
//! along with the loop's locals, and the next call starts in the middle of a
//! frame — reading four payload bytes as a length. From there every frame is
//! garbage, and the failure appears as "malformed frame" a long way from its
//! cause.
//!
//! So the partially-read frame lives in [`FrameReader`], owned by the
//! `Control`. Cancelling `recv` at any point is then indistinguishable from
//! not having been called yet.

use esper_proto::{FrameError, MAX_FRAME_LEN};
use quinn::{RecvStream, SendStream};

/// Size of the big-endian `u32` length prefix.
const LEN_PREFIX: usize = 4;

/// How much is read from the stream at once.
///
/// Matches the largest terminal chunk, so the common case — one screenful or
/// one burst of output — is a single read rather than several.
const READ_CHUNK: usize = 64 * 1024;

/// Accumulates bytes until a whole frame has arrived.
///
/// Exists so that the state of a half-read frame outlives the future reading
/// it. See the module documentation: `recv` is routinely cancelled, and a
/// reader that kept its progress in a local would desynchronise the stream.
///
/// Public because it is the first thing attacker-controlled bytes reach on a
/// control stream, and a parser at that boundary is fuzzed. The fuzz target
/// asserts the invariant the unit tests check for fixed inputs: **how the
/// bytes were split must not change the frames that come out.**
#[derive(Debug, Default)]
pub struct FrameReader {
    /// Everything received and not yet handed out as a frame. Bounded by
    /// [`MAX_FRAME_LEN`] plus one read, because a length over the cap is
    /// rejected as soon as the header is complete.
    buf: Vec<u8>,
}

impl FrameReader {
    /// An empty reader, between frames.
    #[must_use]
    pub const fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// Add freshly-read bytes.
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// One frame's payload, if a whole one is here.
    ///
    /// The length is checked as soon as the header is complete, so a peer
    /// claiming a huge frame is rejected before any of it is accumulated.
    pub fn take(&mut self) -> Result<Option<Vec<u8>>, ControlError> {
        let Some(header) = self.buf.get(..LEN_PREFIX) else {
            return Ok(None);
        };
        let len = u32::from_be_bytes(
            <[u8; LEN_PREFIX]>::try_from(header).map_err(|_| ControlError::Empty)?,
        );

        if len == 0 {
            return Err(ControlError::Empty);
        }
        if len > MAX_FRAME_LEN {
            return Err(ControlError::TooLarge { len });
        }

        let end = LEN_PREFIX.saturating_add(len as usize);
        if self.buf.len() < end {
            return Ok(None);
        }

        let payload = self.buf.get(LEN_PREFIX..end).unwrap_or(&[]).to_vec();
        let _ = self.buf.drain(..end);
        Ok(Some(payload))
    }

    /// Everything read and not yet handed out as a frame.
    ///
    /// For a stream that stops being framed part-way through — a forwarded
    /// connection, where the request is a frame and everything after it is the
    /// connection's own bytes. A read large enough to have taken some of those
    /// bytes along with the frame is the normal case, not a rare one, and
    /// dropping them would silently truncate the payload.
    #[must_use]
    pub fn into_remaining(self) -> Vec<u8> {
        self.buf
    }

    /// How many bytes are held for a frame not yet complete.
    #[must_use]
    pub fn held(&self) -> usize {
        self.buf.len()
    }

    /// If a frame is part-way through, how much was wanted and how much came.
    ///
    /// `None` means the reader is between frames, where the peer finishing the
    /// stream is a clean close rather than a truncated frame.
    #[must_use]
    pub fn shortfall(&self) -> Option<(usize, usize)> {
        if self.buf.is_empty() {
            return None;
        }
        let wanted = match self.buf.get(..LEN_PREFIX) {
            Some(h) => <[u8; LEN_PREFIX]>::try_from(h)
                .map(|b| LEN_PREFIX.saturating_add(u32::from_be_bytes(b) as usize))
                // Unreachable: a slice of LEN_PREFIX bytes always converts.
                .unwrap_or(LEN_PREFIX),
            // Not even a whole header.
            None => LEN_PREFIX,
        };
        Some((wanted, self.buf.len()))
    }
}

/// A bidirectional control stream carrying length-prefixed CBOR frames.
pub struct Control {
    send: SendStream,
    recv: RecvStream,
    frames: FrameReader,
    /// Where a read lands before it is committed to the reader.
    ///
    /// Separate from the reader's own buffer on purpose: a cancelled read must
    /// leave no trace, and reading straight into the reader would mean either
    /// appending bytes that were never received or resizing a buffer that the
    /// next call will misinterpret.
    scratch: Vec<u8>,
}

impl std::fmt::Debug for Control {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Control")
            .field("send_id", &self.send.id())
            .field("recv_id", &self.recv.id())
            .finish()
    }
}

/// What can go wrong on a control stream.
#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    /// The declared frame length exceeds [`MAX_FRAME_LEN`]. Rejected before
    /// anything is allocated.
    #[error("frame length {len} exceeds maximum {MAX_FRAME_LEN}")]
    TooLarge { len: u32 },

    /// A zero-length frame carries no CBOR and is always a protocol error.
    #[error("frame length is zero")]
    Empty,

    /// The peer finished or reset the stream mid-frame.
    #[error("stream ended mid-frame: wanted {wanted} bytes, got {got}")]
    Truncated { wanted: usize, got: usize },

    /// The stream was closed cleanly with no frame in progress. Not an error in
    /// itself — it is how a peer says it is done — but the caller must decide.
    #[error("stream finished")]
    Finished,

    /// Framing or CBOR failure.
    #[error(transparent)]
    Frame(#[from] FrameError),

    /// Transport failure.
    #[error("stream: {0}")]
    Stream(String),
}

impl ControlError {
    /// The wire reason code to close with.
    #[must_use]
    pub const fn reason(&self) -> esper_proto::Reason {
        match self {
            Self::TooLarge { .. } => esper_proto::Reason::FRAME_TOO_LARGE,
            Self::Empty | Self::Truncated { .. } | Self::Frame(_) => {
                esper_proto::Reason::MALFORMED_FRAME
            }
            Self::Finished | Self::Stream(_) => esper_proto::Reason::ALL_TIERS_EXHAUSTED,
        }
    }

    /// Whether this is the peer closing cleanly rather than misbehaving.
    #[must_use]
    pub const fn is_clean_close(&self) -> bool {
        matches!(self, Self::Finished)
    }
}

impl Control {
    /// Wrap a bidirectional stream pair.
    #[must_use]
    pub fn new(send: SendStream, recv: RecvStream) -> Self {
        Self {
            send,
            recv,
            frames: FrameReader::new(),
            scratch: vec![0u8; READ_CHUNK],
        }
    }

    /// Encode and send one message.
    pub async fn send<T: serde::Serialize>(&mut self, value: &T) -> Result<(), ControlError> {
        // Encode the payload, then write the length and the payload. Using
        // `write_frame` into a Vec would do the same thing with an extra copy,
        // and would hide which layer owns the framing.
        let payload = esper_proto::encode(value)?;
        let len =
            u32::try_from(payload.len()).map_err(|_| ControlError::TooLarge { len: u32::MAX })?;

        self.send
            .write_all(&len.to_be_bytes())
            .await
            .map_err(|e| ControlError::Stream(e.to_string()))?;
        self.send
            .write_all(&payload)
            .await
            .map_err(|e| ControlError::Stream(e.to_string()))
    }

    /// Read one message.
    ///
    /// Reads exactly one frame and leaves the stream positioned at the start of
    /// the next, so a long-lived stream can be read in a loop.
    ///
    /// **Cancel-safe.** Dropping this future — which `tokio::select!` does
    /// every time another branch wins — keeps whatever has been read so far,
    /// and the next call carries on from there.
    pub async fn recv<T: serde::de::DeserializeOwned>(&mut self) -> Result<T, ControlError> {
        loop {
            // Whatever arrived earlier may already be enough, including on the
            // first call after a cancellation.
            if let Some(payload) = self.frames.take()? {
                // `decode`, not `read_frame`: the length prefix has already
                // been consumed, so what remains is a bare CBOR body.
                // `read_frame` would read the first four bytes of that body as
                // a length and fail with a nonsense number.
                return esper_proto::decode(&payload).map_err(ControlError::from);
            }

            match self.recv.read(self.scratch.as_mut_slice()).await {
                Ok(Some(0)) | Ok(None) => {
                    return Err(match self.frames.shortfall() {
                        Some((wanted, got)) => ControlError::Truncated { wanted, got },
                        // Nothing in progress: the peer finished between
                        // frames, which is how it says it is done.
                        None => ControlError::Finished,
                    });
                }
                Ok(Some(n)) => {
                    let read = self.scratch.get(..n).unwrap_or(&[]);
                    self.frames.push(read);
                }
                Err(e) => return Err(ControlError::Stream(e.to_string())),
            }
        }
    }

    /// Say how much this stream's data matters against the others.
    ///
    /// # Why a file transfer sets this
    ///
    /// The screen is the product; a file copy is a background errand, and on
    /// one connection they compete. Most of that is already answered for free:
    /// media travels as datagrams, and quinn writes datagram frames into a
    /// packet **before** stream frames. What is left is the session's own
    /// control streams — a keyframe request, a monitor switch — which are
    /// stream data like the transfer is, and must not queue behind a gigabyte.
    ///
    /// Higher goes first. Nothing else in this product sets one, so everything
    /// else is at zero and a transfer asks for less than that.
    ///
    /// # Errors
    ///
    /// If the stream is already closed.
    pub fn set_priority(&self, priority: i32) -> Result<(), ControlError> {
        self.send
            .set_priority(priority)
            .map_err(|e| ControlError::Stream(e.to_string()))
    }

    /// Read bytes with no framing around them, on the receiving half only.
    ///
    /// # When one half stops being framed
    ///
    /// A file transfer is framed control at the front and a raw body after it,
    /// like a forward — but unlike a forward, only *one* half of the stream
    /// switches. The receiving end still has an acceptance and an outcome to
    /// send back, and it sends them on a half that is still framed.
    ///
    /// [`Control::into_raw`] cannot express that: it gives up the framing on
    /// both halves at once. This gives it up on this one.
    ///
    /// **Whatever was read past the last frame comes out first.** A reader
    /// that went straight to the stream would lose it — the bytes are already
    /// in this process, and they are the first bytes of the body.
    pub async fn recv_raw(&mut self, into: &mut [u8]) -> Result<usize, ControlError> {
        if self.frames.held() > 0 {
            let held = std::mem::take(&mut self.frames).into_remaining();
            let took = held.len().min(into.len());
            let (taking, keeping) = held.split_at(took);
            if let Some(room) = into.get_mut(..took) {
                room.copy_from_slice(taking);
            }
            // Anything that would not fit goes back, to come out next time.
            self.frames.push(keeping);
            return Ok(took);
        }
        match self.recv.read(into).await {
            Ok(Some(n)) => Ok(n),
            // The far end finished: there is no more body.
            Ok(None) => Ok(0),
            Err(e) => Err(ControlError::Stream(e.to_string())),
        }
    }

    /// Write bytes with no framing around them, on the sending half only.
    ///
    /// The other half of this stream is still framed. See
    /// [`Control::recv_raw`].
    pub async fn send_raw(&mut self, bytes: &[u8]) -> Result<(), ControlError> {
        self.send
            .write_all(bytes)
            .await
            .map_err(|e| ControlError::Stream(e.to_string()))
    }

    /// Give up the framing and hand back the raw stream.
    ///
    /// Returns the two halves and **whatever was read past the last frame**,
    /// which the caller must treat as the first bytes of the raw stream. See
    /// [`FrameReader::into_remaining`]: a stream that stops being framed
    /// cannot simply drop the reader.
    #[must_use]
    pub fn into_raw(self) -> (SendStream, RecvStream, Vec<u8>) {
        (self.send, self.recv, self.frames.into_remaining())
    }

    /// Finish the send side, telling the peer no more frames are coming.
    pub fn finish(&mut self) -> Result<(), ControlError> {
        self.send
            .finish()
            .map_err(|e| ControlError::Stream(e.to_string()))
    }
}

#[cfg(test)]
mod raw_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    /// **The bytes read past the last frame are the first bytes of the body.**
    /// One read off a socket takes whatever is there, so a frame and the
    /// beginning of what follows it arrive together — and a reader that went
    /// straight to the stream afterwards would have thrown them away.
    #[test]
    fn nothing_read_past_the_last_frame_is_lost_when_a_half_stops_being_framed() {
        let mut frames = FrameReader::new();
        // One frame, and some body immediately behind it.
        let mut wire = Vec::new();
        esper_proto::frame::write_frame(&mut wire, &"the frame").expect("encode");
        let body = b"the first bytes of the body";
        wire.extend_from_slice(body);
        frames.push(&wire);

        let payload = frames.take().expect("a frame").expect("a whole frame");
        let got: String = esper_proto::decode(&payload).expect("decode");
        assert_eq!(got, "the frame");

        // What is left is exactly the body, and nothing of the frame.
        assert_eq!(frames.held(), body.len());
        assert_eq!(frames.into_remaining(), body);
    }

    /// And it comes out in pieces if the caller's buffer is small, rather than
    /// the tail being dropped.
    #[test]
    fn a_small_buffer_gets_the_leftover_in_pieces() {
        let mut frames = FrameReader::new();
        frames.push(b"0123456789");
        let mut out = Vec::new();
        // The same arithmetic `recv_raw` does, without a socket to attach.
        while frames.held() > 0 {
            let held = std::mem::take(&mut frames).into_remaining();
            let took = held.len().min(4);
            let (taking, keeping) = held.split_at(took);
            out.extend_from_slice(taking);
            frames.push(keeping);
        }
        assert_eq!(out, b"0123456789");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame as it goes on the wire: four-byte big-endian length, then body.
    fn framed(body: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(
            &u32::try_from(body.len())
                .expect("test bodies are small")
                .to_be_bytes(),
        );
        v.extend_from_slice(body);
        v
    }

    /// Feed `wire` in pieces of `step` bytes, collecting every frame that
    /// comes out.
    ///
    /// This *is* the cancellation test. Every point at which `tokio::select!`
    /// could drop the read future is a point at which the reader has been
    /// given some prefix of the stream and then called again later — which is
    /// exactly what feeding in pieces does.
    fn frames_from(wire: &[u8], step: usize) -> Result<Vec<Vec<u8>>, ControlError> {
        let mut r = FrameReader::new();
        let mut out = Vec::new();
        for chunk in wire.chunks(step.max(1)) {
            r.push(chunk);
            while let Some(f) = r.take()? {
                out.push(f);
            }
        }
        Ok(out)
    }

    #[test]
    fn a_whole_frame_arriving_at_once_comes_straight_back() {
        let wire = framed(b"hello");
        assert_eq!(
            frames_from(&wire, wire.len()).expect("no error"),
            vec![b"hello".to_vec()]
        );
    }

    /// **The bug this exists for.** A frame split anywhere must still decode,
    /// because a cancelled read is a frame split at that point.
    #[test]
    fn a_frame_split_at_any_point_is_reassembled() {
        let wire = framed(b"the quick brown fox jumps over the lazy dog");
        for step in 1..=wire.len() {
            assert_eq!(
                frames_from(&wire, step).expect("no error"),
                vec![b"the quick brown fox jumps over the lazy dog".to_vec()],
                "split into {step}-byte pieces"
            );
        }
    }

    /// Several frames in one read, and frames straddling reads. A busy
    /// terminal produces both.
    #[test]
    fn many_frames_are_split_apart_however_they_arrive() {
        let bodies: Vec<Vec<u8>> = (0u8..12).map(|i| vec![i; (i as usize) * 7 + 1]).collect();
        let mut wire = Vec::new();
        for b in &bodies {
            wire.extend_from_slice(&framed(b));
        }
        for step in [1usize, 2, 3, 5, 17, 64, wire.len(), wire.len() * 2] {
            assert_eq!(
                frames_from(&wire, step).expect("no error"),
                bodies,
                "split into {step}-byte pieces"
            );
        }
    }

    /// An oversized length is rejected as soon as the header is complete —
    /// before any of the claimed payload is accumulated. The peer may not be
    /// authenticated yet, so a claim must not be able to cost us memory.
    #[test]
    fn an_oversized_length_is_rejected_from_the_header_alone() {
        let mut r = FrameReader::new();
        r.push(&(MAX_FRAME_LEN + 1).to_be_bytes());
        match r.take() {
            Err(ControlError::TooLarge { len }) => assert_eq!(len, MAX_FRAME_LEN + 1),
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }

    /// And that rejection must not need the header to arrive in one piece.
    #[test]
    fn an_oversized_length_is_rejected_even_split_byte_by_byte() {
        let mut r = FrameReader::new();
        let header = (MAX_FRAME_LEN + 1).to_be_bytes();
        for (i, b) in header.iter().enumerate() {
            r.push(&[*b]);
            let got = r.take();
            if i + 1 < LEN_PREFIX {
                assert!(
                    matches!(got, Ok(None)),
                    "an incomplete header is not yet a verdict, got {got:?}"
                );
            } else {
                assert!(matches!(got, Err(ControlError::TooLarge { .. })), "{got:?}");
            }
        }
    }

    #[test]
    fn a_zero_length_frame_is_a_protocol_error() {
        let mut r = FrameReader::new();
        r.push(&0u32.to_be_bytes());
        assert!(matches!(r.take(), Err(ControlError::Empty)));
    }

    /// Between frames, nothing is in progress — so a peer finishing there is a
    /// clean close, not a truncation.
    #[test]
    fn nothing_is_in_progress_between_frames() {
        let mut r = FrameReader::new();
        assert_eq!(r.shortfall(), None, "a fresh reader is between frames");

        r.push(&framed(b"one"));
        let _ = r.take().expect("no error").expect("a frame");
        assert_eq!(
            r.shortfall(),
            None,
            "having handed out a whole frame, the reader is between frames again"
        );
    }

    /// Mid-frame, the shortfall says how much was wanted and how much came, so
    /// a truncation reports something a person can act on.
    #[test]
    fn a_part_read_frame_reports_what_is_missing() {
        let mut r = FrameReader::new();
        r.push(&[0, 0, 0, 10, b'a', b'b']);
        assert_eq!(
            r.shortfall(),
            Some((14, 6)),
            "4 header + 10 body, 6 arrived"
        );

        // Even an incomplete header is a frame in progress.
        let mut r = FrameReader::new();
        r.push(&[0, 0]);
        assert_eq!(r.shortfall(), Some((4, 2)));
    }

    /// The buffer must not grow without bound, whatever a peer sends.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "a randomised sweep: MIRI interprets rather than executes, so this costs minutes. The property it checks is fuzzed natively at hundreds of thousands of runs a minute, which is a better home for it; what MIRI is here for is undefined behavior, and the small deterministic tests exercise the same code paths"
    )]
    fn arbitrary_input_never_panics_and_never_hoards() {
        let mut x = 0x243F_6A88_85A3_08D3u64;
        let mut r = FrameReader::new();
        for _ in 0..4000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let n = (x % 200) as usize;
            let chunk: Vec<u8> = (0..n).map(|i| ((x >> (i % 8)) & 0xFF) as u8).collect();
            r.push(&chunk);
            // A rejected frame is fatal to the stream, so a fresh reader is
            // the honest continuation.
            if r.take().is_err() {
                r = FrameReader::new();
            }
            assert!(
                r.held() <= (MAX_FRAME_LEN as usize) + 200,
                "buffer grew to {}",
                r.held()
            );
        }
    }

    /// Bytes that arrived past the last frame survive the switch to raw.
    ///
    /// A forwarded connection stops being framed after the reply, and the read
    /// that took the reply routinely took some payload with it — one packet
    /// holds both. Dropping the reader would silently swallow the first bytes
    /// of the connection, which shows up as a protocol that hangs on its own
    /// greeting rather than as anything that looks like a framing bug.
    #[test]
    fn what_arrived_past_the_last_frame_is_handed_back() {
        let mut r = FrameReader::new();
        let mut wire = framed(b"the reply");
        wire.extend_from_slice(b"and the first payload bytes");
        r.push(&wire);

        assert_eq!(r.take().unwrap().as_deref(), Some(&b"the reply"[..]));
        assert_eq!(r.into_remaining(), b"and the first payload bytes");
    }

    /// And nothing is handed back when nothing was read past the frame.
    #[test]
    fn a_reader_between_frames_has_nothing_left_over() {
        let mut r = FrameReader::new();
        r.push(&framed(b"the reply"));
        assert!(r.take().unwrap().is_some());
        assert!(r.into_remaining().is_empty());
    }
}
