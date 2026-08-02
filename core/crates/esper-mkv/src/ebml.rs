//! EBML: the handful of primitives Matroska is built out of.
//!
//! Every element is an **id**, a **size**, and a payload — and both the id and
//! the size are variable-length integers whose first byte says how long they
//! are. That is the whole format at this level, which is why this file is
//! short and why writing a container by hand is a reasonable thing to do at
//! all.
//!
//! Only what a recording needs is here. There is no reader: what checks this
//! writer is a reader written independently in the tests, for the same reason
//! the PNG writer is checked by an inflater written from the RFC rather than
//! by its own code.

/// An element id, written as the bytes the specification gives.
///
/// **Ids are already length-marked** in the spec — `0xA3` is one byte,
/// `0x1F43B675` is four — so they go out verbatim rather than being encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Id(pub u32);

impl Id {
    /// The id's bytes, most significant first, with leading zeros dropped.
    pub fn write(self, out: &mut Vec<u8>) {
        let bytes = self.0.to_be_bytes();
        let first = bytes.iter().position(|b| *b != 0).unwrap_or(3);
        out.extend_from_slice(bytes.get(first..).unwrap_or(&[]));
    }
}

/// Write a size as a variable-length integer.
///
/// The first byte's leading zeros say how many bytes the whole thing takes: a
/// marker bit, then the value. Eight bytes covers any size a file can have, so
/// the only interesting case is the smallest encoding that fits — a size
/// written wider than it needs to be is legal, and writing it narrow keeps the
/// output the size a reader expects.
pub fn write_size(value: u64, out: &mut Vec<u8>) {
    for length in 1u32..=8 {
        let bits = 7 * length;
        // The all-ones value at each length means "unknown", so it cannot be
        // used for a real size.
        let most = (1u64 << bits) - 2;
        if value <= most {
            let marker = 1u64 << bits;
            let encoded = (marker | value).to_be_bytes();
            let start = 8 - length as usize;
            out.extend_from_slice(encoded.get(start..).unwrap_or(&[]));
            return;
        }
    }
    // Unreachable for any real file: eight bytes hold 2^56 - 2.
    out.extend_from_slice(&[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFE]);
}

/// The size that means "this element ends when its parent does".
///
/// **What makes a recording writable as it happens.** A Segment whose size is
/// not known until the session ends would otherwise have to be seeked back to
/// and patched — which a file being written to a pipe, or a process that is
/// killed, cannot do.
pub const UNKNOWN_SIZE: [u8; 8] = [0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];

/// An element with a payload already built.
pub fn element(id: Id, payload: &[u8], out: &mut Vec<u8>) {
    id.write(out);
    write_size(payload.len() as u64, out);
    out.extend_from_slice(payload);
}

/// An unsigned integer element, in the fewest bytes that hold it.
pub fn unsigned(id: Id, value: u64, out: &mut Vec<u8>) {
    let bytes = value.to_be_bytes();
    let first = bytes.iter().position(|b| *b != 0).unwrap_or(7);
    let trimmed = bytes.get(first..).unwrap_or(&[]);
    element(id, trimmed, out);
}

/// A string element.
pub fn text(id: Id, value: &str, out: &mut Vec<u8>) {
    element(id, value.as_bytes(), out);
}

/// A 64-bit float element.
///
/// Matroska allows four or eight bytes; eight is written because the values
/// that need it — a sampling frequency — are the ones where a reader
/// disagreeing with the writer about rounding is a stream that plays at the
/// wrong speed.
pub fn float(id: Id, value: f64, out: &mut Vec<u8>) {
    element(id, &value.to_be_bytes(), out);
}

/// The ids this writer uses, named as the specification names them.
pub mod id {
    use super::Id;

    pub const EBML: Id = Id(0x1A45_DFA3);
    pub const EBML_VERSION: Id = Id(0x4286);
    pub const EBML_READ_VERSION: Id = Id(0x42F7);
    pub const EBML_MAX_ID_LENGTH: Id = Id(0x42F2);
    pub const EBML_MAX_SIZE_LENGTH: Id = Id(0x42F3);
    pub const DOC_TYPE: Id = Id(0x4282);
    pub const DOC_TYPE_VERSION: Id = Id(0x4287);
    pub const DOC_TYPE_READ_VERSION: Id = Id(0x4285);

    pub const SEGMENT: Id = Id(0x1853_8067);
    pub const INFO: Id = Id(0x1549_A966);
    pub const TIMESTAMP_SCALE: Id = Id(0x002A_D7B1);
    pub const MUXING_APP: Id = Id(0x4D80);
    pub const WRITING_APP: Id = Id(0x5741);
    pub const DURATION: Id = Id(0x4489);

    pub const TRACKS: Id = Id(0x1654_AE6B);
    pub const TRACK_ENTRY: Id = Id(0xAE);
    pub const TRACK_NUMBER: Id = Id(0xD7);
    pub const TRACK_UID: Id = Id(0x73C5);
    pub const TRACK_TYPE: Id = Id(0x83);
    pub const CODEC_ID: Id = Id(0x86);
    pub const CODEC_PRIVATE: Id = Id(0x63A2);
    pub const VIDEO: Id = Id(0xE0);
    pub const PIXEL_WIDTH: Id = Id(0xB0);
    pub const PIXEL_HEIGHT: Id = Id(0xBA);
    pub const AUDIO: Id = Id(0xE1);
    pub const SAMPLING_FREQUENCY: Id = Id(0xB5);
    pub const CHANNELS: Id = Id(0x9F);

    pub const CLUSTER: Id = Id(0x1F43_B675);
    pub const TIMESTAMP: Id = Id(0xE7);
    pub const SIMPLE_BLOCK: Id = Id(0xA3);
}
