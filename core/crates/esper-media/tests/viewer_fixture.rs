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
use esper_media::viewer_stream::{FLAG_KEYFRAME, MAGIC, STREAM_VERSION, Writer, record};

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
        bytes: Vec<u8>,
    },
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
        match kind {
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
                let n = self.u32() as usize;
                Some(Record::Picture {
                    keyframe: flags & FLAG_KEYFRAME != 0,
                    micros,
                    bytes: self.take(n),
                })
            }
            other => panic!("a record kind this format does not have: {other}"),
        }
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
            Record::Picture {
                keyframe,
                micros,
                bytes,
            } => {
                pictures += 1;
                first_is_key.get_or_insert(*keyframe);
                if let Some(previous) = last_micros {
                    assert!(*micros > previous, "{what}: time has to move forward");
                }
                last_micros = Some(*micros);
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
