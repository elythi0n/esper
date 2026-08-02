//! **A reader written from the specification, not from the writer.**
//!
//! A format checked only by its own code is checked against its own
//! misunderstanding — the lesson the timezone parser learned against Python's
//! `zoneinfo`, and the PNG writer against an inflater written from RFC 1951.
//! So this file parses EBML from first principles: an id, a size whose first
//! byte says how long it is, and a payload.
//!
//! And where `ffprobe` is on the machine, it is asked as well. Agreement with
//! the thing people actually play files with is the check that matters; the
//! reader here is what says *why* when they disagree.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use esper_capture::damage::Damage;
use esper_capture::frame::{Frame, PixelFormat};
use esper_capture::pattern::Pattern;
use esper_capture::pool::Pixels;
use esper_color::convert::whole_frame;
use esper_color::{ColorSpace, I420};
use esper_encode::encoder::{EncoderConfig, VideoEncoder};
use esper_encode::openh264::OpenH264;
use esper_mkv::{Audio, Recording};

/// One element, as read back out of the file.
#[derive(Debug)]
struct Element {
    id: u64,
    payload: Vec<u8>,
    children: Vec<Element>,
}

/// Read a variable-length integer: the first byte's leading zeros say how many
/// bytes it takes, and the marker bit is not part of the value.
fn read_vint(bytes: &[u8], at: &mut usize) -> Option<(u64, bool)> {
    let first = *bytes.get(*at)?;
    let length = (first.leading_zeros() + 1) as usize;
    if length > 8 || *at + length > bytes.len() {
        return None;
    }
    // The marker bit is not part of the value, and a length of eight leaves
    // no value bits in the first byte at all — `0xFF >> 8` overflows a `u8`
    // shift, which is what the first version of this did.
    let mask = if length >= 8 { 0u8 } else { 0xFFu8 >> length };
    let mut value = u64::from(first & mask);
    let mut all_ones = first & mask == mask;
    for i in 1..length {
        let byte = *bytes.get(*at + i)?;
        value = (value << 8) | u64::from(byte);
        all_ones = all_ones && byte == 0xFF;
    }
    *at += length;
    Some((value, all_ones))
}

/// An id is read as its literal bytes, which is how it was written.
fn read_id(bytes: &[u8], at: &mut usize) -> Option<u64> {
    let first = *bytes.get(*at)?;
    let length = (first.leading_zeros() + 1) as usize;
    if length > 4 || *at + length > bytes.len() {
        return None;
    }
    let mut value = 0u64;
    for i in 0..length {
        value = (value << 8) | u64::from(*bytes.get(*at + i)?);
    }
    *at += length;
    Some(value)
}

/// Which ids hold other elements rather than a value.
fn is_container(id: u64) -> bool {
    matches!(
        id,
        0x1A45_DFA3 | 0x1853_8067 | 0x1549_A966 | 0x1654_AE6B | 0xAE | 0xE0 | 0xE1 | 0x1F43_B675
    )
}

fn parse(bytes: &[u8], at: &mut usize, until: usize) -> Vec<Element> {
    let mut out = Vec::new();
    while *at < until {
        let Some(id) = read_id(bytes, at) else { break };
        let Some((size, unknown)) = read_vint(bytes, at) else {
            break;
        };
        // An unknown size means "until the parent ends", which for a Segment
        // being written live means "to the end of the file".
        let end = if unknown {
            until
        } else {
            (*at + size as usize).min(until)
        };
        let element = if is_container(id) {
            let mut children_at = *at;
            let children = parse(bytes, &mut children_at, end);
            Element {
                id,
                payload: Vec::new(),
                children,
            }
        } else {
            Element {
                id,
                payload: bytes[*at..end].to_vec(),
                children: Vec::new(),
            }
        };
        *at = end;
        out.push(element);
    }
    out
}

fn find(elements: &[Element], id: u64) -> Option<&Element> {
    elements.iter().find(|e| e.id == id)
}

fn unsigned(element: &Element) -> u64 {
    element
        .payload
        .iter()
        .fold(0u64, |acc, b| (acc << 8) | u64::from(*b))
}

/// A keyframe with the parameter sets a real encoder emits, in Annex B.
fn keyframe() -> Vec<u8> {
    let mut out = Vec::new();
    // SPS: NAL header 0x67, then profile/constraints/level and a payload.
    out.extend_from_slice(&[0, 0, 0, 1, 0x67, 0x42, 0xC0, 0x1E, 0xAB, 0xCD]);
    // PPS: NAL header 0x68.
    out.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xCE, 0x3C, 0x80]);
    // IDR slice: NAL header 0x65.
    out.extend_from_slice(&[0, 0, 0, 1, 0x65, 1, 2, 3, 4, 5, 6, 7, 8]);
    out
}

fn delta() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 0, 1, 0x41, 9, 9, 9]);
    out
}

/// A frame the way a capturer hands one over, from the shared test pattern.
///
/// **A padded stride, because no real backend is tightly packed** — the same
/// reasoning as the decode crate's own helper, which this mirrors rather than
/// shares: test helpers are not a public API to depend on across crates.
fn captured(pattern: &Pattern, index: u64) -> Frame {
    let (w, h) = (pattern.width(), pattern.height());
    let format = PixelFormat::Bgrx8;
    let bpp = format.bytes_per_pixel();
    let stride = (w as usize) * bpp + 64;
    let mut buf = vec![0u8; stride * h as usize];
    for y in 0..h {
        for x in 0..w {
            let [r, g, b] = pattern.pixel(x, y, index);
            let at = y as usize * stride + x as usize * bpp;
            buf[at] = b;
            buf[at + 1] = g;
            buf[at + 2] = r;
        }
    }
    Frame::new(w, h, stride, format, Damage::Everything, Pixels::owned(buf)).expect("a frame")
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("esper-mkv-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir.join(name)
}

/// **The file says what it is, and where every frame belongs.**
#[test]
fn a_recording_reads_back_as_matroska_with_the_timing_it_was_given() {
    let path = scratch("timing.mkv");
    let mut recording = Recording::begin(
        &path,
        &keyframe(),
        (1280, 720),
        Some(Audio {
            sample_rate: 48_000,
            channels: 2,
        }),
    )
    .expect("started");
    recording.video(&keyframe(), 0, true).expect("a keyframe");
    recording.audio(&[0xFC, 1, 2, 3], 0).expect("a packet");
    // A gap no fixed framerate could express, which is the whole reason for a
    // container: a desktop session is idle and then busy.
    recording.video(&delta(), 3_400, false).expect("a frame");
    recording.video(&delta(), 3_433, false).expect("a frame");
    // And past the cluster length, so the next one starts a cluster.
    recording
        .video(&keyframe(), 9_000, true)
        .expect("a keyframe");
    let written = recording.finish().expect("finished");
    assert_eq!((written.frames, written.audio_frames), (4, 1));
    assert!(written.bytes > 0);
    assert_eq!(written.length_ms, 9_000, "the last frame is where it ends");

    let file = std::fs::read(&path).expect("the file");
    let mut at = 0;
    let top = parse(&file, &mut at, file.len());

    let header = find(&top, 0x1A45_DFA3).expect("an EBML header");
    let doc_type = find(&header.children, 0x4282).expect("a DocType");
    assert_eq!(
        String::from_utf8_lossy(&doc_type.payload),
        "matroska",
        "the file has to say what it is"
    );

    let segment = find(&top, 0x1853_8067).expect("a Segment");
    let info = find(&segment.children, 0x1549_A966).expect("an Info");
    assert_eq!(
        unsigned(find(&info.children, 0x002A_D7B1).expect("a TimestampScale")),
        1_000_000,
        "a millisecond a tick"
    );

    let tracks = find(&segment.children, 0x1654_AE6B).expect("Tracks");
    let entries: Vec<&Element> = tracks.children.iter().filter(|e| e.id == 0xAE).collect();
    assert_eq!(entries.len(), 2, "one video track and one audio track");

    let video = entries[0];
    assert_eq!(
        String::from_utf8_lossy(&find(&video.children, 0x86).expect("a CodecID").payload),
        "V_MPEG4/ISO/AVC"
    );
    let geometry = find(&video.children, 0xE0).expect("a Video element");
    assert_eq!(
        unsigned(find(&geometry.children, 0xB0).expect("width")),
        1280
    );
    assert_eq!(
        unsigned(find(&geometry.children, 0xBA).expect("height")),
        720
    );

    // **The parameter sets are in the track, which is what makes the file
    // playable at all**: Matroska describes H.264 the way MP4 does.
    let private = find(&video.children, 0x63A2).expect("CodecPrivate");
    assert_eq!(private.payload.first(), Some(&1), "an avcC record");
    assert_eq!(
        private.payload.get(1..4),
        Some(&[0x42, 0xC0, 0x1E][..]),
        "with the profile, constraints and level copied out of the SPS"
    );

    let audio = entries[1];
    assert_eq!(
        String::from_utf8_lossy(&find(&audio.children, 0x86).expect("a CodecID").payload),
        "A_OPUS"
    );

    // Two clusters, because the last frame is past the cluster length — and
    // every block's offset fits the signed 16 bits it has.
    let clusters: Vec<&Element> = segment
        .children
        .iter()
        .filter(|e| e.id == 0x1F43_B675)
        .collect();
    assert_eq!(clusters.len(), 2, "a long gap starts a new cluster");
    let first = unsigned(find(&clusters[0].children, 0xE7).expect("a Timestamp"));
    let second = unsigned(find(&clusters[1].children, 0xE7).expect("a Timestamp"));
    assert_eq!((first, second), (0, 9_000));

    // The frames are where they were put: the first cluster holds the
    // keyframe, the audio packet and both deltas.
    let blocks: Vec<&Element> = clusters[0]
        .children
        .iter()
        .filter(|e| e.id == 0xA3)
        .collect();
    assert_eq!(blocks.len(), 4);
    // Each block: a track number, a signed offset, flags, then the frame.
    let mut cursor = 0usize;
    let (track, _) = read_vint(&blocks[2].payload, &mut cursor).expect("a track number");
    assert_eq!(track, 1, "video is track one");
    let offset = i16::from_be_bytes([blocks[2].payload[cursor], blocks[2].payload[cursor + 1]]);
    assert_eq!(offset, 3_400, "the frame keeps the time it was given");
    assert_eq!(
        blocks[2].payload[cursor + 2] & 0x80,
        0,
        "and is not marked as a keyframe, because it is not one"
    );
}

/// **A recording cannot start before a keyframe**, and says so rather than
/// writing a file that opens to nothing.
#[test]
fn a_recording_without_parameter_sets_is_refused() {
    let path = scratch("no-sps.mkv");
    let refused = Recording::begin(&path, &delta(), (640, 360), None);
    assert!(
        matches!(refused, Err(esper_mkv::MkvError::NoParameterSets)),
        "a stream with no SPS/PPS has nothing to put in the track header"
    );
}

/// **A file cut off mid-recording is still a file.**
///
/// The Segment declares an unknown size on purpose, so nothing has to be
/// seeked back to and patched — which a process that is killed cannot do. What
/// this asserts is that the header and the first cluster stand on their own.
#[test]
fn a_recording_that_was_never_finished_still_parses() {
    let path = scratch("cut-short.mkv");
    let mut recording = Recording::begin(&path, &keyframe(), (640, 360), None).expect("started");
    recording.video(&keyframe(), 0, true).expect("a keyframe");
    recording.video(&delta(), 33, false).expect("a frame");
    // Past the cluster length, which is what forces the first cluster out to
    // the disk. Then the recording is dropped without `finish`, the way a
    // killed process leaves it.
    recording.video(&delta(), 6_000, false).expect("a frame");
    drop(recording);

    let file = std::fs::read(&path).expect("the file");
    let mut at = 0;
    let top = parse(&file, &mut at, file.len());
    let segment = find(&top, 0x1853_8067).expect("a Segment");
    assert!(
        find(&segment.children, 0x1654_AE6B).is_some(),
        "the tracks have to be readable in a file nobody finished"
    );
    let clusters: Vec<&Element> = segment
        .children
        .iter()
        .filter(|e| e.id == 0x1F43_B675)
        .collect();
    assert_eq!(
        clusters.len(),
        1,
        "the cluster that reached the disk is there, and the one still buffered is not"
    );
}

/// **And `ffprobe` agrees**, where the machine has it.
///
/// The reader above says what this writer produced; this says whether anything
/// else can read it. **With a real bitstream from the real encoder**, because
/// the first version of this test used a hand-made SPS and `ffprobe` was quite
/// right to complain — a container whose payload is nonsense proves nothing
/// about the container, and the complaint hid whether the file was well formed.
///
/// Skipped out loud rather than silently, because a check that quietly
/// disappears on a machine without the tool is a check nobody knows they are
/// missing.
#[test]
fn ffprobe_reads_a_real_recording() {
    if std::process::Command::new("ffprobe")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_err()
    {
        println!("skipped: this machine has no ffprobe");
        return;
    }

    let (w, h) = (320u32, 180u32);
    let mut staging = I420::new(w, h).expect("a size");
    let pattern = Pattern::new(w, h).expect("a size");
    let mut encoder = OpenH264::new(&EncoderConfig {
        fps: 30,
        ..EncoderConfig::for_size(w, h)
    })
    .expect("an encoder");

    let mut encode = |index: u64, keyframe: bool| {
        let frame = captured(&pattern, index);
        let _ = whole_frame(&frame, ColorSpace::default(), &mut staging).expect("converted");
        encoder
            .encode(&staging, keyframe)
            .expect("encoded")
            .bitstream
    };

    let path = scratch("real.mkv");
    let first = encode(0, true);
    let mut recording = Recording::begin(&path, &first, (w, h), None).expect("started");
    recording.video(&first, 0, true).expect("a keyframe");
    for index in 1..30u64 {
        let frame = encode(index, false);
        recording.video(&frame, index * 33, false).expect("a frame");
    }
    let written = recording.finish().expect("finished");
    assert_eq!(written.frames, 30);

    let out = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-count_frames",
            "-show_entries",
            "stream=codec_name,width,height,nb_read_frames",
            "-of",
            "csv=p=0",
        ])
        .arg(&path)
        .output()
        .expect("ffprobe ran");
    let said = String::from_utf8_lossy(&out.stdout).to_string();
    let errors = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        errors.trim().is_empty(),
        "ffprobe complained about this file: {errors}"
    );
    assert!(
        said.contains("h264") && said.contains("320") && said.contains("180"),
        "ffprobe did not see the video track this writer claims: {said:?}"
    );
    assert!(
        said.trim_end().ends_with(",30"),
        "ffprobe did not decode all thirty frames: {said:?}"
    );

    // **And the file says how long it is**, which needs the duration slot in
    // the header to have been patched on the way out: a live writer cannot
    // know the length in advance, and most players show no progress bar for a
    // file that never says.
    let length = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
        ])
        .arg(&path)
        .output()
        .expect("ffprobe ran");
    let seconds: f64 = String::from_utf8_lossy(&length.stdout)
        .trim()
        .parse()
        .unwrap_or(0.0);
    assert!(
        (0.9..1.1).contains(&seconds),
        "thirty frames at 33 ms should be about a second, not {seconds}"
    );
}
