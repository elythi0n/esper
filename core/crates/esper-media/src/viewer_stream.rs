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
//!   kind 3, session:
//!     profile   u8         0 balanced, 1 text, 2 motion
//!     sharpness u8         0 none stated, 1 sharp, 2 balanced, 3 soft
//!     showing   u8 count + that many u32 monitor ids
//!     monitors  u8 count, then per monitor:
//!       id u32, x i32, y i32, width u32, height u32,
//!       flags u8           bit 0 set for the host's primary screen
//!       name  u16 length + that many UTF-8 bytes
//!   kind 4, pointer:
//!     flags u8             bit 0 the pointer is in the picture
//!                          bit 1 draw it — the host's pointer is not under
//!                                this person's hand, so the window has to
//!                                show where it is
//!     x i32, y i32         where it is, in picture pixels
//!   kind 5, pointer shape:
//!     flags u8             bit 0 there is an image; 0 means hidden
//!     hot_x u16, hot_y u16
//!     width u16, height u16
//!     rgba  u32 length + that many bytes    straight RGBA, not premultiplied
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
//! # Why a window is told what the session *could* be
//!
//! Because a window that cannot offer the other screens is a window that can
//! only ever show the one the host happened to start with. The host already
//! says what it has — it has to, since pointer positions are a fraction of a
//! named monitor — and the controller already had the list. It simply never
//! passed it on, so the switcher in the window's own chrome was a button that
//! could not be pressed.
//!
//! It carries the quality the session is asking for on the same record and
//! for the same reason: a control that shows a state it cannot read is a
//! control that lies the first time anything else changes it.
//!
//! # Why the pointer is two records and not one
//!
//! The same reason it is two messages on the wire: a pointer that moves must
//! not cost a shape. A shape is kilobytes and changes when somebody crosses
//! an edge; a position is nine bytes and changes at the speed of a hand.
//!
//! **And why the window is told to draw it.** A remote pointer is best *worn*
//! — the browser draws it at the person's own hand, with no network in the
//! path at all — but that is only honest while the local pointer is the
//! host's pointer. When it is not, the host's own position has to be drawn.
//! Which of the two it is comes down to whether this session is driving and
//! whether the host is following, and the engine has worked that out already:
//! sending the answer rather than the evidence keeps the window from being a
//! second implementation of a rule that took a finding to get right.
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
    /// What the session is of, and what else it could be of.
    pub const SESSION: u8 = 3;
    /// Where the host's pointer is, and whether to draw it.
    pub const POINTER: u8 = 4;
    /// What the host's pointer looks like. Sent on change, never on a move.
    pub const POINTER_SHAPE: u8 = 5;
}

/// Set on a picture a decoder can begin at.
pub const FLAG_KEYFRAME: u8 = 0b0000_0001;

/// Set on the screen the host calls its primary one.
pub const FLAG_PRIMARY: u8 = 0b0000_0001;

/// Set when the pointer is somewhere in the picture at all.
pub const FLAG_IN_PICTURE: u8 = 0b0000_0001;
/// Set when the window has to draw the pointer rather than wear it.
pub const FLAG_DRAW_POINTER: u8 = 0b0000_0010;
/// Set on a shape that has an image. Clear means the pointer is hidden.
pub const FLAG_HAS_IMAGE: u8 = 0b0000_0001;

/// One of the host's screens, as a window needs to know it.
///
/// Deliberately not `esper_proto::MonitorDescription`: this crate does not
/// depend on the protocol crate, and a window needs a name to show and an id
/// to ask for rather than everything the protocol carries. The scale is left
/// out for that reason — a window offering a list of screens has no use for
/// the host's backing-store factor, and a field nothing reads is a field that
/// goes wrong unnoticed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    /// The host's id for it, which a window echoes back and never interprets.
    pub id: u32,
    /// What the host calls it.
    pub name: String,
    /// Where it sits in the host's desktop. Can be negative.
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// Whether the host calls this its primary display.
    pub primary: bool,
}

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

    /// What the session is of, and what else it could be of.
    ///
    /// Written when the host first says what it has and again whenever that
    /// changes — a screen plugged in, a screen switched to. A window that was
    /// told once would go on offering a monitor somebody unplugged.
    pub fn session(
        &mut self,
        profile: u8,
        sharpness: u8,
        showing: &[u32],
        screens: &[Screen],
    ) -> io::Result<()> {
        let mut body = Vec::with_capacity(16 + showing.len() * 4 + screens.len() * 32);
        body.push(profile);
        body.push(sharpness);
        body.push(u8::try_from(showing.len()).unwrap_or(u8::MAX));
        for id in showing.iter().take(usize::from(u8::MAX)) {
            body.extend_from_slice(&id.to_be_bytes());
        }
        body.push(u8::try_from(screens.len()).unwrap_or(u8::MAX));
        for screen in screens.iter().take(usize::from(u8::MAX)) {
            body.extend_from_slice(&screen.id.to_be_bytes());
            body.extend_from_slice(&screen.x.to_be_bytes());
            body.extend_from_slice(&screen.y.to_be_bytes());
            body.extend_from_slice(&screen.width.to_be_bytes());
            body.extend_from_slice(&screen.height.to_be_bytes());
            body.push(if screen.primary { FLAG_PRIMARY } else { 0 });
            let name = screen.name.as_bytes();
            let len = u16::try_from(name.len()).unwrap_or(u16::MAX);
            body.extend_from_slice(&len.to_be_bytes());
            body.extend_from_slice(name.get(..usize::from(len)).unwrap_or_default());
        }
        self.record(record::SESSION, &body)
    }

    /// Where the host's pointer is, in picture pixels.
    ///
    /// Nine bytes, because this is the record that moves: a hand crossing a
    /// screen produces one of these a hundred times a second and must never
    /// cost a shape.
    pub fn pointer(&mut self, in_picture: bool, draw: bool, x: i32, y: i32) -> io::Result<()> {
        let mut body = Vec::with_capacity(9);
        let mut flags = 0;
        if in_picture {
            flags |= FLAG_IN_PICTURE;
        }
        if draw {
            flags |= FLAG_DRAW_POINTER;
        }
        body.push(flags);
        body.extend_from_slice(&x.to_be_bytes());
        body.extend_from_slice(&y.to_be_bytes());
        self.record(record::POINTER, &body)
    }

    /// What the host's pointer looks like, or that there is not one.
    ///
    /// `None` is hidden, which is a state rather than four kilobytes of
    /// transparent pixels — and on X11 an invisible cursor *is* how an
    /// application hides one, so it is what the host reads it from.
    pub fn pointer_shape(&mut self, image: Option<(u16, u16, u16, u16, &[u8])>) -> io::Result<()> {
        let Some((width, height, hot_x, hot_y, rgba)) = image else {
            return self.record(
                record::POINTER_SHAPE,
                &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            );
        };
        let mut body = Vec::with_capacity(13 + rgba.len());
        body.push(FLAG_HAS_IMAGE);
        body.extend_from_slice(&hot_x.to_be_bytes());
        body.extend_from_slice(&hot_y.to_be_bytes());
        body.extend_from_slice(&width.to_be_bytes());
        body.extend_from_slice(&height.to_be_bytes());
        let len = u32::try_from(rgba.len()).unwrap_or(u32::MAX);
        body.extend_from_slice(&len.to_be_bytes());
        body.extend_from_slice(rgba.get(..len as usize).unwrap_or_default());
        self.record(record::POINTER_SHAPE, &body)
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

    /// What a window is told about the session, read back out of the bytes.
    ///
    /// Read by hand rather than by a reader this module owns, because the
    /// reader that matters is in TypeScript: what is asserted here is the
    /// shape the page is written against, field by field, in the order they
    /// go down the socket.
    #[test]
    fn a_window_is_told_what_the_session_is_of() {
        let mut w = Writer::new(Vec::new());
        w.session(
            1,
            3,
            &[7],
            &[
                Screen {
                    id: 7,
                    name: "eDP-1".to_owned(),
                    x: 0,
                    y: 0,
                    width: 1920,
                    height: 1080,
                    primary: true,
                },
                Screen {
                    id: 8,
                    name: "the one on the left".to_owned(),
                    x: -1280,
                    y: 120,
                    width: 1280,
                    height: 1024,
                    primary: false,
                },
            ],
        )
        .expect("a buffer does not fail");
        let out = w.into_inner();
        let all = records(&out);
        assert_eq!(all.len(), 1);
        let (kind, body) = &all[0];
        assert_eq!(*kind, record::SESSION);

        assert_eq!(body[0], 1, "the profile");
        assert_eq!(body[1], 3, "what it will settle for");
        assert_eq!(body[2], 1, "one screen is being shown");
        assert_eq!(u32::from_be_bytes([body[3], body[4], body[5], body[6]]), 7);
        assert_eq!(body[7], 2, "and the host has two");

        let at = 8;
        assert_eq!(
            u32::from_be_bytes([body[at], body[at + 1], body[at + 2], body[at + 3]]),
            7
        );
        assert_eq!(
            i32::from_be_bytes([body[at + 4], body[at + 5], body[at + 6], body[at + 7]]),
            0
        );
        assert_eq!(
            u32::from_be_bytes([body[at + 12], body[at + 13], body[at + 14], body[at + 15]]),
            1920
        );
        assert_eq!(body[at + 20] & FLAG_PRIMARY, FLAG_PRIMARY);
        let name_len = usize::from(u16::from_be_bytes([body[at + 21], body[at + 22]]));
        assert_eq!(name_len, "eDP-1".len());
        assert_eq!(&body[at + 23..at + 23 + name_len], b"eDP-1");

        // And the second, whose x is negative — a screen left of the primary
        // one starts at a negative offset, and a format that could not say so
        // would describe a desktop nobody has.
        let at = at + 23 + name_len;
        assert_eq!(
            u32::from_be_bytes([body[at], body[at + 1], body[at + 2], body[at + 3]]),
            8
        );
        assert_eq!(
            i32::from_be_bytes([body[at + 4], body[at + 5], body[at + 6], body[at + 7]]),
            -1280
        );
        assert_eq!(body[at + 20] & FLAG_PRIMARY, 0, "and it is not the primary");
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
