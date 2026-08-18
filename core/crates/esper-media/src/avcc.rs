//! Repackaging Annex-B into AVCC, which is the only form a webview decodes.
//!
//! # Why this exists
//!
//! The viewer is a `VideoDecoder` in the window, and it was measured before it
//! was built: this encoder's own output decodes at 151 frames a second at
//! 1920x1080 with every frame drawn, on software rendering with no GPU. The
//! same measurement found the constraint this module exists for — **WebKit
//! refuses Annex-B outright**, `EncodingError` on the first chunk and zero
//! frames, while accepting exactly the same pictures when each access unit
//! arrives as length-prefixed NAL units with the parameter sets handed over
//! separately as an `avcC` description. Chromium takes either form, so one
//! repackaging serves the desktop window and a browser viewer.
//!
//! This is the whole of that repackaging. It is here rather than in the page
//! because the engine already walks every NAL unit to packetize, because Rust
//! can be tested without a browser, and because the page is the one part of
//! this product that cannot be trusted to be fast.
//!
//! # What it does not do
//!
//! It does not parse the bitstream. The NAL units are copied verbatim,
//! emulation-prevention bytes and all — a decoder removes those, and a
//! repackager that rewrote payloads would be a second encoder. The only bytes
//! read are the four at the front of the SPS, which are the profile and level
//! the `avcC` header repeats and the codec string is derived from.

use crate::annexb::{NalUnits, nal};

/// The length prefix AVCC samples use here, in bytes.
///
/// Four, and fixed. The format allows one, two or four; two would cap a NAL
/// unit at 64 KiB, which a keyframe exceeds at 1080p, and one is for
/// thumbnails. A fixed four also means `lengthSizeMinusOne` is a constant
/// rather than a thing a reader has to agree with.
pub const LENGTH_PREFIX: usize = 4;

/// The parameter sets a decoder needs before its first picture.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParameterSets {
    /// The sequence parameter set, without a start code.
    pub sps: Vec<u8>,
    /// The picture parameter set, without a start code.
    pub pps: Vec<u8>,
}

impl ParameterSets {
    /// Whether both sets are present, which is what makes a description.
    #[must_use]
    pub fn complete(&self) -> bool {
        !self.sps.is_empty() && !self.pps.is_empty()
    }

    /// The `avcC` configuration record these sets describe.
    ///
    /// The layout is `ISO/IEC 14496-15` §5.2.4.1: a version, the three profile
    /// and level bytes copied straight out of the SPS, then the sets with
    /// their lengths. The top six bits of the two count bytes are reserved and
    /// specified as ones, which is what `0xFF` and `0xE1` are.
    #[must_use]
    pub fn description(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(11 + self.sps.len() + self.pps.len());
        out.push(1);
        // The profile, the constraint flags and the level, from the SPS
        // itself. Not from what the encoder was configured with: the record
        // has to describe the stream, and a configuration is an intention.
        out.push(self.sps.get(1).copied().unwrap_or(0));
        out.push(self.sps.get(2).copied().unwrap_or(0));
        out.push(self.sps.get(3).copied().unwrap_or(0));
        #[expect(
            clippy::cast_possible_truncation,
            reason = "lengthSizeMinusOne is two bits; LENGTH_PREFIX is 4 by construction"
        )]
        out.push(0xFC | (LENGTH_PREFIX as u8 - 1));
        out.push(0xE0 | 1);
        push_with_length(&mut out, &self.sps);
        out.push(1);
        push_with_length(&mut out, &self.pps);
        out
    }

    /// The codec string a decoder is configured with, such as `avc1.4D4028`.
    ///
    /// Derived from the SPS for the same reason the description is: the
    /// encoder's ladder can move the level, and a decoder configured for a
    /// level below the stream's is a decoder that fails on a picture that is
    /// larger than it was promised.
    #[must_use]
    pub fn codec(&self) -> String {
        let mut out = String::from("avc1.");
        for at in 1..4 {
            out.push_str(&format!("{:02X}", self.sps.get(at).copied().unwrap_or(0)));
        }
        out
    }
}

fn push_with_length(out: &mut Vec<u8>, unit: &[u8]) {
    let len = u16::try_from(unit.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(unit.get(..usize::from(len)).unwrap_or_default());
}

/// One access unit, repackaged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sample {
    /// The NAL units, each behind a four-byte big-endian length.
    pub bytes: Vec<u8>,
    /// Whether a decoder can begin at this sample.
    pub keyframe: bool,
}

/// Annex-B access units in, AVCC samples out, remembering the parameter sets.
///
/// # Why the parameter sets are held rather than copied into every sample
///
/// Because a `VideoDecoder` is *configured* with them once and then fed
/// samples, and a sample that repeated them would be handing the decoder a
/// second copy of a thing it was already told. The encoder emits them before
/// every keyframe, so they arrive repeatedly whether anybody wants them or
/// not; this keeps the latest and tells the caller when they changed, which is
/// the moment — and the only moment — a decoder has to be reconfigured.
#[derive(Debug, Clone, Default)]
pub struct Repackager {
    sets: ParameterSets,
    /// Whether the sets changed since the caller last asked.
    fresh: bool,
}

impl Repackager {
    /// A repackager that has seen nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The parameter sets as they stand, complete or not.
    #[must_use]
    pub fn sets(&self) -> &ParameterSets {
        &self.sets
    }

    /// Whether a decoder needs configuring — because the sets arrived, or
    /// because they changed. Reading it clears it.
    pub fn take_description_change(&mut self) -> bool {
        std::mem::take(&mut self.fresh)
    }

    /// Repackage one access unit.
    ///
    /// `None` when there is nothing for a decoder: an access unit that carried
    /// only parameter sets, or one that arrived before any were seen. The
    /// second is the ordinary case at the start of a session — a viewer
    /// joining mid-stream waits for the next keyframe, exactly as the recorder
    /// does, because a decoder handed a delta frame with no reference shows
    /// either nothing or garbage.
    pub fn push(&mut self, access_unit: &[u8]) -> Option<Sample> {
        let mut bytes = Vec::with_capacity(access_unit.len() + LENGTH_PREFIX * 4);
        let mut keyframe = false;

        for unit in NalUnits::new(access_unit) {
            match unit.kind() {
                nal::SPS => {
                    if self.sets.sps != unit.bytes {
                        self.sets.sps = unit.bytes.to_vec();
                        self.fresh = true;
                    }
                }
                nal::PPS => {
                    if self.sets.pps != unit.bytes {
                        self.sets.pps = unit.bytes.to_vec();
                        self.fresh = true;
                    }
                }
                kind => {
                    // Everything else travels: slices because they are the
                    // picture, SEI because a decoder may act on it and
                    // dropping it silently changes what the stream said.
                    if kind == nal::IDR {
                        keyframe = true;
                    }
                    let len = u32::try_from(unit.bytes.len()).unwrap_or(u32::MAX);
                    bytes.extend_from_slice(&len.to_be_bytes());
                    bytes.extend_from_slice(unit.bytes.get(..len as usize).unwrap_or_default());
                }
            }
        }

        if bytes.is_empty() || !self.sets.complete() {
            return None;
        }
        Some(Sample { bytes, keyframe })
    }
}

#[cfg(test)]
#[expect(
    clippy::indexing_slicing,
    reason = "a test that indexes past its own fixture is a failing test, which is the point"
)]
mod tests {
    use super::*;

    /// An Annex-B access unit from NAL units, with four-byte start codes.
    fn annexb(units: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for u in units {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(u);
        }
        out
    }

    /// A plausible SPS: the header byte, then profile, constraints, level.
    fn sps(level: u8) -> Vec<u8> {
        vec![0x67, 0x4D, 0x40, level, 0x1F, 0xAB]
    }
    fn pps() -> Vec<u8> {
        vec![0x68, 0xEE, 0x3C, 0x80]
    }
    fn idr() -> Vec<u8> {
        vec![0x65, 0x88, 0x84, 0x00, 0x33]
    }
    fn slice() -> Vec<u8> {
        vec![0x41, 0x9A, 0x00, 0x11]
    }

    #[test]
    fn the_description_says_what_the_stream_is() {
        let mut r = Repackager::new();
        r.push(&annexb(&[&sps(0x28), &pps(), &idr()])).unwrap();
        let d = r.sets().description();

        assert_eq!(d.first(), Some(&1), "configurationVersion");
        assert_eq!(
            &d[1..4],
            &[0x4D, 0x40, 0x28],
            "profile, constraints, level from the SPS"
        );
        assert_eq!(d[4], 0xFF, "reserved ones and lengthSizeMinusOne = 3");
        assert_eq!(d[5], 0xE1, "reserved ones and one SPS");
        let sps_len = usize::from(u16::from_be_bytes([d[6], d[7]]));
        assert_eq!(sps_len, sps(0x28).len());
        assert_eq!(
            &d[8..8 + sps_len],
            sps(0x28).as_slice(),
            "the SPS, verbatim"
        );
        let at = 8 + sps_len;
        assert_eq!(d[at], 1, "one PPS");
        let pps_len = usize::from(u16::from_be_bytes([d[at + 1], d[at + 2]]));
        assert_eq!(
            &d[at + 3..at + 3 + pps_len],
            pps().as_slice(),
            "the PPS, verbatim"
        );
    }

    #[test]
    fn the_codec_string_comes_from_the_stream() {
        let mut r = Repackager::new();
        r.push(&annexb(&[&sps(0x28), &pps(), &idr()]));
        assert_eq!(r.sets().codec(), "avc1.4D4028");

        // A ladder that moves the level moves the string with it, which is the
        // whole reason this is derived rather than written down.
        let mut r = Repackager::new();
        r.push(&annexb(&[&sps(0x1F), &pps(), &idr()]));
        assert_eq!(r.sets().codec(), "avc1.4D401F");
    }

    #[test]
    fn parameter_sets_do_not_travel_in_the_sample() {
        let mut r = Repackager::new();
        let sample = r.push(&annexb(&[&sps(0x28), &pps(), &idr()])).unwrap();
        assert!(sample.keyframe);
        // One unit: the IDR, behind its length. Not three.
        assert_eq!(
            sample.bytes.len(),
            LENGTH_PREFIX + idr().len(),
            "the SPS and PPS belong to the description, not to every keyframe"
        );
        assert_eq!(
            &sample.bytes[..4],
            &u32::try_from(idr().len()).unwrap().to_be_bytes()
        );
        assert_eq!(&sample.bytes[4..], idr().as_slice());
    }

    #[test]
    fn nothing_decodable_before_the_parameter_sets() {
        let mut r = Repackager::new();
        // A viewer joining mid-stream sees deltas first, and a decoder handed
        // one with no reference shows garbage or nothing.
        assert_eq!(r.push(&annexb(&[&slice()])), None);
        assert_eq!(
            r.push(&annexb(&[&sps(0x28)])),
            None,
            "sets alone are not a picture"
        );
        assert_eq!(r.push(&annexb(&[&pps()])), None);
        assert!(
            r.push(&annexb(&[&slice()])).is_some(),
            "and then it can begin"
        );
    }

    #[test]
    fn a_change_of_parameter_sets_is_reported_once() {
        let mut r = Repackager::new();
        r.push(&annexb(&[&sps(0x28), &pps(), &idr()]));
        assert!(
            r.take_description_change(),
            "the first sets are a change from nothing"
        );
        assert!(!r.take_description_change(), "and reading it clears it");

        r.push(&annexb(&[&sps(0x28), &pps(), &idr()]));
        assert!(
            !r.take_description_change(),
            "the encoder repeats the sets before every keyframe; repeating is not changing"
        );

        // A monitor switch, or a rung that changed the size, brings a new SPS.
        r.push(&annexb(&[&sps(0x1F), &pps(), &idr()]));
        assert!(
            r.take_description_change(),
            "a different SPS has to reconfigure the decoder"
        );
    }

    #[test]
    fn every_other_unit_travels() {
        let mut r = Repackager::new();
        r.push(&annexb(&[&sps(0x28), &pps(), &idr()]));
        let sei: &[u8] = &[0x06, 0x05, 0x01, 0x00, 0x80];
        let sample = r.push(&annexb(&[sei, &slice()])).unwrap();
        assert!(!sample.keyframe);
        assert_eq!(
            sample.bytes.len(),
            LENGTH_PREFIX * 2 + sei.len() + slice().len(),
            "SEI is part of what the stream said and is not this module's to drop"
        );
    }
}
