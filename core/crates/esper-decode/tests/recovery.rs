#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! **A keyframe rescues a decoder that lost sync — the library's own promise.**
//!
//! This is the fact the whole recovery path is built on, and it belongs in a
//! test because it is a property of openh264 rather than of anything here: a
//! decoder that has just refused a frame for want of its reference pictures
//! must be able to start again from a keyframe, with no reset and no new
//! decoder. Everything above it — the receiver asking, the host obliging, the
//! worker passing the frame on — is only worth building if this holds.
//!
//! Its companion is `worker_recovery`, which asks the same question of the
//! sandboxed worker in front of the library. That one caught a real bug; this
//! one is why the fix to it was "stop refusing to pass the keyframe on"
//! rather than "restart the decoder".

use esper_capture::pattern::Pattern;
use esper_color::convert::whole_frame;
use esper_color::{ColorSpace, I420};
use esper_decode::decoder::{DecoderConfig, VideoDecoder};
use esper_decode::openh264::OpenH264Decoder;
use esper_encode::encoder::{EncoderConfig, VideoEncoder};
use esper_encode::openh264::OpenH264;

mod common;
use common::captured;

#[test]
fn a_keyframe_rescues_a_decoder_that_lost_sync() {
    let (w, h) = (320u32, 180u32);
    let pattern = Pattern::new(w, h).expect("a valid size");
    let space = ColorSpace::default();
    let mut staging = I420::new(w, h).expect("a valid size");
    let mut encoder = OpenH264::new(&EncoderConfig {
        fps: 15,
        ..EncoderConfig::for_size(staging.width(), staging.height())
    })
    .expect("an encoder");
    let mut decoder = OpenH264Decoder::new(&DecoderConfig::default()).expect("a decoder");

    let mut encode = |index: u64, keyframe: bool| {
        let frame = captured(&pattern, index);
        let _ = whole_frame(&frame, space, &mut staging).expect("converted");
        encoder
            .encode(&staging, keyframe)
            .expect("encoded")
            .bitstream
    };

    let first = encode(0, true);
    assert!(
        decoder.decode(&first).expect("the first frame").is_some(),
        "a keyframe on a fresh decoder must produce a picture"
    );

    // The link loses frame 1 outright, and the three after it arrive whole.
    // Each refers to a picture the decoder does not have, so each is refused —
    // correctly, because error concealment is off on purpose.
    let _lost_in_flight = encode(1, false);
    for index in 2..5u64 {
        let orphan = encode(index, false);
        assert!(
            decoder.decode(&orphan).is_err(),
            "frame {index} refers to a lost picture and must not be concealed"
        );
    }

    // Three of them, and it still recovers on the first keyframe after.
    let rescue = encode(5, true);
    let picture = decoder
        .decode(&rescue)
        .expect("a keyframe must rescue a decoder that lost sync")
        .expect("a keyframe must produce a picture");
    assert_eq!((picture.width(), picture.height()), (w, h));

    for index in 6..12u64 {
        let next = encode(index, false);
        assert!(
            decoder
                .decode(&next)
                .expect("a frame after recovery")
                .is_some(),
            "frame {index} follows a decoded keyframe and should decode"
        );
    }

    let stats = decoder.stats();
    assert_eq!(
        stats.undecodable, 3,
        "the three orphaned frames should be counted as undecodable"
    );
    assert_eq!(
        stats.refused, 0,
        "nothing was refused by this crate's own bounds"
    );
}
