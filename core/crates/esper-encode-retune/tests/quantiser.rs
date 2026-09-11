#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! The tripwire: openh264 still will not move the quantiser in place.
//!
//! The fidelity rung is the one change this crate cannot make, and the reason
//! is upstream rather than here — `iMinQp` and `iMaxQp` are not in
//! `WelsEncoderParamAdjust`'s copy list, so setting them on a running encoder
//! is accepted, reports success, and does nothing.
//!
//! That is a fact about a dependency, which means it can change. Left as a
//! comment it would quietly become false; measured here, the day it changes
//! this test fails and says the fidelity rung can move in place too.
//!
//! # Unsafe inventory
//!
//! - **`raw_api()` to read the current parameter block.** The same shape the
//!   library uses: `GetOption` with `ENCODER_OPTION_SVC_ENCODE_PARAM_EXT`
//!   only reads, into a caller-owned `SEncParamExt` that `Default` has fully
//!   initialised before the pointer is taken, and the pointer does not
//!   outlive the call. The encoder is initialised by construction.
//! - **`raw_api()` to write a quantiser range back.** Deliberately setting
//!   two fields the library does *not* set, because the point of this file is
//!   to find out whether they take. Nothing else in the block differs from
//!   the one just read, so picture size, profile, level and layer count are
//!   carried through untouched — and the encoder is thrown away at the end of
//!   the call either way.

use openh264::OpenH264API;
use openh264::encoder::{
    BitRate, Encoder, EncoderConfig, FrameRate, IntraFramePeriod, QpRange, RateControlMode,
};
use openh264::formats::YUVSource;
use openh264_sys2::{ENCODER_OPTION_SVC_ENCODE_PARAM_EXT, SEncParamExt};

/// Screen-shaped content, which is what a quantiser change actually shows up
/// on: flat ground and hard edges, not noise.
struct Screen {
    w: usize,
    h: usize,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

impl Screen {
    fn new(w: usize, h: usize, shift: usize) -> Self {
        let mut y = vec![30u8; w * h];
        for row in 0..h {
            for col in 0..w {
                let c = (col + shift) % w;
                let ground = if (row / 16 + c / 16).is_multiple_of(2) {
                    28
                } else {
                    34
                };
                let edge = if c % 64 < 2 || row % 48 < 2 {
                    210
                } else {
                    ground
                };
                y[row * w + col] =
                    if row % 48 >= 8 && row % 48 < 20 && (c / 3 + row).is_multiple_of(7) {
                        190
                    } else {
                        edge
                    };
            }
        }
        Self {
            w,
            h,
            y,
            u: vec![124; w * h / 4],
            v: vec![132; w * h / 4],
        }
    }
}

impl YUVSource for Screen {
    fn dimensions(&self) -> (usize, usize) {
        (self.w, self.h)
    }
    fn strides(&self) -> (usize, usize, usize) {
        (self.w, self.w / 2, self.w / 2)
    }
    fn y(&self) -> &[u8] {
        &self.y
    }
    fn u(&self) -> &[u8] {
        &self.u
    }
    fn v(&self) -> &[u8] {
        &self.v
    }
}

/// Encode thirty pictures, optionally pushing a quantiser range at the running
/// encoder first. Returns the total bytes.
fn bytes(built: (u8, u8), pushed: Option<(i32, i32)>) -> usize {
    let config = EncoderConfig::new()
        .rate_control_mode(RateControlMode::Quality)
        .bitrate(BitRate::from_bps(2_000_000))
        .max_frame_rate(FrameRate::from_hz(30.0))
        // Far out of reach, so no periodic keyframe lands inside the run.
        .intra_frame_period(IntraFramePeriod::from_num_frames(600))
        .qp(QpRange::new(built.0, built.1))
        .debug(false);
    let mut enc = Encoder::with_api_config(OpenH264API::from_source(), config).expect("an encoder");
    enc.encode(&Screen::new(640, 480, 0))
        .expect("the first picture");

    if let Some((min, max)) = pushed {
        let mut params = SEncParamExt::default();
        #[allow(unsafe_code)]
        // SAFETY: the module's inventory covers this shape — a read into a
        // caller-owned, fully initialised struct whose pointer does not
        // outlive the call.
        let read = unsafe {
            enc.raw_api().get_option(
                ENCODER_OPTION_SVC_ENCODE_PARAM_EXT,
                std::ptr::addr_of_mut!(params).cast(),
            )
        };
        assert_eq!(read, 0, "openh264 would not report its parameters");
        params.iMinQp = min;
        params.iMaxQp = max;
        #[allow(unsafe_code)]
        // SAFETY: as above; only the two quantiser fields differ from the
        // block just read.
        let wrote = unsafe {
            enc.raw_api().set_option(
                ENCODER_OPTION_SVC_ENCODE_PARAM_EXT,
                std::ptr::addr_of_mut!(params).cast(),
            )
        };
        assert_eq!(wrote, 0, "openh264 refused the parameters outright");
    }

    (1..=30)
        .map(|i| {
            enc.encode(&Screen::new(640, 480, i * 3))
                .expect("encode")
                .to_vec()
                .len()
        })
        .sum()
}

/// First: the measurement can see a quantiser change at all.
///
/// Without this the test below would pass on a broken instrument — which is
/// the way a negative result usually goes wrong.
#[test]
fn the_measurement_moves_when_the_range_really_changes() {
    let sharp = bytes((12, 40), None);
    let coarse = bytes((40, 51), None);
    assert!(
        coarse * 4 < sharp,
        "built sharp {sharp} bytes against built coarse {coarse}: the \
         measurement cannot see the quantiser, so it cannot testify that \
         pushing one had no effect"
    );
}

/// And now the claim: pushed at a running encoder, it does nothing.
#[test]
fn a_quantiser_pushed_at_a_running_encoder_is_ignored() {
    let baseline = bytes((12, 40), None);
    let coarser = bytes((12, 40), Some((40, 51)));
    let sharper = bytes((12, 40), Some((12, 24)));

    assert_eq!(
        (baseline, baseline),
        (coarser, sharper),
        "openh264 has started honouring a quantiser range on a running \
         encoder. That is good news: the fidelity rung can move in place too. \
         Update `quantiser_moves_in_place`, let `EncoderConfig::change_to` \
         treat a `qp` difference as `InPlace`, and carry the range in \
         `InPlace`."
    );
    assert!(
        !esper_encode_retune::quantiser_moves_in_place(),
        "the constant and the measurement disagree"
    );
}
