//! Writing a session to a file, and telling the machine being recorded.
//!
//! # Why the far end is told
//!
//! A recording is a copy of somebody else's screen that **outlives the
//! session**. Watching is over when the window closes; a file is not, and the
//! person whose screen it is has no way to know it exists unless they are
//! told. So this product always says so, on the host's own screen, for as long
//! as the recording lasts.
//!
//! What that cannot do is *enforce* it. A modified controller can write the
//! same frames to the same file and send nothing — as somebody can point a
//! phone at a monitor — and no message can prevent either. What an announced
//! recording buys is that the ordinary case is honest by construction, and
//! that a host has something to write in its log.
//!
//! # Why it starts at a keyframe
//!
//! The container needs the stream's parameter sets in its header, and a
//! recording that began mid-picture would be a file whose first seconds no
//! decoder can make sense of. Waiting costs nothing anyone notices: a session
//! opens with a keyframe, and one is asked for the moment recording starts if
//! the wait would be longer than that.

use std::path::{Path, PathBuf};
use std::time::Duration;

use esper_link::ControllerLink;
use esper_mkv::{Audio, Recording};

/// A recording in progress.
pub struct Recorder {
    path: PathBuf,
    writing: Option<Recording>,
    /// The picture's size, from the config the host announced.
    size: (u32, u32),
    /// The shape of the sound, when the session has any.
    audio: Option<Audio>,
    /// Frames that arrived before the first keyframe, counted so the wait can
    /// be reported rather than looking like nothing happening.
    waited: u64,
    /// Said once, when something has gone wrong with the file.
    complained: bool,
}

impl std::fmt::Debug for Recorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recorder")
            .field("path", &self.path)
            .field("writing", &self.writing.is_some())
            .finish()
    }
}

impl Recorder {
    /// Get ready to record, without creating the file yet.
    ///
    /// **Nothing is written until the first keyframe**, so a session that ends
    /// before one arrives leaves no file rather than an empty one somebody has
    /// to wonder about.
    #[must_use]
    pub fn waiting(path: &Path, size: (u32, u32), audio: Option<Audio>) -> Self {
        Self {
            path: path.to_path_buf(),
            writing: None,
            size,
            audio,
            waited: 0,
            complained: false,
        }
    }

    /// Take whatever the link has kept, and write it.
    ///
    /// `since` is how long the session has been running, which is what the
    /// frames are timestamped with. **Arrival time rather than the host's own
    /// clock**: the two ends' media clocks start at arbitrary offsets and
    /// reconciling them would be guesswork, while arrival time is exactly what
    /// the person watching experienced — including the pauses.
    pub fn take_from(&mut self, link: &mut ControllerLink, since: Duration) {
        let at_ms = u64::try_from(since.as_millis()).unwrap_or(u64::MAX);
        for frame in link.take_encoded() {
            if self.writing.is_none() {
                if !frame.keyframe {
                    self.waited = self.waited.saturating_add(1);
                    continue;
                }
                match Recording::begin(&self.path, &frame.bitstream, self.size, self.audio) {
                    Ok(started) => {
                        println!(
                            "  recording  {} — started at the first keyframe{}",
                            self.path.display(),
                            if self.waited > 0 {
                                format!(", after {} frame(s) that could not begin one", self.waited)
                            } else {
                                String::new()
                            }
                        );
                        self.writing = Some(started);
                    }
                    Err(e) => {
                        self.complain(&format!("could not start the recording: {e}"));
                        continue;
                    }
                }
            }
            if let Some(writing) = self.writing.as_mut()
                && let Err(e) = writing.video(&frame.bitstream, at_ms, frame.keyframe)
            {
                self.complain(&format!("could not write a frame: {e}"));
            }
        }
        for packet in link.take_audio_packets() {
            // Sound only once the video has begun, because the file's header
            // is written with the first keyframe and a packet before that has
            // nowhere to go.
            if let Some(writing) = self.writing.as_mut()
                && let Err(e) = writing.audio(&packet, at_ms)
            {
                self.complain(&format!("could not write a sound packet: {e}"));
            }
        }
    }

    /// Close the file, saying what is in it.
    ///
    /// **A recording that was never started is not a failure**: a session that
    /// ended before its first keyframe is a session with nothing to record.
    pub fn finish(self) {
        let Some(writing) = self.writing else {
            println!(
                "  recording  nothing was written to {}: no keyframe arrived",
                self.path.display()
            );
            return;
        };
        match writing.finish() {
            Ok(written) => println!(
                "  recording  {} — {} frames, {} sound packets, {:.1} MB, {:.1}s",
                self.path.display(),
                written.frames,
                written.audio_frames,
                written.bytes as f64 / 1_048_576.0,
                written.length_ms as f64 / 1000.0
            ),
            Err(e) => eprintln!("  · the recording could not be closed cleanly: {e}"),
        }
    }

    /// Say what went wrong, once.
    ///
    /// A disk that is full fails on every frame, and a session that printed a
    /// line per frame would bury the picture in complaints — the same reason
    /// the audit trail says its own failures once.
    fn complain(&mut self, what: &str) {
        if self.complained {
            return;
        }
        self.complained = true;
        eprintln!("  · {what}");
        eprintln!("    the session carries on; the recording does not");
    }
}
