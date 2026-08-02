//! Writing a session to a file, in Matroska.
//!
//! # Why a container at all
//!
//! A recording could be the H.264 access units concatenated with start codes,
//! which is two lines of code and a file that plays. It would also be a file
//! with **no timing in it**: players assume 25 frames a second, and a recorded
//! desktop session is nothing like 25 frames a second — it is 3 while somebody
//! reads and 60 while they scroll. The recording of an hour at a text editor
//! would play back in four minutes, and the sound would not be in it at all.
//!
//! So: a container with a timestamp on every frame, and room for the audio
//! beside it. Matroska is the one that can be written **as the session
//! happens** — its Segment may declare an unknown size, so nothing has to be
//! seeked back to and patched when the session ends. A recording of a session
//! that crashed is a recording up to the crash, which is exactly what somebody
//! wants from it.
//!
//! # What this is not
//!
//! Not a general muxer. One video track and at most one audio track, no
//! seeking index, no chapters, no tags: what a session recording needs and
//! nothing else. The [`ebml`] module has only the primitives those need.
//!
//! # What checks it
//!
//! A reader written independently in the tests, from the specification rather
//! than from this code — the same arrangement as the PNG writer, which is
//! checked by an inflater written from RFC 1951 and by Python's `zlib`. Where
//! `ffprobe` is on the machine, the suite asks it too: agreement with the
//! thing everybody actually plays files with is the check that matters.

pub mod ebml;

use std::io::{Seek, SeekFrom, Write};

use ebml::id;

/// Anything that went wrong writing a recording.
#[derive(Debug, thiserror::Error)]
pub enum MkvError {
    /// The file could not be written.
    #[error("writing the recording: {0}")]
    Io(String),
    /// A frame arrived before the parameter sets that describe it.
    ///
    /// **Its own error because it is a real sequence, not a bug**: a session
    /// that starts recording mid-stream has no keyframe yet, and a recording
    /// that began with a frame no decoder can start from would be a file that
    /// opens to a grey rectangle. The caller waits for a keyframe.
    #[error("a recording cannot start before a keyframe: no SPS/PPS has been seen")]
    NoParameterSets,
}

/// How the video track is described. H.264 in Matroska is length-prefixed
/// NAL units with the parameter sets in `CodecPrivate`, not Annex B.
const VIDEO_CODEC: &str = "V_MPEG4/ISO/AVC";

/// And the audio, which needs no private data: Opus in Matroska carries its
/// own configuration in the packets.
const AUDIO_CODEC: &str = "A_OPUS";

/// One millisecond per tick.
///
/// **Chosen over the 90 kHz the video arrives with**, because Matroska's
/// timestamp scale is nanoseconds per tick and 1/90000 s is 11111.1 ns — not a
/// whole number, so a 90 kHz scale would drift by a tick every nine frames.
/// A millisecond is exact, and no eye has ever noticed a millisecond of a
/// screen recording.
const TICK_NS: u64 = 1_000_000;

/// The track numbers, fixed because there are only ever these two.
const VIDEO_TRACK: u64 = 1;
const AUDIO_TRACK: u64 = 2;

/// How long a cluster may run before a new one is started.
///
/// **A block's timestamp is a signed 16-bit offset from its cluster**, so a
/// cluster cannot span more than about 32 seconds whatever anybody prefers.
/// Five is well inside that and keeps a truncated file — one whose writer was
/// killed — losing at most five seconds at the end.
const CLUSTER_MS: u64 = 5_000;

/// A recording being written.
pub struct Recording {
    out: std::io::BufWriter<std::fs::File>,
    /// Where the current cluster started, in milliseconds.
    cluster_at: Option<u64>,
    /// The cluster being built, flushed when it gets long enough.
    cluster: Vec<u8>,
    /// What has been written, for the line that says how big it is.
    bytes: u64,
    frames: u64,
    audio_frames: u64,
    /// Where the duration's eight bytes sit in the file, so a clean ending can
    /// fill them in.
    ///
    /// **Reserved up front and patched at the end**, which is the one thing in
    /// this writer that needs the output to be a real file. A live writer
    /// cannot know the length in advance, and a file with no duration is one
    /// most players show no progress bar for until they have scanned it — so
    /// the slot is written as zero and overwritten when the recording stops
    /// properly. A recording that was killed keeps the zero and stays
    /// playable, which is the trade this makes on purpose.
    duration_at: u64,
    /// The last timestamp any track was given.
    last_ms: u64,
}

impl std::fmt::Debug for Recording {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recording")
            .field("frames", &self.frames)
            .field("audio_frames", &self.audio_frames)
            .field("bytes", &self.bytes)
            .finish()
    }
}

impl Recording {
    /// Start a recording, given the first keyframe's parameter sets.
    ///
    /// The header names the picture's size and the audio's shape, so it can
    /// only be written once both are known — which is why this takes them
    /// rather than offering setters. A recording whose header said one size
    /// and whose frames were another would be a file that plays at the wrong
    /// aspect ratio, with nothing to say why.
    ///
    /// # Errors
    ///
    /// If the file cannot be created or written, or if no parameter sets were
    /// found in `keyframe`.
    pub fn begin(
        path: &std::path::Path,
        keyframe: &[u8],
        size: (u32, u32),
        audio: Option<Audio>,
    ) -> Result<Self, MkvError> {
        let avcc = avcc_from(keyframe).ok_or(MkvError::NoParameterSets)?;
        let file = std::fs::File::create(path).map_err(|e| MkvError::Io(e.to_string()))?;
        let mut me = Self {
            out: std::io::BufWriter::new(file),
            cluster_at: None,
            cluster: Vec::new(),
            bytes: 0,
            frames: 0,
            audio_frames: 0,
            duration_at: 0,
            last_ms: 0,
        };
        let (header, duration_at) = header(&avcc, size, audio.as_ref());
        me.duration_at = duration_at;
        me.write_all(&header)?;
        Ok(me)
    }

    /// Add one video frame, at this many milliseconds into the recording.
    ///
    /// # Errors
    ///
    /// If the file cannot be written.
    pub fn video(&mut self, bitstream: &[u8], at_ms: u64, keyframe: bool) -> Result<(), MkvError> {
        let payload = length_prefixed(bitstream);
        self.block(VIDEO_TRACK, &payload, at_ms, keyframe)?;
        self.last_ms = self.last_ms.max(at_ms);
        self.frames = self.frames.saturating_add(1);
        Ok(())
    }

    /// Add one Opus packet, at this many milliseconds into the recording.
    ///
    /// # Errors
    ///
    /// If the file cannot be written.
    pub fn audio(&mut self, packet: &[u8], at_ms: u64) -> Result<(), MkvError> {
        // Every audio frame is a point a decoder can start from, so all of
        // them are marked as such: a player seeking into a recording has to be
        // able to start the sound somewhere.
        self.block(AUDIO_TRACK, packet, at_ms, true)?;
        self.last_ms = self.last_ms.max(at_ms);
        self.audio_frames = self.audio_frames.saturating_add(1);
        Ok(())
    }

    /// Finish, flushing whatever is buffered.
    ///
    /// **Not required for a playable file**, which is the point of the format:
    /// a recording whose writer was killed is playable up to the last cluster
    /// that reached the disk. This is the tidy ending, not a repair.
    ///
    /// # Errors
    ///
    /// If the last cluster or the file itself cannot be flushed.
    pub fn finish(mut self) -> Result<Written, MkvError> {
        self.flush_cluster()?;
        self.write_duration()?;
        self.out.flush().map_err(|e| MkvError::Io(e.to_string()))?;
        Ok(Written {
            frames: self.frames,
            audio_frames: self.audio_frames,
            bytes: self.bytes,
            length_ms: self.last_ms,
        })
    }

    /// Fill in the duration slot reserved in the header.
    ///
    /// Failing here does **not** fail the recording: the file is complete and
    /// playable without it, and a caller who is told "your recording failed"
    /// about a file that is sitting there working would go looking for a
    /// problem that is not theirs.
    fn write_duration(&mut self) -> Result<(), MkvError> {
        if self.duration_at == 0 {
            return Ok(());
        }
        self.out.flush().map_err(|e| MkvError::Io(e.to_string()))?;
        let file = self.out.get_mut();
        let here = file
            .stream_position()
            .map_err(|e| MkvError::Io(e.to_string()))?;
        let ticks = self.last_ms as f64;
        if file.seek(SeekFrom::Start(self.duration_at)).is_ok() {
            let _ = file.write_all(&ticks.to_be_bytes());
        }
        let _ = file.seek(SeekFrom::Start(here));
        Ok(())
    }

    /// How much has been written, for a caller that reports progress.
    #[must_use]
    pub const fn written(&self) -> Written {
        Written {
            frames: self.frames,
            audio_frames: self.audio_frames,
            bytes: self.bytes,
            length_ms: self.last_ms,
        }
    }

    fn block(
        &mut self,
        track: u64,
        payload: &[u8],
        at_ms: u64,
        keyframe: bool,
    ) -> Result<(), MkvError> {
        // A new cluster when there is none, when this one has run long
        // enough, or when the offset would not fit the signed 16 bits a block
        // has for it.
        let start = match self.cluster_at {
            Some(start) if at_ms.saturating_sub(start) < CLUSTER_MS => start,
            _ => {
                self.flush_cluster()?;
                self.cluster_at = Some(at_ms);
                at_ms
            }
        };
        let offset = i64::try_from(at_ms.saturating_sub(start)).unwrap_or(0);
        let relative = i16::try_from(offset).unwrap_or(i16::MAX);

        let mut block = Vec::with_capacity(payload.len() + 8);
        ebml::write_size(track, &mut block);
        block.extend_from_slice(&relative.to_be_bytes());
        block.push(if keyframe { 0x80 } else { 0x00 });
        block.extend_from_slice(payload);
        ebml::element(id::SIMPLE_BLOCK, &block, &mut self.cluster);
        Ok(())
    }

    fn flush_cluster(&mut self) -> Result<(), MkvError> {
        let Some(start) = self.cluster_at.take() else {
            return Ok(());
        };
        if self.cluster.is_empty() {
            return Ok(());
        }
        let blocks = std::mem::take(&mut self.cluster);
        let mut payload = Vec::with_capacity(blocks.len() + 16);
        ebml::unsigned(id::TIMESTAMP, start, &mut payload);
        payload.extend_from_slice(&blocks);
        let mut out = Vec::with_capacity(payload.len() + 16);
        ebml::element(id::CLUSTER, &payload, &mut out);
        self.write_all(&out)
    }

    fn write_all(&mut self, bytes: &[u8]) -> Result<(), MkvError> {
        self.out
            .write_all(bytes)
            .map_err(|e| MkvError::Io(e.to_string()))?;
        self.bytes = self.bytes.saturating_add(bytes.len() as u64);
        Ok(())
    }
}

/// The shape of the sound in a recording.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Audio {
    pub sample_rate: u32,
    pub channels: u8,
}

/// The file's header, and where in it the duration's bytes sit.
fn header(avcc: &[u8], size: (u32, u32), audio: Option<&Audio>) -> (Vec<u8>, u64) {
    let mut out = Vec::new();

    let mut ebml_header = Vec::new();
    ebml::unsigned(id::EBML_VERSION, 1, &mut ebml_header);
    ebml::unsigned(id::EBML_READ_VERSION, 1, &mut ebml_header);
    ebml::unsigned(id::EBML_MAX_ID_LENGTH, 4, &mut ebml_header);
    ebml::unsigned(id::EBML_MAX_SIZE_LENGTH, 8, &mut ebml_header);
    ebml::text(id::DOC_TYPE, "matroska", &mut ebml_header);
    ebml::unsigned(id::DOC_TYPE_VERSION, 4, &mut ebml_header);
    ebml::unsigned(id::DOC_TYPE_READ_VERSION, 2, &mut ebml_header);
    ebml::element(id::EBML, &ebml_header, &mut out);

    // **The Segment's size is left unknown**, which is what makes a recording
    // writable as it happens: see `UNKNOWN_SIZE`.
    id::SEGMENT.write(&mut out);
    out.extend_from_slice(&ebml::UNKNOWN_SIZE);

    let mut info = Vec::new();
    ebml::unsigned(id::TIMESTAMP_SCALE, TICK_NS, &mut info);
    ebml::text(id::MUXING_APP, "esper", &mut info);
    ebml::text(id::WRITING_APP, "esper", &mut info);
    // The duration, reserved as zero. Its offset is the file position of the
    // eight payload bytes, which is where they land once `out` is written.
    ebml::float(id::DURATION, 0.0, &mut info);
    let duration_at = (out.len() + info.len() - 8) as u64
        // The Info element's own id and size go in front of the payload.
        + id_and_size_length(id::INFO, info.len()) as u64;
    ebml::element(id::INFO, &info, &mut out);

    let mut tracks = Vec::new();
    let mut video = Vec::new();
    ebml::unsigned(id::TRACK_NUMBER, VIDEO_TRACK, &mut video);
    ebml::unsigned(id::TRACK_UID, VIDEO_TRACK, &mut video);
    ebml::unsigned(id::TRACK_TYPE, 1, &mut video);
    ebml::text(id::CODEC_ID, VIDEO_CODEC, &mut video);
    ebml::element(id::CODEC_PRIVATE, avcc, &mut video);
    let mut geometry = Vec::new();
    ebml::unsigned(id::PIXEL_WIDTH, u64::from(size.0), &mut geometry);
    ebml::unsigned(id::PIXEL_HEIGHT, u64::from(size.1), &mut geometry);
    ebml::element(id::VIDEO, &geometry, &mut video);
    ebml::element(id::TRACK_ENTRY, &video, &mut tracks);

    if let Some(audio) = audio {
        let mut track = Vec::new();
        ebml::unsigned(id::TRACK_NUMBER, AUDIO_TRACK, &mut track);
        ebml::unsigned(id::TRACK_UID, AUDIO_TRACK, &mut track);
        ebml::unsigned(id::TRACK_TYPE, 2, &mut track);
        ebml::text(id::CODEC_ID, AUDIO_CODEC, &mut track);
        let mut shape = Vec::new();
        ebml::float(
            id::SAMPLING_FREQUENCY,
            f64::from(audio.sample_rate),
            &mut shape,
        );
        ebml::unsigned(id::CHANNELS, u64::from(audio.channels), &mut shape);
        ebml::element(id::AUDIO, &shape, &mut track);
        ebml::element(id::TRACK_ENTRY, &track, &mut tracks);
    }
    ebml::element(id::TRACKS, &tracks, &mut out);
    (out, duration_at)
}

/// How many bytes an element's id and size take in front of its payload.
///
/// Needed because the duration is patched by seeking to a byte offset, and the
/// offset has to account for the header that precedes the payload it sits in.
fn id_and_size_length(id: ebml::Id, payload: usize) -> usize {
    let mut probe = Vec::new();
    id.write(&mut probe);
    ebml::write_size(payload as u64, &mut probe);
    probe.len()
}

/// What a finished recording came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Written {
    pub frames: u64,
    pub audio_frames: u64,
    pub bytes: u64,
    /// The last timestamp any track was given, which is the file's length.
    pub length_ms: u64,
}

/// An `avcC` configuration record, built from a keyframe's parameter sets.
///
/// Matroska describes H.264 the way MP4 does: the sequence and picture
/// parameter sets live in the track header, and the frames are length-prefixed
/// rather than start-code separated. So a recording cannot begin until a
/// keyframe has been seen — which is also when the picture is first showable,
/// so the wait costs nothing.
fn avcc_from(keyframe: &[u8]) -> Option<Vec<u8>> {
    let mut sps: Option<&[u8]> = None;
    let mut pps: Option<&[u8]> = None;
    for unit in esper_media::NalUnits::new(keyframe) {
        match unit.kind() {
            esper_media::annexb::nal::SPS if sps.is_none() => sps = Some(unit.bytes),
            esper_media::annexb::nal::PPS if pps.is_none() => pps = Some(unit.bytes),
            _ => {}
        }
    }
    let (sps, pps) = (sps?, pps?);
    // The profile, constraints and level are copied out of the SPS itself
    // rather than assumed: a recording that claimed a profile the stream does
    // not use is a file some players refuse and others play wrongly.
    let profile = *sps.get(1)?;
    let constraints = *sps.get(2)?;
    let level = *sps.get(3)?;
    let mut out = vec![
        1, // configurationVersion
        profile,
        constraints,
        level,
        // Six reserved bits set, then a two-bit length-size-minus-one: four
        // byte lengths, which is what `length_prefixed` writes.
        0xFF,
        // Three reserved bits set, then five bits of SPS count: one.
        0xE1,
    ];
    out.extend_from_slice(&u16::try_from(sps.len()).ok()?.to_be_bytes());
    out.extend_from_slice(sps);
    out.push(1); // one PPS
    out.extend_from_slice(&u16::try_from(pps.len()).ok()?.to_be_bytes());
    out.extend_from_slice(pps);
    Some(out)
}

/// An access unit with each NAL unit prefixed by its length.
///
/// The parameter sets are **kept** rather than stripped. They are redundant
/// with the track header, and a few dozen bytes on each keyframe is a price
/// worth paying for a file that a decoder can still start from if it is cut
/// with a knife — which is what a truncated recording is.
fn length_prefixed(bitstream: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bitstream.len() + 8);
    for unit in esper_media::NalUnits::new(bitstream) {
        let Ok(length) = u32::try_from(unit.bytes.len()) else {
            continue;
        };
        out.extend_from_slice(&length.to_be_bytes());
        out.extend_from_slice(unit.bytes);
    }
    out
}
