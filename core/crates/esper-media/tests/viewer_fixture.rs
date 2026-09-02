#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! The viewer stream, as a fixture both halves read.
//!
//! `testdata/fixtures/README.md` says why this directory exists: Esper is two
//! implementations of one product, and anything both halves read has to be
//! read the same way. The viewer adds a third reader in a third language — the
//! page parses this stream with TypeScript and hands it to the browser's own
//! decoder — so the stream joins the corpus for the same reason the wire
//! fixtures did.
//!
//! # Why this is not asserted byte for byte
//!
//! The wire fixtures are held to byte identity because *we* encode them, and a
//! difference is our drift. This one contains openh264's output, and holding a
//! third party's encoder to byte identity would make an upstream release look
//! like a broken format. What is asserted is everything the format promises:
//! the magic, the version, the record layout, and that every length adds up.

use std::path::PathBuf;

use esper_capture::pattern::Pattern;
use esper_color::{ColorSpace, I420};
use esper_encode::encoder::{EncoderConfig, VideoEncoder};
use esper_encode::openh264::OpenH264;
use esper_media::avcc::Repackager;
use esper_media::viewer_stream::{
    FLAG_DRAW_POINTER, FLAG_HAS_IMAGE, FLAG_IN_PICTURE, FLAG_KEYFRAME, FLAG_PRIMARY, MAGIC,
    STREAM_VERSION, Screen, Writer, record,
};

const W: u32 = 320;
const H: u32 = 240;
const FRAMES: u64 = 24;
const FPS: u64 = 30;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("the crate is at <root>/core/crates/<name>")
        .join("testdata/fixtures/viewer_stream.bin")
}

fn picture(index: u64) -> I420 {
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

/// The stream this build produces, from the encoder that ships with it.
fn build() -> Vec<u8> {
    let mut encoder = OpenH264::new(&EncoderConfig {
        fps: u32::try_from(FPS).unwrap(),
        ..EncoderConfig::for_size(W, H)
    })
    .expect("an encoder");
    let mut repack = Repackager::new();
    let mut writer = Writer::new(Vec::new());

    /*
        Everything a window is told, not only the pictures.

        The page reads this stream with a reader of its own, in a third
        language, and a record kind that is never in the fixture is a kind
        whose two readers have never been compared. The session and the
        pointer went in the day they were added for that reason.
    */
    let mut said_what_it_is = false;

    for index in 0..FRAMES {
        let encoded = encoder
            .encode(&picture(index), index == 0)
            .expect("encoded");
        let Some(sample) = repack.push(&encoded.bitstream) else {
            continue;
        };
        if repack.take_description_change() {
            writer
                .configuration(W, H, repack.sets())
                .expect("a buffer does not fail");
        }
        writer
            .picture(&sample, index * 1_000_000 / FPS)
            .expect("a buffer does not fail");

        if !said_what_it_is {
            said_what_it_is = true;
            writer
                .session(
                    1,
                    1,
                    &[0],
                    &[
                        Screen {
                            id: 0,
                            name: "the one it is showing".to_owned(),
                            x: 0,
                            y: 0,
                            width: W,
                            height: H,
                            primary: true,
                        },
                        // Left of the primary one, so its origin is negative:
                        // a reader that took this for unsigned would put the
                        // screen four billion pixels to the right.
                        Screen {
                            id: 1,
                            name: "the one to the left".to_owned(),
                            x: -640,
                            y: 24,
                            width: 640,
                            height: 480,
                            primary: false,
                        },
                    ],
                )
                .expect("a buffer does not fail");
            // A pointer with a hotspot that is not its corner, and pixels
            // that are not all the same: both are things a reader can get
            // wrong in a way that still looks like a cursor.
            let rgba: Vec<u8> = (0..16 * 16)
                .flat_map(|i: u32| {
                    let on = (i / 16 + i % 16).is_multiple_of(2);
                    [
                        255,
                        u8::try_from(i % 251).unwrap(),
                        0,
                        if on { 255 } else { 0 },
                    ]
                })
                .collect();
            writer
                .pointer_shape(Some((16, 16, 4, 3, &rgba)))
                .expect("a buffer does not fail");
            writer
                .pointer(true, true, 100, 50)
                .expect("a buffer does not fail");
        }
    }
    writer.into_inner()
}

/// A reader written from the format's documentation, not from the writer.
struct Reader<'a> {
    rest: &'a [u8],
}

#[derive(Debug, PartialEq, Eq)]
enum Record {
    Configuration {
        width: u32,
        height: u32,
        codec: String,
        description: Vec<u8>,
    },
    Picture {
        keyframe: bool,
        micros: u64,
        handed: u64,
        bytes: Vec<u8>,
    },
    Session {
        profile: u8,
        sharpness: u8,
        showing: Vec<u32>,
        screens: Vec<(u32, String, i32, i32, u32, u32, bool)>,
    },
    Pointer {
        in_picture: bool,
        draw: bool,
        x: i32,
        y: i32,
    },
    PointerShape {
        image: Option<(u16, u16, u16, u16, Vec<u8>)>,
    },
    /// A kind this reader does not know, walked past by its length.
    Unknown { kind: u8 },
}

impl<'a> Reader<'a> {
    fn begin(stream: &'a [u8]) -> Self {
        assert_eq!(&stream[..8], MAGIC, "the stream names itself");
        assert_eq!(
            stream[8], STREAM_VERSION,
            "a reader refuses a version it does not know"
        );
        Self { rest: &stream[9..] }
    }

    fn u16(&mut self) -> u16 {
        let v = u16::from_be_bytes(self.rest[..2].try_into().unwrap());
        self.rest = &self.rest[2..];
        v
    }
    fn u8(&mut self) -> u8 {
        let v = self.rest[0];
        self.rest = &self.rest[1..];
        v
    }
    fn i32(&mut self) -> i32 {
        let v = i32::from_be_bytes(self.rest[..4].try_into().unwrap());
        self.rest = &self.rest[4..];
        v
    }
    fn u32(&mut self) -> u32 {
        let v = u32::from_be_bytes(self.rest[..4].try_into().unwrap());
        self.rest = &self.rest[4..];
        v
    }
    fn u64(&mut self) -> u64 {
        let v = u64::from_be_bytes(self.rest[..8].try_into().unwrap());
        self.rest = &self.rest[8..];
        v
    }
    fn take(&mut self, n: usize) -> Vec<u8> {
        let v = self.rest[..n].to_vec();
        self.rest = &self.rest[n..];
        v
    }
}

impl Iterator for Reader<'_> {
    type Item = Record;

    fn next(&mut self) -> Option<Record> {
        if self.rest.is_empty() {
            return None;
        }
        let kind = self.rest[0];
        self.rest = &self.rest[1..];
        // The body's length, which is what lets a reader skip a kind it does
        // not know — and what lets this one insist that a record reads
        // exactly as many bytes as it declared.
        let declared = self.u32() as usize;
        let before = self.rest.len();
        let record = match kind {
            record::CONFIGURATION => {
                let width = self.u32();
                let height = self.u32();
                let n = usize::from(self.u16());
                let codec = String::from_utf8(self.take(n)).expect("utf-8");
                let n = self.u32() as usize;
                let description = self.take(n);
                Some(Record::Configuration {
                    width,
                    height,
                    codec,
                    description,
                })
            }
            record::PICTURE => {
                let flags = self.rest[0];
                self.rest = &self.rest[1..];
                let micros = self.u64();
                let handed = self.u64();
                let n = self.u32() as usize;
                Some(Record::Picture {
                    keyframe: flags & FLAG_KEYFRAME != 0,
                    micros,
                    handed,
                    bytes: self.take(n),
                })
            }
            record::SESSION => {
                let profile = self.u8();
                let sharpness = self.u8();
                let showing = (0..self.u8()).map(|_| self.u32()).collect();
                let mut screens = Vec::new();
                for _ in 0..self.u8() {
                    let id = self.u32();
                    let x = self.i32();
                    let y = self.i32();
                    let width = self.u32();
                    let height = self.u32();
                    let primary = self.u8() & FLAG_PRIMARY != 0;
                    let n = usize::from(self.u16());
                    let name = String::from_utf8(self.take(n)).expect("utf-8");
                    screens.push((id, name, x, y, width, height, primary));
                }
                Some(Record::Session {
                    profile,
                    sharpness,
                    showing,
                    screens,
                })
            }
            record::POINTER => {
                let flags = self.u8();
                Some(Record::Pointer {
                    in_picture: flags & FLAG_IN_PICTURE != 0,
                    draw: flags & FLAG_DRAW_POINTER != 0,
                    x: self.i32(),
                    y: self.i32(),
                })
            }
            record::POINTER_SHAPE => {
                let flags = self.u8();
                let hot_x = self.u16();
                let hot_y = self.u16();
                let width = self.u16();
                let height = self.u16();
                let n = self.u32() as usize;
                let rgba = self.take(n);
                Some(Record::PointerShape {
                    image: (flags & FLAG_HAS_IMAGE != 0)
                        .then_some((width, height, hot_x, hot_y, rgba)),
                })
            }
            other => {
                // Skipped rather than refused: a kind from a later version is
                // exactly what the length is for.
                self.take(declared);
                Some(Record::Unknown { kind: other })
            }
        };
        assert_eq!(
            before - self.rest.len(),
            declared,
            "a record of kind {kind} read a different number of bytes than it declared"
        );
        record
    }
}

/// Every length in an AVCC sample adds up to exactly the sample.
fn lengths_add_up(sample: &[u8]) -> usize {
    let mut at = 0;
    let mut units = 0;
    while at + 4 <= sample.len() {
        let len = u32::from_be_bytes(sample[at..at + 4].try_into().unwrap()) as usize;
        at += 4 + len;
        units += 1;
        assert!(
            at <= sample.len(),
            "a length prefix that runs past its own sample"
        );
    }
    assert_eq!(at, sample.len(), "a sample with bytes nobody claimed");
    units
}

fn check(stream: &[u8], what: &str) {
    let records: Vec<Record> = Reader::begin(stream).collect();
    let mut configurations = 0;
    let mut pictures = 0;
    let mut sessions = 0;
    let mut pointers = 0;
    let mut shapes = 0;
    let mut first_is_key = None;
    let mut last_micros = None;

    for r in &records {
        match r {
            Record::Configuration {
                width,
                height,
                codec,
                description,
            } => {
                configurations += 1;
                assert_eq!((*width, *height), (W, H), "{what}: the size it says it is");
                assert!(
                    codec.starts_with("avc1.4D"),
                    "{what}: Main profile, got {codec}"
                );
                assert_eq!(description[0], 1, "{what}: configurationVersion");
                assert_eq!(description[4] & 0x03, 3, "{what}: four-byte lengths");
            }
            Record::Unknown { kind } => panic!("{what}: an unexpected record kind {kind}"),
            Record::Session {
                profile,
                sharpness,
                showing,
                screens,
            } => {
                sessions += 1;
                assert_eq!((*profile, *sharpness), (1, 1), "{what}: what it asked for");
                assert_eq!(showing, &vec![0], "{what}: the screen it is of");
                assert_eq!(screens.len(), 2, "{what}: the screens the host has");
                assert!(screens[0].6, "{what}: the first is the primary one");
                assert_eq!(screens[0].1, "the one it is showing");
                // The one to the left starts at a negative x, which is the
                // whole reason this field is signed.
                assert_eq!(screens[1].2, -640, "{what}: a screen left of the primary");
                assert!(!screens[1].6, "{what}: and it is not the primary");
            }
            Record::Pointer {
                in_picture,
                draw,
                x,
                y,
            } => {
                pointers += 1;
                assert!(
                    *in_picture && *draw,
                    "{what}: the flags it was written with"
                );
                assert_eq!((*x, *y), (100, 50), "{what}: where the pointer is");
            }
            Record::PointerShape { image } => {
                shapes += 1;
                let (width, height, hot_x, hot_y, rgba) =
                    image.as_ref().expect("{what}: a shape with an image");
                assert_eq!((*width, *height), (16, 16));
                // A hotspot that is not the corner: one read as (0,0) still
                // looks like a cursor and points a few pixels wrong.
                assert_eq!((*hot_x, *hot_y), (4, 3), "{what}: the hotspot");
                assert_eq!(
                    rgba.len(),
                    16 * 16 * 4,
                    "{what}: four bytes a pixel, straight RGBA"
                );
                assert_eq!(rgba[3], 255, "{what}: the first pixel is opaque");
                assert_eq!(rgba[7], 0, "{what}: and the second is not");
            }
            Record::Picture {
                keyframe,
                micros,
                handed,
                bytes,
            } => {
                pictures += 1;
                first_is_key.get_or_insert(*keyframe);
                if let Some(previous) = last_micros {
                    assert!(*micros > previous, "{what}: time has to move forward");
                }
                last_micros = Some(*micros);
                // The wall clock is what measures the hop this path adds; a
                // zero is a writer that could not read a clock.
                assert!(
                    *handed > 1_600_000_000_000_000,
                    "{what}: a picture handed over at {handed}, which is not a wall clock"
                );
                assert!(
                    lengths_add_up(bytes) >= 1,
                    "{what}: a picture with no units"
                );
            }
        }
    }

    assert_eq!(
        configurations, 1,
        "{what}: one configuration for one unchanging size"
    );
    assert!(pictures >= 20, "{what}: only {pictures} pictures");
    assert_eq!(
        first_is_key,
        Some(true),
        "{what}: a viewer has to be able to begin"
    );
    assert!(
        matches!(records.first(), Some(Record::Configuration { .. })),
        "{what}: the decoder is configured before it is fed"
    );
    // One of each, so a reader in another language has something of every
    // kind to be wrong about.
    assert_eq!(sessions, 1, "{what}: what the session is of");
    assert_eq!(pointers, 1, "{what}: where the pointer is");
    assert_eq!(shapes, 1, "{what}: what the pointer looks like");
}

#[test]
fn the_committed_fixture_is_a_stream_this_build_can_read() {
    let path = fixture();
    let stream = std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "{}: {e} — regenerate with `make viewer-fixture`",
            path.display()
        )
    });
    check(&stream, "the committed fixture");
}

#[test]
fn what_this_build_writes_is_the_same_shape() {
    check(&build(), "a freshly written stream");
}

/// Regenerate the fixture. `#[ignore]`d because it writes into the tree.
#[test]
#[ignore = "writes testdata/fixtures/viewer_stream.bin; run it through `make viewer-fixture`"]
fn write_the_fixture() {
    let stream = build();
    check(&stream, "what is about to be written");
    std::fs::write(fixture(), &stream).expect("writing the fixture");
    println!("wrote {} bytes to {}", stream.len(), fixture().display());
}
