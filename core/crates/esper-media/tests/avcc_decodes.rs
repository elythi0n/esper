#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! What the webview is handed, decoded by something that is not the webview.
//!
//! The unit tests prove the framing against synthetic units. This proves the
//! thing that matters: that an `avcC` description and the samples beside it
//! carry a **complete** stream — that nothing a decoder needs was left in the
//! Annex-B the repackager consumed.
//!
//! The same argument as `real_bitstream.rs` next door: a repackager that is
//! subtly wrong still round-trips through its own inverse, and both halves
//! agreeing on a mistake is the failure a self-round-trip cannot see. So the
//! rebuild here reads the description **by its specified layout** rather than
//! through the struct that wrote it, and openh264 decides whether the result
//! is a picture.

use esper_capture::pattern::Pattern;
use esper_color::{ColorSpace, I420};
use esper_decode::decoder::{DecoderConfig, VideoDecoder};
use esper_decode::openh264::OpenH264Decoder;
use esper_encode::encoder::{EncoderConfig, VideoEncoder};
use esper_encode::openh264::OpenH264;
use esper_media::avcc::{LENGTH_PREFIX, Repackager};

const W: u32 = 320;
const H: u32 = 240;
const FRAMES: u64 = 12;

fn frame(index: u64) -> I420 {
    let pattern = Pattern::new(W, H).expect("a valid size");
    let mut img = I420::new(W, H).expect("a valid size");
    let space = ColorSpace::default();
    let (ys, us) = (img.y_stride(), img.uv_stride());
    let (y, u, v) = img.planes_mut();
    for row in 0..H as usize {
        for col in 0..W as usize {
            let [r, g, b] = pattern.pixel(col as u32, row as u32, index);
            let (yy, uu, vv) = esper_color::convert::reference_yuv(r, g, b, space);
            if let Some(p) = y.get_mut(row * ys + col) {
                *p = yy;
            }
            if row % 2 == 0 && col % 2 == 0 {
                let at = (row / 2) * us + col / 2;
                if let Some(p) = u.get_mut(at) {
                    *p = uu;
                }
                if let Some(p) = v.get_mut(at) {
                    *p = vv;
                }
            }
        }
    }
    img
}

/// The parameter sets, read out of an `avcC` record by its layout.
///
/// Deliberately not `ParameterSets::description`'s inverse: this is what a
/// decoder does with the bytes, and it is the only way the test can disagree
/// with the code under test about what those bytes mean.
fn sets_from_description(d: &[u8]) -> (Vec<u8>, Vec<u8>) {
    assert_eq!(d[0], 1, "configurationVersion");
    assert_eq!(d[4] & 0x03, 3, "four-byte lengths");
    assert_eq!(d[5] & 0x1F, 1, "one SPS");
    let sps_len = usize::from(u16::from_be_bytes([d[6], d[7]]));
    let sps = d[8..8 + sps_len].to_vec();
    let at = 8 + sps_len;
    assert_eq!(d[at], 1, "one PPS");
    let pps_len = usize::from(u16::from_be_bytes([d[at + 1], d[at + 2]]));
    let pps = d[at + 3..at + 3 + pps_len].to_vec();
    (sps, pps)
}

/// AVCC back to Annex-B, which is what a decoder reconstructs internally.
fn to_annexb(sample: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(sample.len() + 16);
    let mut at = 0;
    while at + LENGTH_PREFIX <= sample.len() {
        let len = u32::from_be_bytes(sample[at..at + LENGTH_PREFIX].try_into().unwrap()) as usize;
        at += LENGTH_PREFIX;
        assert!(
            at + len <= sample.len(),
            "a length prefix that runs past the sample"
        );
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&sample[at..at + len]);
        at += len;
    }
    assert_eq!(
        at,
        sample.len(),
        "the sample is exactly its length-prefixed units"
    );
    out
}

#[test]
fn a_repackaged_stream_still_decodes() {
    let mut encoder = OpenH264::new(&EncoderConfig {
        fps: 30,
        ..EncoderConfig::for_size(W, H)
    })
    .expect("an encoder");
    let mut decoder = OpenH264Decoder::new(&DecoderConfig::default()).expect("a decoder");
    let mut repack = Repackager::new();

    let mut description: Option<Vec<u8>> = None;
    let mut decoded = 0usize;
    let mut keyframes = 0usize;

    for index in 0..FRAMES {
        let encoded = encoder.encode(&frame(index), index == 0).expect("encoded");
        let Some(sample) = repack.push(&encoded.bitstream) else {
            continue;
        };
        if repack.take_description_change() {
            description = Some(repack.sets().description());
        }
        if sample.keyframe {
            keyframes += 1;
        }

        // What a decoder is given: the parameter sets from the description,
        // once, and then the samples.
        let mut feed = Vec::new();
        if sample.keyframe {
            let (sps, pps) = sets_from_description(description.as_ref().expect("a description"));
            feed.extend_from_slice(&[0, 0, 0, 1]);
            feed.extend_from_slice(&sps);
            feed.extend_from_slice(&[0, 0, 0, 1]);
            feed.extend_from_slice(&pps);
        }
        feed.extend_from_slice(&to_annexb(&sample.bytes));

        if let Some(picture) = decoder.decode(&feed).expect("the decoder took it") {
            assert_eq!(
                (picture.width(), picture.height()),
                (W, H),
                "the picture that came back is the picture that went in"
            );
            decoded += 1;
        }
    }

    assert!(
        keyframes >= 1,
        "the first picture has to be one a decoder can begin at"
    );
    assert!(
        decoded >= FRAMES as usize - 2,
        "only {decoded} of {FRAMES} pictures survived the repackaging"
    );
}

#[test]
fn the_codec_string_matches_what_the_encoder_made() {
    let mut encoder = OpenH264::new(&EncoderConfig {
        fps: 30,
        ..EncoderConfig::for_size(W, H)
    })
    .expect("an encoder");
    let mut repack = Repackager::new();
    repack.push(&encoder.encode(&frame(0), true).expect("encoded").bitstream);

    let codec = repack.sets().codec();
    // Main profile is the encoder's ceiling and the design's; 0x4D is Main.
    // The level is the encoder's business and is not asserted — what matters
    // is that the string is *derived*, so a level change carries into it.
    assert!(
        codec.starts_with("avc1.4D"),
        "expected Main profile, got {codec}"
    );
    assert_eq!(codec.len(), "avc1.4D4028".len());
    let (sps, _) = sets_from_description(&repack.sets().description());
    assert_eq!(
        codec,
        format!("avc1.{:02X}{:02X}{:02X}", sps[1], sps[2], sps[3]),
        "the string has to be the SPS's own three bytes"
    );
}
