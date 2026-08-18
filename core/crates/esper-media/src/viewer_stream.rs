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
//! record  := kind u8 | length u32 | body       the length is the body's
//!   kind 1, configuration:
//!     width u32, height u32,
//!     codec  u16 length + that many UTF-8 bytes   ("avc1.4D4028")
//!     avcC   u32 length + that many bytes          (the description)
//!   kind 2, picture:
//!     flags  u8           bit 0 set for a picture a decoder can begin at
//!     time   u64          microseconds since the session opened
//!     handed u64          this machine's wall clock, in microseconds, when
//!                         the picture was written — 0 when unknown
//!     bytes  u32 length + that many bytes          (AVCC, length-prefixed NALs)
//! ```
//!
//! Everything is big-endian, which is what the rest of this protocol uses and
//! what `DataView` reads by default in the page.
//!
//! # Why every record carries its own length
//!
//! So a reader can skip a record it does not understand, and so a body can
//! grow a field without every reader being replaced on the same day. Without
//! it the stream is parseable only by something that already knows every
//! kind — which is fine on the day it is written and is exactly how a private
//! format becomes impossible to extend. The first version had no length; the
//! second added one *before* anything shipped it, which is the only cheap
//! time to make this mistake.
//!
//! # Why a picture carries two clocks
//!
//! `time` is the session's own clock and is what a decoder orders pictures
//! by; it starts at zero and means nothing outside the session. `handed` is
//! this machine's wall clock when the picture went down the socket, and it is
//! there to measure **the hop this path adds** — engine to window to drawn —
//! which is the part of the latency the viewer is responsible for and the
//! part nobody had measured.
//!
//! It is deliberately *not* a capture time and deliberately *not* comparable
//! across machines: the two ends of a session have two wall clocks, and a
//! number that had quietly become a clock-skew measurement would be worse
//! than no number. A reader that sees 0 has a writer that could not say.

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
///
/// Two: the first had records with no length, which made the stream readable
/// only by something that already knew every kind.
pub const STREAM_VERSION: u8 = 2;

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

    /// One record: its kind, the length of its body, and the body.
    fn record(&mut self, kind: u8, body: &[u8]) -> io::Result<()> {
        self.begin()?;
        let len = u32::try_from(body.len()).unwrap_or(u32::MAX);
        self.out.write_all(&[kind])?;
        self.out.write_all(&len.to_be_bytes())?;
        self.out
            .write_all(body.get(..len as usize).unwrap_or_default())?;
        self.out.flush()
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
        let codec = sets.codec();
        let description = sets.description();
        let mut body = Vec::with_capacity(14 + codec.len() + description.len());
        body.extend_from_slice(&width.to_be_bytes());
        body.extend_from_slice(&height.to_be_bytes());
        let codec_len = u16::try_from(codec.len()).unwrap_or(u16::MAX);
        body.extend_from_slice(&codec_len.to_be_bytes());
        body.extend_from_slice(
            codec
                .as_bytes()
                .get(..usize::from(codec_len))
                .unwrap_or_default(),
        );
        let d_len = u32::try_from(description.len()).unwrap_or(u32::MAX);
        body.extend_from_slice(&d_len.to_be_bytes());
        body.extend_from_slice(description.get(..d_len as usize).unwrap_or_default());
        self.record(record::CONFIGURATION, &body)
    }

    /// One picture, at a time in microseconds since the session opened.
    ///
    /// Flushed on every picture, deliberately: a buffered stream delivers the
    /// picture somebody is waiting for once the *next* one has been encoded,
    /// which is a frame of latency added by nothing but tidiness.
    ///
    /// The wall clock is read here rather than taken from the caller: the
    /// number means "when this went down the socket", and the only thing that
    /// knows that is the line writing it.
    pub fn picture(&mut self, sample: &Sample, at_micros: u64) -> io::Result<()> {
        let handed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_micros()).unwrap_or(0));
        let flags = if sample.keyframe { FLAG_KEYFRAME } else { 0 };
        let mut body = Vec::with_capacity(21 + sample.bytes.len());
        body.push(flags);
        body.extend_from_slice(&at_micros.to_be_bytes());
        body.extend_from_slice(&handed.to_be_bytes());
        let len = u32::try_from(sample.bytes.len()).unwrap_or(u32::MAX);
        body.extend_from_slice(&len.to_be_bytes());
        body.extend_from_slice(sample.bytes.get(..len as usize).unwrap_or_default());
        self.record(record::PICTURE, &body)
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

    /// A reader that knows only the framing, which is the point of the framing.
    ///
    /// It does not understand either record kind — it walks kinds and lengths
    /// — so anything it can do here, a reader of a *later* version could do
    /// with a record this one has never heard of.
    fn records(stream: &[u8]) -> Vec<(u8, Vec<u8>)> {
        assert_eq!(&stream[..8], MAGIC, "the stream names itself");
        assert_eq!(stream[8], STREAM_VERSION);
        let mut out = Vec::new();
        let mut at = 9;
        while at < stream.len() {
            let kind = stream[at];
            let len = u32::from_be_bytes(stream[at + 1..at + 5].try_into().unwrap()) as usize;
            out.push((kind, stream[at + 5..at + 5 + len].to_vec()));
            at += 5 + len;
        }
        assert_eq!(
            at,
            stream.len(),
            "a record whose length ran past the stream"
        );
        out
    }

    #[test]
    fn the_stream_begins_with_its_name_and_version() {
        let mut w = Writer::new(Vec::new());
        w.configuration(1280, 720, &sets()).unwrap();
        let out = w.into_inner();

        let all = records(&out);
        assert_eq!(all.len(), 1);
        let (kind, body) = &all[0];
        assert_eq!(*kind, record::CONFIGURATION);
        assert_eq!(u32::from_be_bytes(body[0..4].try_into().unwrap()), 1280);
        assert_eq!(u32::from_be_bytes(body[4..8].try_into().unwrap()), 720);
        let codec_len = usize::from(u16::from_be_bytes(body[8..10].try_into().unwrap()));
        assert_eq!(&body[10..10 + codec_len], b"avc1.4D4028");
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
        assert_eq!(records(&out).len(), 3);
    }

    #[test]
    fn a_picture_says_whether_a_decoder_can_begin_at_it_and_when_it_was_sent() {
        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_micros() as u64;
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

        let all = records(&out);
        let (kind, body) = &all[0];
        assert_eq!(*kind, record::PICTURE);
        assert_eq!(
            body[0] & FLAG_KEYFRAME,
            0,
            "a delta frame is not a starting point"
        );
        assert_eq!(
            u64::from_be_bytes(body[1..9].try_into().unwrap()),
            33_333,
            "the session's own clock, as the caller gave it"
        );
        let handed = u64::from_be_bytes(body[9..17].try_into().unwrap());
        assert!(
            handed >= before,
            "the wall clock is read when the picture is written, not before it"
        );
        assert_eq!(u32::from_be_bytes(body[17..21].try_into().unwrap()), 3);
        assert_eq!(&body[21..24], &[9, 9, 9]);
    }

    /// The reason every record carries a length: one that nothing understands
    /// can still be walked past.
    #[test]
    fn a_reader_can_skip_a_record_it_has_never_heard_of() {
        let mut w = Writer::new(Vec::new());
        w.configuration(64, 48, &sets()).unwrap();
        // A kind from a future version, in the middle of a stream.
        w.record(200, b"something a later version added").unwrap();
        w.picture(
            &Sample {
                bytes: vec![1, 2, 3, 4],
                keyframe: true,
            },
            10,
        )
        .unwrap();

        let out = w.into_inner();
        let kinds: Vec<u8> = records(&out).iter().map(|(k, _)| *k).collect();
        assert_eq!(
            kinds,
            vec![record::CONFIGURATION, 200, record::PICTURE],
            "the unknown record is walked past and the picture after it still arrives"
        );
    }
}
