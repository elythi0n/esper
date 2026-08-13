//! The controller's speakers, on a thread of their own.
//!
//! # Why a thread
//!
//! Playing a frame blocks until the sound card has room for it, which is what
//! makes sound come out at the right speed — and it is the last thing a
//! viewer's loop should do. That loop is drawing the screen and reading the
//! keyboard, and a session whose pointer stuttered every 20 ms because it was
//! waiting on a speaker would be a worse product than one with no sound.
//!
//! So the loop hands frames over and never waits. What that costs is a
//! decision about what to do when the speaker is behind, and this is where it
//! is made: **the frame is dropped.**
//!
//! # Why dropping is the right answer
//!
//! It is the A/V policy of the whole product, applied here. Sound is never
//! delayed to line up with the picture, so a queue that grew to hold late
//! frames would turn into a delay — and a person hears a delay as somebody
//! talking out of step with their mouth, which is more distracting than a
//! click. Ten frames of slack absorb an ordinary hiccup; beyond that, the
//! oldest sound is worth less than the newest.

// Nothing in this module runs on a platform with no sound backend for it:
// `start` refuses and the session says so. It is still compiled everywhere,
// which is deliberate — the thread below is generic over the trait, so it is
// the part a new platform reuses rather than rewrites, and compiling it
// everywhere is what keeps it honest.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::sync::mpsc::{Receiver, SyncSender, TrySendError};

use esper_audio::{Format, Frame, Sink};

/// How many frames may wait for the speaker.
///
/// **Ten, a fifth of a second.** Enough to cover a viewer that was busy
/// drawing a keyframe, and short enough that the sound is never a fifth of a
/// second behind the picture.
const BACKLOG: usize = 10;

/// The handle the session holds.
#[derive(Debug)]
pub struct Playing {
    frames: SyncSender<Frame>,
    backend: &'static str,
    dropped: u64,
}

impl Playing {
    /// Which backend is playing.
    #[must_use]
    pub const fn backend(&self) -> &'static str {
        self.backend
    }

    /// How many frames were dropped because the speaker was behind.
    #[must_use]
    pub const fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Hand a frame over to be played, without waiting for it.
    ///
    /// Returns `false` when playback has stopped, so a caller can say so once
    /// rather than discovering it per frame.
    pub fn play(&mut self, frame: Frame) -> bool {
        match self.frames.try_send(frame) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                self.dropped = self.dropped.saturating_add(1);
                true
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }
}

/// Open the speaker and start playing what is handed over.
///
/// # Errors
///
/// If there is no sound server or the tools to reach it are not installed. **A
/// caller must not treat that as fatal**: a session that shows a screen and
/// cannot play its sound is still a session, and saying so once is the whole
/// of the right response.
pub fn start(format: Format) -> Result<Playing, String> {
    #[cfg(target_os = "linux")]
    {
        let sink = esper_audio_pulse::Playback::open(format).map_err(|e| e.to_string())?;
        let backend = esper_audio::Sink::backend(&sink);
        let (frames, incoming) = std::sync::mpsc::sync_channel(BACKLOG);
        std::thread::spawn(move || run(sink, &incoming));
        Ok(Playing {
            frames,
            backend,
            dropped: 0,
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = format;
        Err("no audio backend is compiled in for this platform".to_owned())
    }
}

/// The thread: play whatever arrives, until nothing more will.
fn run<K: Sink>(mut sink: K, incoming: &Receiver<Frame>) {
    while let Ok(frame) = incoming.recv() {
        if let Err(e) = sink.play(&frame) {
            // The speaker is gone. Said once, because the alternative is one
            // line per 20 milliseconds.
            tracing::warn!("the sound stopped: {e}");
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    /// A sink that keeps what it was given.
    #[derive(Debug, Default)]
    struct Recording {
        played: std::sync::Arc<std::sync::Mutex<Vec<Frame>>>,
    }

    impl Sink for Recording {
        fn backend(&self) -> &'static str {
            "recording"
        }

        fn format(&self) -> Format {
            esper_audio::SENT
        }

        fn play(&mut self, frame: &Frame) -> Result<(), esper_audio::AudioError> {
            if let Ok(mut held) = self.played.lock() {
                held.push(frame.clone());
            }
            Ok(())
        }
    }

    /// Frames handed over are played, in order.
    #[test]
    fn frames_are_played_in_the_order_they_arrive() {
        let sink = Recording::default();
        let played = std::sync::Arc::clone(&sink.played);
        let (tx, rx) = std::sync::mpsc::sync_channel(BACKLOG);
        for at in [0u64, 960, 1920] {
            tx.send(Frame::silence(&esper_audio::SENT, at))
                .expect("sent");
        }
        drop(tx);

        run(sink, &rx);

        let held = played.lock().expect("the lock");
        assert_eq!(
            held.iter().map(|f| f.at).collect::<Vec<_>>(),
            vec![0, 960, 1920]
        );
    }

    /// **A speaker that stops does not stall the loop for ever.** The thread
    /// returns, and the caller finds out the next time it hands a frame over.
    #[test]
    fn a_speaker_that_fails_stops_the_thread() {
        #[derive(Debug)]
        struct Broken;
        impl Sink for Broken {
            fn backend(&self) -> &'static str {
                "broken"
            }
            fn format(&self) -> Format {
                esper_audio::SENT
            }
            fn play(&mut self, _frame: &Frame) -> Result<(), esper_audio::AudioError> {
                Err(esper_audio::AudioError::Backend {
                    backend: "broken",
                    detail: "gone".to_owned(),
                })
            }
        }
        let (tx, rx) = std::sync::mpsc::sync_channel(BACKLOG);
        for _ in 0..3 {
            tx.send(Frame::silence(&esper_audio::SENT, 0))
                .expect("sent");
        }
        // Returns on the first failure rather than draining: the test would
        // hang on a `send` into a full channel if it did not.
        run(Broken, &rx);
    }

    /// **The frame is dropped when the speaker is behind, and counted.** This
    /// is the A/V policy: sound is never queued into a delay.
    #[test]
    fn a_frame_is_dropped_rather_than_queued_when_the_speaker_is_behind() {
        let (frames, held) = std::sync::mpsc::sync_channel(BACKLOG);
        let mut playing = Playing {
            frames,
            backend: "recording",
            dropped: 0,
        };
        // Nothing is reading `held`, so the channel fills and then refuses.
        for at in 0..(BACKLOG as u64 + 5) {
            assert!(playing.play(Frame::silence(&esper_audio::SENT, at * 960)));
        }
        assert_eq!(playing.dropped(), 5, "five frames past the backlog");
        drop(held);
        assert!(
            !playing.play(Frame::silence(&esper_audio::SENT, 0)),
            "a caller should be told when playback has gone"
        );
    }
}
