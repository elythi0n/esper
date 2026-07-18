//! The openh264 decoder, in this process.
//!
//! **Not the one to ship on a controller.** It is here because a decoder has
//! to work before it is worth jailing, because the round-trip test needs one,
//! and because the sandboxed decoder will use exactly this code inside its own
//! process. What it is not is safe to point at a peer: a large C parser in the
//! same address space as the session keys is precisely the arrangement the
//! design refuses.
//!
//! [`Self::is_sandboxed`] returns `false`, and it is checked, so a controller
//! wired to this by accident can be made to say so.

use std::time::Instant;

use esper_color::i420::I420;
use openh264::OpenH264API;
use openh264::decoder::{Decoder as Inner, DecoderConfig as InnerConfig};
use openh264::formats::YUVSource;

use crate::decoder::{
    DecodeError, DecodeStats, DecoderConfig, VideoDecoder, check_packet, check_size,
};

/// The backend name, for the HUD and the log.
pub const BACKEND: &str = "openh264";

/// A software H.264 decoder running in this process.
pub struct OpenH264Decoder {
    inner: Inner,
    config: DecoderConfig,
    stats: DecodeStats,
    /// Reused across pictures, so a stream does not allocate per frame.
    out: I420,
}

impl std::fmt::Debug for OpenH264Decoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenH264Decoder")
            .field("max_pixels", &self.config.max_pixels)
            .field("sandboxed", &false)
            .finish_non_exhaustive()
    }
}

impl OpenH264Decoder {
    /// A decoder with this configuration.
    pub fn new(config: &DecoderConfig) -> Result<Self, DecodeError> {
        // Error concealment stays off, which is openh264's default and the
        // right answer here for a reason worth stating: concealment means the
        // decoder tries to invent plausible pixels from corrupt data, which is
        // *more* parsing of malformed input rather than less. On a hostile
        // stream that is the wrong direction. A corrupt picture should be a
        // dropped picture and a keyframe request.
        let inner_config = InnerConfig::new().debug(false);
        let inner =
            Inner::with_api_config(OpenH264API::from_source(), inner_config).map_err(|e| {
                DecodeError::Backend {
                    backend: BACKEND,
                    detail: e.to_string(),
                }
            })?;
        // A placeholder; the first picture resizes it. Sixteen square rather
        // than zero because `I420` refuses a zero dimension, and it is one
        // allocation either way.
        let out = I420::new(16, 16)?;
        Ok(Self {
            inner,
            config: *config,
            stats: DecodeStats::default(),
            out,
        })
    }

    /// The configuration in force.
    #[must_use]
    pub const fn config(&self) -> &DecoderConfig {
        &self.config
    }
}

impl VideoDecoder for OpenH264Decoder {
    fn backend(&self) -> &'static str {
        BACKEND
    }

    fn is_sandboxed(&self) -> bool {
        false
    }

    fn decode(&mut self, packet: &[u8]) -> Result<Option<I420>, DecodeError> {
        // Before the C sees it.
        if let Err(e) = check_packet(packet) {
            self.stats.refused = self.stats.refused.saturating_add(1);
            return Err(e);
        }
        self.stats.packets = self.stats.packets.saturating_add(1);
        self.stats.bytes = self.stats.bytes.saturating_add(packet.len() as u64);

        let at = Instant::now();
        let decoded = self
            .inner
            .decode(packet)
            .map_err(|e| DecodeError::Backend {
                backend: BACKEND,
                detail: e.to_string(),
            })?;
        let took = at.elapsed();

        // No picture yet. Ordinary: parameter sets arrive without one, and a
        // picture can span packets.
        let Some(yuv) = decoded else {
            return Ok(None);
        };

        let (w, h) = yuv.dimensions();
        let width = u32::try_from(w).unwrap_or(u32::MAX);
        let height = u32::try_from(h).unwrap_or(u32::MAX);
        check_size(width, height, self.config.max_pixels)?;

        let (y_stride, u_stride, v_stride) = yuv.strides();
        let _ = self.out.resize(width, height)?;
        let dest_y = self.out.y_stride();
        let dest_uv = self.out.uv_stride();
        let uv_rows = self.out.uv_height();
        let (dy, du, dv) = self.out.planes_mut();

        // Copied row by row, because **the decoder's strides are its own**.
        // openh264 pads its planes to its internal alignment, so a copy that
        // assumed the destination's stride would shear the picture — the same
        // mistake as reading a capture buffer as though it were tightly
        // packed, in the other direction.
        copy_plane(yuv.y(), y_stride, dy, dest_y, width as usize, height)?;
        copy_plane(
            yuv.u(),
            u_stride,
            du,
            dest_uv,
            (width / 2) as usize,
            uv_rows,
        )?;
        copy_plane(
            yuv.v(),
            v_stride,
            dv,
            dest_uv,
            (width / 2) as usize,
            uv_rows,
        )?;

        self.stats.pictures = self.stats.pictures.saturating_add(1);
        self.stats.spent = self.stats.spent.saturating_add(took);
        if took > self.stats.worst {
            self.stats.worst = took;
        }

        // Handed out as a fresh buffer rather than a borrow of `self.out`,
        // because the caller wants to keep it while the next packet decodes.
        // The copy is one memcpy of a plane set; borrowing would mean the
        // caller could not hold two pictures, which a jitter buffer must.
        let mut handed = I420::new(width, height)?;
        {
            let (hy, hu, hv) = handed.planes_mut();
            hy.copy_from_slice(self.out.y());
            hu.copy_from_slice(self.out.u());
            hv.copy_from_slice(self.out.v());
        }
        Ok(Some(handed))
    }

    fn stats(&self) -> DecodeStats {
        self.stats
    }
}

/// Copy one plane, honouring both strides.
///
/// Refuses rather than truncating if the source is short: a plane shorter than
/// its declared dimensions means the decoder and its own output disagree,
/// which is a reason to stop trusting this decoder instance rather than to
/// paper over with whatever bytes are there.
fn copy_plane(
    src: &[u8],
    src_stride: usize,
    dst: &mut [u8],
    dst_stride: usize,
    row_bytes: usize,
    rows: u32,
) -> Result<(), DecodeError> {
    let needed = src_stride
        .saturating_mul((rows as usize).saturating_sub(1))
        .saturating_add(row_bytes);
    if src.len() < needed {
        return Err(DecodeError::ShortPlane {
            needed,
            got: src.len(),
        });
    }

    for row in 0..rows as usize {
        let from = src_stride.saturating_mul(row);
        let to = dst_stride.saturating_mul(row);
        let Some(source) = src.get(from..from.saturating_add(row_bytes)) else {
            return Err(DecodeError::ShortPlane {
                needed,
                got: src.len(),
            });
        };
        let Some(target) = dst.get_mut(to..to.saturating_add(row_bytes)) else {
            return Err(DecodeError::ShortPlane {
                needed: to.saturating_add(row_bytes),
                got: dst.len(),
            });
        };
        target.copy_from_slice(source);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decoder() -> OpenH264Decoder {
        OpenH264Decoder::new(&DecoderConfig::default()).expect("decoder")
    }

    /// The bound has to bite before the C parser is reached, not after.
    #[test]
    fn an_oversized_packet_never_reaches_the_parser() {
        let mut d = decoder();
        let huge = vec![0u8; crate::decoder::MAX_PACKET + 1];
        let err = d.decode(&huge).expect_err("too large");
        assert!(matches!(err, DecodeError::PacketTooLarge { .. }), "{err:?}");
        assert_eq!(d.stats().refused, 1);
        assert_eq!(
            d.stats().packets,
            0,
            "a refused packet must not be counted as one the decoder looked at"
        );
    }

    /// Garbage must be refused or ignored, never accepted as a picture and
    /// never a panic. This is the entry point the fuzz target hammers; these
    /// are the cases worth having as a fast regression test too.
    #[test]
    fn garbage_is_never_mistaken_for_a_picture() {
        let mut d = decoder();
        let cases: Vec<Vec<u8>> = vec![
            vec![],
            vec![0],
            vec![0, 0, 0, 1],
            vec![0, 0, 0, 1, 0x67],
            vec![0, 0, 0, 1, 0x65],
            vec![0xFF; 64],
            (0..=255u8).collect(),
            vec![0, 0, 0, 1, 0x67, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF],
        ];
        for case in cases {
            match d.decode(&case) {
                Ok(None) | Err(_) => {}
                Ok(Some(picture)) => panic!(
                    "garbage {:02x?} produced a {}x{} picture",
                    case.get(..8.min(case.len())),
                    picture.width(),
                    picture.height()
                ),
            }
        }
    }

    /// An in-process decoder must say what it is, so a controller wired to it
    /// by accident can be caught.
    #[test]
    fn it_reports_that_it_is_not_sandboxed() {
        let d = decoder();
        assert_eq!(d.backend(), "openh264");
        assert!(
            !d.is_sandboxed(),
            "this decoder shares an address space with the session keys, and \
             must not claim otherwise"
        );
        let dbg = format!("{d:?}");
        assert!(dbg.contains("sandboxed: false"), "{dbg}");
    }

    /// A stride mismatch shears a picture, and the copy honours both strides
    /// precisely so it does not. Checked directly, because constructing a
    /// decoder whose output is padded is not something a test can arrange.
    #[test]
    fn a_plane_copy_honours_both_strides() {
        // Source: 4 pixels a row, padded to 8 bytes.
        let src: Vec<u8> = (0..32u8).collect();
        // Destination: 4 pixels a row, padded to 6.
        let mut dst = vec![0xEEu8; 24];
        copy_plane(&src, 8, &mut dst, 6, 4, 4).expect("copied");

        for row in 0..4usize {
            let want = src.get(row * 8..row * 8 + 4).expect("source row");
            let got = dst.get(row * 6..row * 6 + 4).expect("dest row");
            assert_eq!(got, want, "row {row} landed at the wrong offset");
            // And the destination's own padding is untouched.
            assert_eq!(
                dst.get(row * 6 + 4..row * 6 + 6),
                Some(&[0xEE, 0xEE][..]),
                "row {row}'s padding was written into"
            );
        }
    }

    /// A plane shorter than its dimensions means the decoder disagrees with
    /// itself. Refused rather than papered over with whatever bytes are there.
    #[test]
    fn a_short_plane_is_refused_rather_than_truncated() {
        let src = vec![0u8; 10];
        let mut dst = vec![0u8; 64];
        let err = copy_plane(&src, 8, &mut dst, 8, 8, 4).expect_err("too short");
        match err {
            DecodeError::ShortPlane { needed, got } => {
                assert_eq!(needed, 8 * 3 + 8);
                assert_eq!(got, 10);
            }
            other => panic!("{other:?}"),
        }
    }

    /// A destination too small must be refused rather than written past.
    #[test]
    fn a_short_destination_is_refused() {
        let src = vec![0u8; 64];
        let mut dst = vec![0u8; 10];
        let err = copy_plane(&src, 8, &mut dst, 8, 8, 4).expect_err("dest too small");
        assert!(matches!(err, DecodeError::ShortPlane { .. }), "{err:?}");
    }

    /// A tightly packed plane is the easy case and must also be exact.
    #[test]
    fn a_tightly_packed_plane_copies_exactly() {
        let src: Vec<u8> = (0..16u8).collect();
        let mut dst = vec![0u8; 16];
        copy_plane(&src, 4, &mut dst, 4, 4, 4).expect("copied");
        assert_eq!(dst, src);
    }
}
