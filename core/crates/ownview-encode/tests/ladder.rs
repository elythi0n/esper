#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Every rung of the CPU budget's ladder must configure a real encoder.
//!
//! The ladder is a table of numbers, and a table of numbers can describe a
//! configuration the encoder refuses — or worse, one that makes it panic. This
//! walks all of them through an actual encode.
//!
//! openh264's `QpRange::new` **panics** on a range whose ends are the wrong way
//! round or whose maximum exceeds 51, so `EncoderConfig::is_well_formed` is
//! what stands between a bad quantiser and an abort. That check runs before
//! any of the wrapper's constructors, and this is the test that says the two
//! agree about every rung the governor can actually select.

use ownview_color::I420;
use ownview_encode::budget::LADDER;
use ownview_encode::encoder::{EncoderConfig, VideoEncoder};
use ownview_encode::openh264::OpenH264;

#[test]
fn every_rung_configures_and_encodes() {
    for native in [(1920u32, 1080u32), (3840, 2160), (1366, 768)] {
        for level in LADDER {
            let (w, h) = level.size(native.0, native.1);
            let config = EncoderConfig::for_size(w, h).with_level(level);
            assert!(
                config.is_well_formed(),
                "{native:?} at {}: {config:?}",
                level.describe()
            );

            let mut encoder = OpenH264::new(&config)
                .unwrap_or_else(|e| panic!("{native:?} at {}: {e}", level.describe()));
            let picture = I420::new(w, h).expect("a valid picture");
            let out = encoder
                .encode(&picture, true)
                .unwrap_or_else(|e| panic!("{native:?} at {}: {e}", level.describe()));
            assert!(
                !out.is_empty(),
                "{native:?} at {}: encoded to nothing",
                level.describe()
            );
            assert!(out.keyframe);
        }
    }
}

/// The quantiser range is the one configuration value that would reach a
/// panicking constructor, so the validation has to reject exactly what the
/// wrapper rejects.
#[test]
fn a_quantiser_range_the_wrapper_would_panic_on_is_refused_first() {
    let base = EncoderConfig::for_size(320, 240);
    for bad in [(40u8, 20u8), (10, 52), (52, 53), (30, 30)] {
        let config = EncoderConfig {
            qp: Some(bad),
            ..base
        };
        assert!(
            !config.is_well_formed(),
            "{bad:?} should be refused before it reaches QpRange::new, which \
             panics on it"
        );
        assert!(
            OpenH264::new(&config).is_err(),
            "{bad:?} must be an error rather than an abort"
        );
    }
}
