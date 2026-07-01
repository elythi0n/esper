//! Length-prefixed CBOR framing.
//!
//! ```text
//! ┌──────────────┬───────────────────────────┐
//! │ u32 len (BE) │ CBOR map                  │
//! └──────────────┴───────────────────────────┘
//! ```
//!
//! This is the first parser attacker-controlled bytes reach, so it is written
//! to a stricter standard than the rest of the crate:
//!
//! - **The length is validated before anything is allocated.** A peer claiming
//!   4 GiB must cost us a rejected header, not four gigabytes.
//! - **No panicking constructs.** A panic reachable from the network is a
//!   denial of service; the workspace denies `unwrap`, `expect`, `panic!` and
//!   `indexing_slicing` so this is enforced rather than intended.
//! - **Errors are typed and carry a wire reason code**, so a malformed frame
//!   produces a clean close rather than a dropped connection with no
//!   explanation.

use crate::codes::Reason;

/// Maximum control frame length. Anything larger is a bulk stream by
/// definition, so this is a protocol invariant rather than a tuning knob.
pub const MAX_FRAME_LEN: u32 = 1024 * 1024;

/// The length prefix is a big-endian `u32`.
const LEN_PREFIX: usize = 4;

/// A framing failure.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// The declared length exceeds [`MAX_FRAME_LEN`]. Rejected before allocating.
    #[error("frame length {len} exceeds maximum {MAX_FRAME_LEN}")]
    TooLarge { len: u32 },

    /// A zero-length frame carries no CBOR and is always a protocol error.
    #[error("frame length is zero")]
    Empty,

    /// The stream ended before a complete frame was read.
    #[error("stream ended mid-frame: wanted {wanted} bytes, got {got}")]
    Truncated { wanted: usize, got: usize },

    /// The payload was not valid CBOR, or did not match the expected shape.
    #[error("malformed CBOR payload: {0}")]
    Malformed(String),

    /// The value could not be encoded to CBOR.
    #[error("could not encode to CBOR: {0}")]
    Encode(String),

    /// Underlying I/O failure.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

impl FrameError {
    /// The wire reason code to close the connection with.
    #[must_use]
    pub const fn reason(&self) -> Reason {
        match self {
            Self::TooLarge { .. } => Reason::FRAME_TOO_LARGE,
            Self::Empty | Self::Truncated { .. } | Self::Malformed(_) => Reason::MALFORMED_FRAME,
            // An encode failure or an I/O error is our fault or the network's,
            // not the peer's; neither is a protocol violation by them.
            Self::Encode(_) | Self::Io(_) => Reason::MALFORMED_FRAME,
        }
    }
}

/// Read one frame and decode it as `T`.
///
/// Reads exactly the declared number of bytes and no more, so the reader is
/// left positioned at the start of the next frame.
///
/// # Errors
/// Returns [`FrameError`] if the length is invalid, the stream is truncated, or
/// the payload is not valid CBOR of the expected shape.
pub fn read_frame<T, R>(reader: &mut R) -> Result<T, FrameError>
where
    T: serde::de::DeserializeOwned,
    R: std::io::Read,
{
    let mut len_buf = [0u8; LEN_PREFIX];
    read_exact_counted(reader, &mut len_buf)?;
    let len = u32::from_be_bytes(len_buf);

    // Validate before allocating. This ordering is the point of the function.
    if len == 0 {
        return Err(FrameError::Empty);
    }
    if len > MAX_FRAME_LEN {
        return Err(FrameError::TooLarge { len });
    }

    let mut payload = vec![0u8; len as usize];
    read_exact_counted(reader, &mut payload)?;

    ciborium::from_reader(payload.as_slice()).map_err(|e| FrameError::Malformed(e.to_string()))
}

/// Encode `value` as CBOR and write it with its length prefix.
///
/// # Errors
/// Returns [`FrameError::Encode`] if the value cannot be serialized,
/// [`FrameError::TooLarge`] if the encoding exceeds [`MAX_FRAME_LEN`], or
/// [`FrameError::Io`] on a write failure.
pub fn write_frame<T, W>(writer: &mut W, value: &T) -> Result<(), FrameError>
where
    T: serde::Serialize,
    W: std::io::Write,
{
    let mut payload = Vec::new();
    ciborium::into_writer(value, &mut payload).map_err(|e| FrameError::Encode(e.to_string()))?;

    let len = u32::try_from(payload.len()).map_err(|_| FrameError::TooLarge { len: u32::MAX })?;
    if len == 0 {
        return Err(FrameError::Empty);
    }
    if len > MAX_FRAME_LEN {
        return Err(FrameError::TooLarge { len });
    }

    writer.write_all(&len.to_be_bytes())?;
    writer.write_all(&payload)?;
    writer.flush()?;
    Ok(())
}

/// `read_exact`, but reporting how much was actually available on truncation.
///
/// `std::io::Read::read_exact` collapses a short read into an opaque
/// `UnexpectedEof`; knowing the shortfall is the difference between a useful
/// log line and a mystery.
fn read_exact_counted<R: std::io::Read>(reader: &mut R, buf: &mut [u8]) -> Result<(), FrameError> {
    let wanted = buf.len();
    let mut got = 0usize;
    let mut rest = buf;

    while !rest.is_empty() {
        match reader.read(rest) {
            Ok(0) => return Err(FrameError::Truncated { wanted, got }),
            Ok(n) => {
                got = got.saturating_add(n);
                // `split_at_mut` avoids indexing, which the workspace denies.
                let (_done, remaining) = rest.split_at_mut(n);
                rest = remaining;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(FrameError::Io(e)),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Hello, Role};

    fn sample() -> Hello {
        Hello::new(Role::Host, [7u8; 32], "test-host")
    }

    #[test]
    fn round_trips() {
        let mut buf = Vec::new();
        write_frame(&mut buf, &sample()).expect("write");
        let back: Hello = read_frame(&mut buf.as_slice()).expect("read");
        assert_eq!(back, sample());
    }

    #[test]
    fn reader_is_left_at_the_next_frame() {
        let mut buf = Vec::new();
        write_frame(&mut buf, &sample()).expect("first");
        write_frame(&mut buf, &sample()).expect("second");

        let mut cursor = buf.as_slice();
        let _: Hello = read_frame(&mut cursor).expect("first read");
        let _: Hello = read_frame(&mut cursor).expect("second read");
        assert!(cursor.is_empty(), "both frames consumed exactly");
    }

    #[test]
    fn oversized_length_is_rejected_without_allocating() {
        // A peer claiming 4 GiB. The four-byte header is all we should read.
        let header = u32::MAX.to_be_bytes();
        let err = read_frame::<Hello, _>(&mut header.as_slice()).expect_err("must reject");
        assert!(matches!(err, FrameError::TooLarge { .. }));
        assert_eq!(err.reason(), Reason::FRAME_TOO_LARGE);
    }

    #[test]
    fn length_exactly_at_the_cap_is_not_rejected_for_size() {
        // Boundary: MAX is allowed, MAX+1 is not. Truncation, not TooLarge.
        let header = MAX_FRAME_LEN.to_be_bytes();
        let err = read_frame::<Hello, _>(&mut header.as_slice()).expect_err("truncated");
        assert!(matches!(err, FrameError::Truncated { .. }), "got {err:?}");

        let header = (MAX_FRAME_LEN + 1).to_be_bytes();
        let err = read_frame::<Hello, _>(&mut header.as_slice()).expect_err("too large");
        assert!(matches!(err, FrameError::TooLarge { .. }));
    }

    #[test]
    fn zero_length_is_rejected() {
        let header = 0u32.to_be_bytes();
        let err = read_frame::<Hello, _>(&mut header.as_slice()).expect_err("must reject");
        assert!(matches!(err, FrameError::Empty));
    }

    #[test]
    fn truncated_stream_reports_the_shortfall() {
        let mut buf = Vec::new();
        write_frame(&mut buf, &sample()).expect("write");
        buf.truncate(buf.len() - 3);
        let err = read_frame::<Hello, _>(&mut buf.as_slice()).expect_err("must reject");
        match err {
            FrameError::Truncated { wanted, got } => assert!(got < wanted),
            other => panic!("expected Truncated, got {other:?}"),
        }
    }

    #[test]
    fn empty_stream_is_truncated_not_a_hang() {
        let err = read_frame::<Hello, _>(&mut [].as_slice()).expect_err("must reject");
        assert!(matches!(err, FrameError::Truncated { wanted: 4, got: 0 }));
    }

    #[test]
    fn garbage_payload_is_rejected_cleanly() {
        let payload = b"this is not cbor at all";
        let mut buf = (payload.len() as u32).to_be_bytes().to_vec();
        buf.extend_from_slice(payload);
        let err = read_frame::<Hello, _>(&mut buf.as_slice()).expect_err("must reject");
        assert!(matches!(err, FrameError::Malformed(_)));
        assert_eq!(err.reason(), Reason::MALFORMED_FRAME);
    }

    /// Every single-byte truncation of a valid frame must produce an error and
    /// never a panic. This is the cheap always-on version of the fuzz target.
    #[test]
    fn no_prefix_of_a_valid_frame_panics() {
        let mut buf = Vec::new();
        write_frame(&mut buf, &sample()).expect("write");
        for n in 0..buf.len() {
            let (prefix, _) = buf.split_at(n);
            let _ = read_frame::<Hello, _>(&mut &*prefix);
        }
    }

    /// Every single-byte corruption must produce an error and never a panic.
    #[test]
    fn no_single_byte_corruption_panics() {
        let mut buf = Vec::new();
        write_frame(&mut buf, &sample()).expect("write");
        for i in 0..buf.len() {
            let mut bad = buf.clone();
            if let Some(b) = bad.get_mut(i) {
                *b = b.wrapping_add(0x5A);
            }
            let _ = read_frame::<Hello, _>(&mut bad.as_slice());
        }
    }
}
