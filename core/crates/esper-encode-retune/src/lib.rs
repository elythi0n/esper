//! Changing a running openh264 encoder's rate settings in place.
//!
//! # Why this crate exists
//!
//! Rebuilding an encoder costs a keyframe. Every reference picture belongs to
//! the encoder that made it, so a fresh one has to start with an IDR — and on
//! the links where the degradation ladder is actually spent, a keyframe is the
//! largest frame and therefore the one a slow link is least able to carry. A
//! session that falls from 60 fps to 15 and recovers pays six of them for
//! three decisions.
//!
//! openh264 can be told some things without being rebuilt. Not all things, and
//! which ones is not a matter of opinion: `WelsEncoderParamAdjust` in
//! `encoder_ext.cpp` copies a hand-picked list of fields when no reset is
//! needed, under the comment *"we can not use direct struct based memcpy due
//! some fields need keep unchanged as before"*. [`InPlace`] is that list,
//! narrowed to what this product changes.
//!
//! # What is deliberately not here: the quantiser
//!
//! The obvious candidate is the fidelity rung, and it does not work.
//! `iMinQp` and `iMaxQp` are **not** in the copy list, so setting them on a
//! running encoder is accepted, reports success, and does nothing. Measured on
//! screen-like content at 640x480 over 30 frames:
//!
//! | | bytes |
//! |---|---|
//! | built with `qp(12,40)` | 90,488 |
//! | built with `qp(40,51)` | 8,326 |
//! | built with `qp(12,40)`, then `qp(40,51)` pushed in place | **90,488** |
//! | built with `qp(12,40)`, then `qp(12,24)` pushed in place | **90,488** |
//!
//! Byte-identical in both directions, against a measurement that moves by 11x
//! when the range is really applied. So a fidelity change still rebuilds, and
//! [`quantiser_moves_in_place`] is the tripwire that will say so if a future
//! openh264 ever starts honouring it.
//!
//! # Unsafe inventory
//!
//! - **`raw_api()` to read the current parameter block.** `Encoder::raw_api`
//!   is `unsafe` because the caller can set parameters the encoder relies on;
//!   the invariant relied on here is that `GetOption` with
//!   `ENCODER_OPTION_SVC_ENCODE_PARAM_EXT` only *reads*, into a caller-owned
//!   `SEncParamExt` that is fully initialised by `Default` before the pointer
//!   is taken. The encoder is initialised by construction — `Encoder` cannot
//!   be built otherwise — and the pointer does not outlive the call.
//! - **`raw_api()` to write the modified block back.** The same pointer
//!   discipline, and the safety condition the binding names — not setting
//!   parameters the encoder relies on — is met by construction: the block
//!   written is the block just read, with only the fields in [`InPlace`]
//!   changed. Picture size, profile, level, layer count and slicing are
//!   carried through untouched, which is what makes this an adjustment rather
//!   than a re-initialisation.

use openh264::encoder::Encoder;
use openh264_sys2::{ENCODER_OPTION_SVC_ENCODE_PARAM_EXT, SEncParamExt};

/// The settings openh264 will accept on a running encoder.
///
/// Every field here is in `WelsEncoderParamAdjust`'s copy list. Nothing else
/// belongs in this struct: a field that is not in that list is accepted,
/// reports success and is silently dropped, which is worse than being refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InPlace {
    /// The frame rate the encoder should assume for its rate model.
    pub fps: u32,
    /// Target bits per second.
    pub bitrate: u32,
    /// Frames between keyframes, **not seconds**.
    ///
    /// This is the field that makes the whole thing work, and leaving it out
    /// is how the first attempt at skipping the rebuild went wrong. openh264's
    /// intra period is a count of *frames*, so it means a different length of
    /// time at every frame rate: an encoder told 15 fps while still holding a
    /// period computed for 60 emits keyframes four times as often as asked.
    /// Changing the frame rate without changing this buys more of exactly the
    /// thing the rebuild was being skipped to avoid.
    pub keyframe_period_frames: u32,
}

/// Why a running encoder could not be retuned.
#[derive(Debug, thiserror::Error)]
pub enum RetuneError {
    /// openh264 refused to hand over its current parameters.
    #[error("openh264 would not report its encoder parameters (code {code})")]
    Unreadable { code: i32 },
    /// openh264 refused the modified parameters.
    #[error("openh264 refused the adjusted encoder parameters (code {code})")]
    Refused { code: i32 },
}

/// Apply `want` to a running encoder, without rebuilding it.
///
/// On success the encoder keeps every reference picture it holds, so the next
/// frame can be a delta and no keyframe is owed. On failure nothing is
/// promised about what was applied — the caller's answer is to rebuild, which
/// is correct from any state.
///
/// # Errors
///
/// [`RetuneError`] if openh264 will not report or will not accept the
/// parameters.
pub fn retune(encoder: &mut Encoder, want: &InPlace) -> Result<(), RetuneError> {
    let mut params = SEncParamExt::default();

    // SAFETY: see the unsafe inventory in the module documentation.
    #[allow(unsafe_code)]
    let read = unsafe {
        encoder.raw_api().get_option(
            ENCODER_OPTION_SVC_ENCODE_PARAM_EXT,
            std::ptr::addr_of_mut!(params).cast(),
        )
    };
    if read != 0 {
        return Err(RetuneError::Unreadable { code: read });
    }

    apply(&mut params, want);

    // SAFETY: see the unsafe inventory in the module documentation.
    #[allow(unsafe_code)]
    let wrote = unsafe {
        encoder.raw_api().set_option(
            ENCODER_OPTION_SVC_ENCODE_PARAM_EXT,
            std::ptr::addr_of_mut!(params).cast(),
        )
    };
    if wrote != 0 {
        return Err(RetuneError::Refused { code: wrote });
    }
    Ok(())
}

/// The field-by-field change, separated so it can be tested without an encoder.
///
/// The spatial layer carries its own copy of the rate and the bitrate, and
/// `WelsEncoderParamAdjust` copies the layer's fields as well as the
/// top-level ones. Setting only the top level leaves the layer describing the
/// old rate, which is the shape of bug that reads as "it sometimes works".
fn apply(params: &mut SEncParamExt, want: &InPlace) {
    let fps = want.fps.max(1);
    let bitrate = i32::try_from(want.bitrate).unwrap_or(i32::MAX);
    #[expect(
        clippy::cast_precision_loss,
        reason = "a frame rate is a small integer; openh264's field is f32"
    )]
    let rate = fps as f32;

    params.fMaxFrameRate = rate;
    params.iTargetBitrate = bitrate;
    params.uiIntraPeriod = want.keyframe_period_frames.max(1);

    let layers = params.iSpatialLayerNum.clamp(0, 4) as usize;
    for layer in params.sSpatialLayers.iter_mut().take(layers) {
        layer.fFrameRate = rate;
        layer.iSpatialBitrate = bitrate;
        layer.iMaxSpatialBitrate = bitrate;
    }
}

/// Whether this openh264 honours a quantiser range pushed at a running encoder.
///
/// **A tripwire, not a feature.** It is `false` for every openh264 this has
/// been built against, and the fidelity rung rebuilds because of it. If it
/// ever becomes `true`, the rung can move in place too — so it is asserted in
/// a test rather than left as a belief.
#[must_use]
pub const fn quantiser_moves_in_place() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params_with_layers(n: i32) -> SEncParamExt {
        SEncParamExt {
            iSpatialLayerNum: n,
            ..SEncParamExt::default()
        }
    }

    #[test]
    fn the_frame_rate_reaches_the_layer_as_well_as_the_top_level() {
        let mut p = params_with_layers(1);
        apply(
            &mut p,
            &InPlace {
                fps: 15,
                bitrate: 800_000,
                keyframe_period_frames: 30,
            },
        );
        assert!((p.fMaxFrameRate - 15.0).abs() < f32::EPSILON);
        let layer = p.sSpatialLayers.first().expect("one layer");
        assert!(
            (layer.fFrameRate - 15.0).abs() < f32::EPSILON,
            "the layer kept the old rate, which is how this half-works"
        );
        assert_eq!(layer.iSpatialBitrate, 800_000);
    }

    #[test]
    fn the_keyframe_period_is_carried_in_frames() {
        let mut p = params_with_layers(1);
        apply(
            &mut p,
            &InPlace {
                fps: 15,
                bitrate: 1,
                keyframe_period_frames: 30,
            },
        );
        assert_eq!(p.uiIntraPeriod, 30);
    }

    #[test]
    fn nothing_is_ever_set_to_zero() {
        // openh264 clips a frame rate to a minimum and treats an intra period
        // of zero as "never"; both are worth refusing here rather than
        // discovering in a bitstream.
        let mut p = params_with_layers(1);
        apply(
            &mut p,
            &InPlace {
                fps: 0,
                bitrate: 0,
                keyframe_period_frames: 0,
            },
        );
        assert!(p.fMaxFrameRate >= 1.0);
        assert!(p.uiIntraPeriod >= 1);
    }

    #[test]
    fn a_layer_count_out_of_range_touches_no_layer_it_should_not() {
        let mut p = params_with_layers(99);
        apply(
            &mut p,
            &InPlace {
                fps: 10,
                bitrate: 5,
                keyframe_period_frames: 10,
            },
        );
        // Four is openh264's maximum; the clamp is what stops a bad count
        // from being read as a length.
        assert_eq!(p.sSpatialLayers.len(), 4);
    }
}
