//! The byte stream a window's decoder reads, and the only thing between them.
//!
//! # Why a format at all
//!
//! The viewer decodes in the page, so something has to carry encoded pictures
//! from the engine to a `VideoDecoder`. Not RTP: the page would have to
//! reassemble it, and the reason the loopback hop exists is that the engine
//! has *already* reassembled it. Not a media container either — a container is
//! for a file somebody seeks in, and this is a stream nobody rewinds.
//!
//! So: a magic number, a version, and a sequence of records. Two record kinds,
//! and the second one is the reason this is not a fixed header —
//! **configuration can change mid-session**. A monitor switch, or a rung that
//! moves the picture size, brings new parameter sets, and a decoder has to be
//! reconfigured at exactly that point rather than told once at the start.
//!
//! # Why the page does not parse H.264
//!
//! Because it does not have to. Every record says its own length, so the
//! reader is a loop over lengths — no start-code scanning, no NAL types, no
//! bitstream at all. The one place that understands H.264 is [`crate::avcc`],
//! in Rust, where it is tested against a decoder.
//!
//! # The format
//!
//! ```text
//! stream  := magic version records
//! magic   := "ESPRVID\0"                       8 bytes
//! version := u8                                1 byte, STREAM_VERSION
//! record  := kind u8 | body
//!   kind 1, configuration:
//!     width u32, height u32,
//!     codec  u16 length + that many UTF-8 bytes   ("avc1.4D4028")
//!     avcC   u32 length + that many bytes          (the description)
//!   kind 2, picture:
//!     flags u8            bit 0 set for a picture a decoder can begin at
//!     time  u64           microseconds since the session opened
//!     bytes u32 length + that many bytes           (AVCC, length-prefixed NALs)
//! ```
//!
//! Everything is big-endian, which is what the rest of this protocol uses and
//! what `DataView` reads by default in the page.

use std::io::{self, Write};

use crate::avcc::{ParameterSets, Sample};

/// The first eight bytes of the stream.
///
/// A magic number on a loopback socket looks like superstition until the day
/// something else answers on that port: the page then fails on the first
/// record with "this is not a video stream" instead of handing a decoder
/// somebody's HTTP response.
pub const MAGIC: &[u8; 8] = b"ESPRVID\0";

/// The format version. A reader refuses anything else rather than guessing.
pub const STREAM_VERSION: u8 = 1;

/// Record kinds.
pub mod record {
    /// Size, codec string and `avcC` description: configure the decoder.
    pub const CONFIGURATION: u8 = 1;
    /// One access unit for the decoder.
    pub const PICTURE: u8 = 2;
}

/// Set on a picture a decoder can begin at.
pub const FLAG_KEYFRAME: u8 = 0b0000_0001;

/// Writes the stream. Owns no socket: a `Write` is a socket, a file, or a
/// test's buffer, and the difference has never been this module's business.
#[derive(Debug)]
pub struct Writer<W: Write> {
    out: W,
    started: bool,
}

impl<W: Write> Writer<W> {
    /// A writer that has not yet written its magic.
    pub const fn new(out: W) -> Self {
        Self {
            out,
            started: false,
        }
    }

    /// The magic and the version, written once, before anything else.
    fn begin(&mut self) -> io::Result<()> {
        if !self.started {
            self.out.write_all(MAGIC)?;
            self.out.write_all(&[STREAM_VERSION])?;
            self.started = true;
        }
        Ok(())
    }

    /// Configure the decoder: the picture size, the codec, the description.
    ///
    /// Written before the first picture and again whenever the parameter sets
    /// change — see [`crate::avcc::Repackager::take_description_change`].
    pub fn configuration(
        &mut self,
        width: u32,
        height: u32,
        sets: &ParameterSets,
    ) -> io::Result<()> {
        self.begin()?;
        let codec = sets.codec();
        let description = sets.description();
        self.out.write_all(&[record::CONFIGURATION])?;
        self.out.write_all(&width.to_be_bytes())?;
        self.out.write_all(&height.to_be_bytes())?;
        let codec_len = u16::try_from(codec.len()).unwrap_or(u16::MAX);
        self.out.write_all(&codec_len.to_be_bytes())?;
        self.out.write_all(
            codec
                .as_bytes()
                .get(..usize::from(codec_len))
                .unwrap_or_default(),
        )?;
        let d_len = u32::try_from(description.len()).unwrap_or(u32::MAX);
        self.out.write_all(&d_len.to_be_bytes())?;
        self.out
            .write_all(description.get(..d_len as usize).unwrap_or_default())?;
        self.out.flush()
    }

    /// One picture, at a time in microseconds since the session opened.
    ///
    /// Flushed on every picture, deliberately: a buffered stream delivers the
    /// picture somebody is waiting for once the *next* one has been encoded,
    /// which is a frame of latency added by nothing but tidiness.
    pub fn picture(&mut self, sample: &Sample, at_micros: u64) -> io::Result<()> {
        self.begin()?;
        let flags = if sample.keyframe { FLAG_KEYFRAME } else { 0 };
        self.out.write_all(&[record::PICTURE, flags])?;
        self.out.write_all(&at_micros.to_be_bytes())?;
        let len = u32::try_from(sample.bytes.len()).unwrap_or(u32::MAX);
        self.out.write_all(&len.to_be_bytes())?;
        self.out
            .write_all(sample.bytes.get(..len as usize).unwrap_or_default())?;
        self.out.flush()
    }

    /// The writer's sink, for a caller that has to close it.
    pub fn into_inner(self) -> W {
        self.out
    }
}

#[cfg(test)]
#[expect(
    clippy::indexing_slicing,
    reason = "a test that indexes past its own fixture is a failing test, which is the point"
)]
mod tests {
    use super::*;

    fn sets() -> ParameterSets {
        ParameterSets {
            sps: vec![0x67, 0x4D, 0x40, 0x28, 0x1F],
            pps: vec![0x68, 0xEE, 0x3C],
        }
    }

    #[test]
    fn the_stream_begins_with_its_name_and_version() {
        let mut w = Writer::new(Vec::new());
        w.configuration(1280, 720, &sets()).unwrap();
        let out = w.into_inner();
        assert_eq!(&out[..8], MAGIC);
        assert_eq!(out[8], STREAM_VERSION);
        assert_eq!(out[9], record::CONFIGURATION);
        assert_eq!(u32::from_be_bytes(out[10..14].try_into().unwrap()), 1280);
        assert_eq!(u32::from_be_bytes(out[14..18].try_into().unwrap()), 720);
    }

    #[test]
    fn the_magic_is_written_once_however_many_records_follow() {
        let mut w = Writer::new(Vec::new());
        w.configuration(64, 48, &sets()).unwrap();
        w.picture(
            &Sample {
                bytes: vec![0, 0, 0, 1, 0x65],
                keyframe: true,
            },
            0,
        )
        .unwrap();
        w.configuration(64, 48, &sets()).unwrap();
        let out = w.into_inner();
        assert_eq!(out.windows(8).filter(|w| *w == MAGIC.as_slice()).count(), 1);
    }

    #[test]
    fn a_picture_says_whether_a_decoder_can_begin_at_it() {
        let mut w = Writer::new(Vec::new());
        w.configuration(64, 48, &sets()).unwrap();
        let at = w.into_inner().len();
        let mut w = Writer::new(Vec::new());
        w.picture(
            &Sample {
                bytes: vec![9, 9, 9],
                keyframe: false,
            },
            33_333,
        )
        .unwrap();
        let out = w.into_inner();
        // Magic and version, then the record.
        assert_eq!(out[9], record::PICTURE);
        assert_eq!(
            out[10] & FLAG_KEYFRAME,
            0,
            "a delta frame is not a starting point"
        );
        assert_eq!(u64::from_be_bytes(out[11..19].try_into().unwrap()), 33_333);
        assert_eq!(u32::from_be_bytes(out[19..23].try_into().unwrap()), 3);
        assert_eq!(&out[23..26], &[9, 9, 9]);
        assert!(at > 9);
    }
}
