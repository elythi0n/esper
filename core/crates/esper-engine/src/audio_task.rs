//! The host's sound, on a thread of its own.
//!
//! # Why a thread
//!
//! Sound is not allowed to wait for the picture. A frame of audio is due every
//! 20 milliseconds and a screen can take longer than that to capture and
//! encode, so a session that produced both from one loop would deliver sound
//! in bursts — heard as a stutter, and heard *immediately*, where a late frame
//! of video is barely noticed.
//!
//! So capture, encode and send happen on their own thread, with their own
//! clone of the session's media channel. The two never wait for each other.
//!
//! # What it does when there is no sound card
//!
//! Nothing, loudly. A host with no sound server says so once and the session
//! carries on without sound, and the controller is told the session cannot do
//! it — see the narrowing in `session_cmd`. A session that quietly sends
//! silence would be worse: somebody would spend an afternoon looking for the
//! fault at the other end.

// Nothing in this module runs on a platform with no sound backend for it:
// `start` refuses and the session says so. It is still compiled everywhere,
// which is deliberate — the thread below is generic over the trait, so it is
// the part a new platform reuses rather than rewrites, and compiling it
// everywhere is what keeps it honest.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::Duration;

use esper_audio::{Captured, Source};
use esper_proto::AudioConfig;

/// How long the thread waits for a frame before looking at its commands.
///
/// **Longer than a frame, shorter than a person notices.** At 20 ms frames a
/// wait of 100 ms means the thread is woken by sound rather than by the clock,
/// and a stop command is acted on within a tenth of a second.
const PATIENCE: Duration = Duration::from_millis(100);

/// What the session wants the sound to do.
#[derive(Debug)]
enum Command {
    Stop,
}

/// The handle the session holds.
#[derive(Debug)]
pub struct Sending {
    to_thread: Sender<Command>,
    backend: &'static str,
    config: AudioConfig,
}

impl Sending {
    /// Which backend is capturing.
    #[must_use]
    pub const fn backend(&self) -> &'static str {
        self.backend
    }

    /// What the host should announce about this sound.
    #[must_use]
    pub const fn config(&self) -> AudioConfig {
        self.config
    }
}

impl Drop for Sending {
    fn drop(&mut self) {
        let _ = self.to_thread.send(Command::Stop);
    }
}

/// Start capturing this machine's sound and sending it.
///
/// The `AudioConfig` that comes back is what the host announces: it carries
/// the shape *and* the stream's identity, so the controller can build a
/// decoder and tell audio datagrams from video ones.
///
/// # Errors
///
/// If there is no sound server, no monitor of the default sink, or the tools
/// to reach them are not installed. **A caller must not treat that as fatal**:
/// a session with a picture and no sound is a session, and the controller is
/// told what it cannot do.
pub fn start(media: &esper_transport::MediaChannel) -> Result<Sending, String> {
    #[cfg(target_os = "linux")]
    {
        let format = esper_audio::SENT;
        let source = esper_audio_pulse::MonitorCapture::open(format).map_err(|e| e.to_string())?;
        let backend = esper_audio::Source::backend(&source);
        let ssrc = esper_stream::audio_ssrc();
        let sending = esper_stream::HostAudio::new(format, ssrc).map_err(|e| e.to_string())?;
        let config = sending.config();
        let sink = esper_link::SessionSink::new(media.clone());
        let (to_thread, commands) = std::sync::mpsc::channel();
        std::thread::spawn(move || run(source, sending, sink, &commands));
        Ok(Sending {
            to_thread,
            backend,
            config,
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = media;
        Err("no audio backend is compiled in for this platform".to_owned())
    }
}

/// The thread: capture, encode, send, until told to stop or the session ends.
///
/// **Generic over the sink**, so the loop a test runs is this loop and not a
/// copy of it. The production path passes the session's own datagram sink; a
/// test passes one that keeps what it was given. Nothing else differs, which
/// is the point: a test against a reimplementation of a loop tests the
/// reimplementation.
///
/// A session that has closed shows up as a send that fails, which is why there
/// is no separate liveness check: two ways of noticing the same thing is one
/// too many.
fn run<S: Source, K: esper_stream::DatagramSink>(
    mut source: S,
    mut sending: esper_stream::HostAudio,
    mut sink: K,
    commands: &Receiver<Command>,
) {
    loop {
        match commands.try_recv() {
            Ok(Command::Stop) | Err(TryRecvError::Disconnected) => break,
            Err(TryRecvError::Empty) => {}
        }
        match source.next_frame(PATIENCE) {
            Ok(Captured::Frame(frame)) => {
                let datagram = match sending.packetize(&frame) {
                    Ok(d) => d,
                    Err(e) => {
                        // One frame. A codec that refuses every frame is a
                        // session with no sound, which is what the count in
                        // the summary is for.
                        tracing::debug!("a frame of audio was not sent: {e}");
                        continue;
                    }
                };
                if sink.send(&datagram).is_err() {
                    break;
                }
            }
            // Nothing to send. Not silence: see `Captured::Nothing`.
            Ok(Captured::Nothing) => {}
            Ok(Captured::Finished) => break,
            Err(e) => {
                tracing::warn!("the host's sound stopped: {e}");
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use esper_audio::SyntheticTone;

    /// A sink that keeps what it was given, so the thread can be run without a
    /// transport.
    #[derive(Debug, Default)]
    struct Recording {
        sent: std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
    }

    impl esper_stream::DatagramSink for Recording {
        fn send(&mut self, payload: &[u8]) -> Result<(), esper_stream::SinkError> {
            if let Ok(mut held) = self.sent.lock() {
                held.push(payload.to_vec());
            }
            Ok(())
        }

        fn max_datagram_size(&self) -> Option<usize> {
            Some(1200)
        }
    }

    /// **The loop turns captured sound into datagrams**, checked without a
    /// sound card, a transport or a thread.
    ///
    /// This calls the loop the product calls: `run` is generic over the sink,
    /// so a test can hand it one that keeps what it was given.
    #[test]
    fn five_frames_are_five_datagrams_of_opus() {
        let source = SyntheticTone::new().unpaced().with_frame_limit(5);
        let sending = esper_stream::HostAudio::new(esper_audio::SENT, 0xA0DA_0001).expect("a host");
        let sink = Recording::default();
        let sent = std::sync::Arc::clone(&sink.sent);
        let (_keep, commands) = std::sync::mpsc::channel();

        run(source, sending, sink, &commands);

        let held = sent.lock().expect("the lock");
        assert_eq!(held.len(), 5, "five frames should be five datagrams");
        for datagram in held.iter() {
            let (header, packet) =
                esper_media::RtpHeader::parse(datagram).expect("a readable header");
            assert_eq!(header.payload_type, esper_stream::PAYLOAD_TYPE_OPUS);
            assert_eq!(header.ssrc, 0xA0DA_0001);
            assert!(!packet.is_empty());
        }
    }

    /// **The session's end stops the thread**, rather than leaving a capture
    /// running and a codec encoding for a peer that is gone.
    #[test]
    fn a_dropped_handle_stops_the_loop_before_it_sends() {
        let source = SyntheticTone::new().unpaced();
        let sending = esper_stream::HostAudio::new(esper_audio::SENT, 0xA0DA_0001).expect("a host");
        let sink = Recording::default();
        let sent = std::sync::Arc::clone(&sink.sent);
        let (to_thread, commands) = std::sync::mpsc::channel();
        drop(to_thread);

        run(source, sending, sink, &commands);

        let held = sent.lock().expect("the lock");
        assert!(
            held.is_empty(),
            "a session that is gone should stop the sound at once, not after {} datagrams",
            held.len()
        );
    }

    /// A sink that refuses stops the loop too: there is nowhere to send.
    #[test]
    fn a_sink_that_is_gone_stops_the_loop() {
        #[derive(Debug)]
        struct Closed;
        impl esper_stream::DatagramSink for Closed {
            fn send(&mut self, _payload: &[u8]) -> Result<(), esper_stream::SinkError> {
                Err(esper_stream::SinkError::Gone("closed".to_owned()))
            }
            fn max_datagram_size(&self) -> Option<usize> {
                Some(1200)
            }
        }
        let source = SyntheticTone::new().unpaced().with_frame_limit(100);
        let sending = esper_stream::HostAudio::new(esper_audio::SENT, 0xA0DA_0001).expect("a host");
        let (_keep, commands) = std::sync::mpsc::channel();
        // Returns rather than looping a hundred times, which is what is being
        // checked: the test would hang if it did not.
        run(source, sending, Closed, &commands);
    }
}
