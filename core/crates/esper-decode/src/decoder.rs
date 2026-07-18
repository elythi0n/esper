//! What a decoder is, and the fact that shapes everything about it.
//!
//! # This is the only place a peer's bytes reach a large C parser
//!
//! Everywhere else in Esper, attacker-controlled input meets Rust that
//! forbids unsafe code: the frame reader, the message decoder, the pairing
//! exchange. Here it meets 155 000 lines of vendored C, and the C is not
//! optional — there is no H.264 decoder in safe Rust worth shipping.
//!
//! So the posture is different. Everything else in the pipeline is written to
//! be correct. This is written on the assumption that the thing underneath it
//! is **not**, and that the job is to bound what a failure can reach:
//!
//! | | |
//! |---|---|
//! | Bounded input | a packet larger than [`MAX_PACKET`] is refused before the C sees it |
//! | Bounded output | a picture larger than the configured maximum is refused rather than copied |
//! | No error concealment | concealment means *more* parsing of corrupt data to invent plausible pixels, which is the opposite of what hostile input calls for |
//! | Its own process | so a crash is a dropped session rather than a compromised machine |
//! | A sandbox | seccomp and Landlock, so a compromise reaches no files, no network and no other session |
//! | Continuously fuzzed | on this exact entry point |
//!
//! The last three are not in this module. The trait exists so they can be a
//! second implementation rather than a rewrite of this one — the same reason
//! the encoder has a trait with one implementation.
//!
//! # What this module cannot do
//!
//! **It cannot bound openh264's own allocations.** A malicious sequence
//! parameter set can declare a picture of 16384 by 16384, and openh264
//! allocates for it inside `decode` before returning anything this code could
//! inspect. Refusing it afterwards is too late.
//!
//! Pre-checking would mean parsing the parameter set ourselves — writing a new
//! parser at the same trust boundary to avoid using an existing one, which is
//! not obviously a gain. So that bound belongs to the process: an address
//! space limit on the child, which is exactly the kind of thing a separate
//! process can have and an in-process decoder cannot.
//!
//! Stated here rather than in a commit message, because someone reading this
//! module needs to know which bounds it actually provides.

use esper_color::i420::{I420, I420Error};

/// Largest packet the decoder will look at.
///
/// Two megabytes: comfortably above a 4K keyframe at any sane bitrate, and far
/// below the point where a peer can make the host allocate meaningfully by
/// claiming a large frame. The transport's own frame cap is smaller still;
/// this is the media layer's own bound, so that a decoder handed bytes from
/// somewhere else is still bounded.
pub const MAX_PACKET: usize = 2 * 1024 * 1024;

/// Largest picture the decoder will hand onwards, in pixels.
///
/// 8K, which is more than the product supports and less than the 268 megapixel
/// a level-6.2 stream may declare. A peer announcing a picture this side
/// refuses is a peer whose stream is dropped, which is the correct outcome —
/// no viewer asked for a 268 megapixel screen.
pub const MAX_PIXELS: u64 = 7680 * 4320;

/// How the decoder should behave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecoderConfig {
    /// Largest picture to accept, in pixels.
    pub max_pixels: u64,
    /// How many threads the decoder may use.
    ///
    /// One, by default. A decoder runs on the *viewer's* machine, so the
    /// "not your machine" argument does not apply — but a threaded decoder is
    /// more concurrent C reachable from hostile input, and the measurement
    /// that would justify it does not exist yet.
    pub threads: u16,
}

impl Default for DecoderConfig {
    fn default() -> Self {
        Self {
            max_pixels: MAX_PIXELS,
            threads: 1,
        }
    }
}

/// What went wrong.
///
/// Every variant is a reason to drop the stream rather than retry, except
/// [`DecodeError::NeedMoreData`], which is not an error at all — see
/// [`VideoDecoder::decode`].
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    /// The packet is larger than [`MAX_PACKET`].
    ///
    /// Refused **before** the C parser sees it, which is the whole point of
    /// having the bound here rather than trusting the transport's.
    #[error("a {len} byte packet exceeds the {MAX_PACKET} byte limit")]
    PacketTooLarge { len: usize },

    /// The stream declares a picture larger than the configuration allows.
    #[error("the stream declares a {width}x{height} picture, over the {max} pixel limit")]
    PictureTooLarge { width: u32, height: u32, max: u64 },

    /// The stream declares a picture that cannot be one.
    #[error("the stream declares an impossible {width}x{height} picture")]
    ImpossiblePicture { width: u32, height: u32 },

    /// The decoded planes are not the size the declared dimensions require.
    ///
    /// Its own variant because it means the decoder and its own output
    /// disagree, which is not a stream problem — it is a reason to stop
    /// trusting this decoder instance.
    #[error("the decoder returned {got} bytes for a plane needing {needed}")]
    ShortPlane { needed: usize, got: usize },

    /// The output buffer could not be sized.
    #[error(transparent)]
    Buffer(#[from] I420Error),

    /// The backend failed.
    #[error("{backend}: {detail}")]
    Backend {
        backend: &'static str,
        detail: String,
    },
}

impl DecodeError {
    /// Whether this stream can continue.
    ///
    /// Nothing here is recoverable, and saying so explicitly matters: a caller
    /// that retried on a malformed packet would be feeding the same bytes to
    /// the same C parser in a loop, which turns one parsing bug into a
    /// sustained one.
    #[must_use]
    pub const fn is_fatal_to_the_stream(&self) -> bool {
        true
    }
}

/// Running totals, for the session HUD and the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DecodeStats {
    pub packets: u64,
    /// Packets that produced a picture.
    pub pictures: u64,
    pub bytes: u64,
    pub spent: std::time::Duration,
    /// The slowest single decode, because a mean hides the frame that stuttered.
    pub worst: std::time::Duration,
    /// Packets refused by this module's own bounds, before the C saw them.
    ///
    /// Worth counting separately: a session where this is non-zero is a
    /// session being sent things it did not ask for.
    pub refused: u64,
}

impl DecodeStats {
    /// Mean milliseconds per picture.
    #[must_use]
    pub fn mean_ms(&self) -> f64 {
        if self.pictures == 0 {
            return 0.0;
        }
        self.spent.as_secs_f64() * 1000.0 / self.pictures as f64
    }
}

/// Something that turns a bitstream into pictures.
pub trait VideoDecoder: std::fmt::Debug + Send {
    /// Which backend this is, for the HUD and the log.
    fn backend(&self) -> &'static str;

    /// Whether this decoder is isolated from the rest of the process.
    ///
    /// Reported rather than assumed, because a viewer should be able to find
    /// out. An in-process decoder is a large C parser inside the application;
    /// the sandboxed one is not, and the difference is worth being able to see.
    fn is_sandboxed(&self) -> bool;

    /// Decode one packet of Annex-B bitstream.
    ///
    /// `Ok(None)` means the packet was consumed and did not complete a
    /// picture. That is ordinary — parameter sets arrive without pictures, and
    /// a picture can span packets — so it is `None` rather than an error.
    fn decode(&mut self, packet: &[u8]) -> Result<Option<I420>, DecodeError>;

    /// Running totals.
    fn stats(&self) -> DecodeStats;
}

/// Whether a declared picture size is one this decoder will handle.
///
/// Split out and public because both the in-process and the sandboxed decoder
/// need exactly this check, and a bound implemented twice is a bound that
/// disagrees with itself.
pub fn check_size(width: u32, height: u32, max_pixels: u64) -> Result<(), DecodeError> {
    if width == 0 || height == 0 {
        return Err(DecodeError::ImpossiblePicture { width, height });
    }
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if pixels > max_pixels {
        return Err(DecodeError::PictureTooLarge {
            width,
            height,
            max: max_pixels,
        });
    }
    Ok(())
}

/// Whether a packet is one this decoder will look at.
pub fn check_packet(packet: &[u8]) -> Result<(), DecodeError> {
    if packet.len() > MAX_PACKET {
        return Err(DecodeError::PacketTooLarge { len: packet.len() });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_conservative() {
        let c = DecoderConfig::default();
        assert_eq!(c.max_pixels, MAX_PIXELS);
        assert_eq!(
            c.threads, 1,
            "more threads is more concurrent C reachable from hostile input, \
             and nothing has measured that it is worth it"
        );
    }

    /// The bound exists so the C parser never sees an oversized packet.
    #[test]
    fn an_oversized_packet_is_refused() {
        assert!(check_packet(&[0u8; 1024]).is_ok());
        let err = check_packet(&vec![0u8; MAX_PACKET + 1]).expect_err("too large");
        match err {
            DecodeError::PacketTooLarge { len } => assert_eq!(len, MAX_PACKET + 1),
            other => panic!("{other:?}"),
        }
        // Exactly at the limit is allowed.
        assert!(check_packet(&vec![0u8; MAX_PACKET]).is_ok());
    }

    /// A malicious parameter set can declare a picture far larger than any
    /// viewer asked for. H.264 allows up to 268 megapixels at level 6.2.
    #[test]
    fn an_absurd_picture_size_is_refused() {
        assert!(check_size(1920, 1080, MAX_PIXELS).is_ok());
        assert!(
            check_size(7680, 4320, MAX_PIXELS).is_ok(),
            "8K is the ceiling"
        );

        let err = check_size(16384, 16384, MAX_PIXELS).expect_err("268 megapixels");
        assert!(
            matches!(err, DecodeError::PictureTooLarge { .. }),
            "{err:?}"
        );
    }

    /// A zero dimension is not a small picture, it is a malformed stream.
    #[test]
    fn a_zero_dimension_is_impossible_rather_than_small() {
        for (w, h) in [(0u32, 1080u32), (1920, 0), (0, 0)] {
            let err = check_size(w, h, MAX_PIXELS).expect_err("{w}x{h}");
            assert!(
                matches!(err, DecodeError::ImpossiblePicture { .. }),
                "{w}x{h}: {err:?}"
            );
        }
    }

    /// The pixel count must not wrap. Two dimensions near `u32::MAX` multiply
    /// to more than a `u64` holds if done carelessly, and release builds trap
    /// on overflow — so a hostile stream could turn a bound into a panic.
    #[test]
    fn the_pixel_count_cannot_overflow_into_passing() {
        let err = check_size(u32::MAX, u32::MAX, MAX_PIXELS).expect_err("absurd");
        assert!(
            matches!(err, DecodeError::PictureTooLarge { .. }),
            "{err:?}"
        );
        // And a caller with an absurd limit still gets a definite answer.
        assert!(check_size(u32::MAX, u32::MAX, u64::MAX).is_ok());
    }

    /// A configured limit lower than the default must be honoured, so a viewer
    /// on a small machine can refuse pictures it cannot afford.
    #[test]
    fn a_lower_configured_limit_is_honoured() {
        let modest = 1280 * 720;
        assert!(check_size(1280, 720, modest).is_ok());
        assert!(check_size(1920, 1080, modest).is_err());
    }

    /// Retrying a malformed packet would feed the same bytes to the same C
    /// parser in a loop, turning one parsing bug into a sustained one.
    #[test]
    fn every_failure_is_fatal_to_the_stream() {
        let errors = [
            DecodeError::PacketTooLarge { len: 1 },
            DecodeError::PictureTooLarge {
                width: 1,
                height: 1,
                max: 0,
            },
            DecodeError::ImpossiblePicture {
                width: 0,
                height: 0,
            },
            DecodeError::ShortPlane { needed: 2, got: 1 },
            DecodeError::Backend {
                backend: "test",
                detail: "x".to_owned(),
            },
        ];
        for e in errors {
            assert!(e.is_fatal_to_the_stream(), "{e:?}");
        }
    }

    #[test]
    fn statistics_over_nothing_do_not_divide_by_zero() {
        assert_eq!(DecodeStats::default().mean_ms(), 0.0);
    }
}
