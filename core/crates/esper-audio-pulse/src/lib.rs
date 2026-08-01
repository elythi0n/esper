//! Sound in and out, over the PulseAudio protocol.
//!
//! # Why the protocol and not the library
//!
//! Every Linux desktop worth naming speaks it: PulseAudio itself, and PipeWire
//! through `pipewire-pulse`, which every PipeWire desktop ships. Targeting one
//! protocol reaches both, where linking `libpipewire` would reach one and
//! linking `libpulse` would pull a client library into a process whose whole
//! job is to be small.
//!
//! # Why a subprocess
//!
//! `parec` and `pacat` are the protocol's own command-line clients: they take
//! a format and a device, and they move raw samples over a pipe. Using them
//! means this crate links **no** audio library, keeps `forbid(unsafe_code)`,
//! and cannot be the thing that segfaults a host — and a pipe carrying 192
//! kilobytes a second is not a cost worth optimising against a codec that
//! costs more.
//!
//! The price is a runtime dependency on those two programs, which is why the
//! absence of either is reported by name rather than as "no audio".
//!
//! # The rule that matters
//!
//! **A monitor of the output, named explicitly, or nothing.**
//!
//! `parec` with no device records from the default *source*, and on almost
//! every machine the default source is a microphone. A fallback of that shape
//! would turn a screen-sharing session into a listening device — silently, and
//! exactly when the intended device was missing. So the monitor is resolved
//! from the default sink by name, and a failure to resolve it is a refusal.
//! There is no path here that opens an input.

#![forbid(unsafe_code)]

use std::io::Read as _;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::time::Duration;

use esper_audio::{AudioError, Captured, Format, Frame, Sink, Source};

/// The backend name reported in logs and the session summary.
pub const BACKEND: &str = "pulse";

/// What is asked of the server, in milliseconds.
///
/// **Twenty, the same as a frame.** Asking for less makes the server wake more
/// often for no gain, since nothing here can act on sound faster than one
/// frame at a time; asking for more adds delay to a path where delay is heard
/// as somebody talking out of step with their mouth.
const LATENCY_MS: u32 = 20;

/// How many frames may wait between the reader thread and the caller.
///
/// **Half a second, and the oldest is dropped when it is full.** A caller that
/// has stopped asking for sound — a session tearing down, a link that stopped
/// draining — must not be able to grow this without bound, and sound that is
/// half a second late is not worth playing: the far end has already heard the
/// silence.
const BACKLOG: usize = 25;

/// Which of the protocol's clients to use for each direction.
///
/// Looked up rather than assumed, so a machine missing one of them is told
/// which one.
fn program(name: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

/// The monitor of the default sink, by name.
///
/// `None` when it cannot be worked out — a missing `pactl`, no server running,
/// or a server with no default sink. **Never a guess**: see this module's
/// documentation for what guessing would open.
///
/// # Errors
///
/// Reports what was tried, because "no audio" is not something an operator can
/// act on and "pactl is not installed" is.
pub fn monitor_of_default_sink() -> Result<String, AudioError> {
    let pactl = program("pactl").ok_or_else(|| AudioError::NoBackend {
        tried: "pactl, to ask which sink is the default one".to_owned(),
    })?;
    let out = Command::new(&pactl)
        .arg("get-default-sink")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| AudioError::Backend {
            backend: BACKEND,
            detail: format!("running pactl: {e}"),
        })?;
    if !out.status.success() {
        return Err(AudioError::NoBackend {
            tried: format!(
                "pactl get-default-sink, which failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        });
    }
    let sink = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    // A server with no default sink says so in various ways depending on its
    // version; all of them are "there is nothing to capture", and none of them
    // is a device name.
    if sink.is_empty() || sink == "@DEFAULT_SINK@" || sink.contains(char::is_whitespace) {
        return Err(AudioError::NoBackend {
            tried: format!("pactl get-default-sink, which answered {sink:?}"),
        });
    }
    Ok(format!("{sink}.monitor"))
}

/// Sound from what this machine is playing.
#[derive(Debug)]
pub struct MonitorCapture {
    format: Format,
    device: String,
    frames: Receiver<Frame>,
    child: Child,
    /// Set once the reader thread has stopped, so `Finished` is reported once
    /// the last frame has been handed over rather than the moment the pipe
    /// closed.
    ended: bool,
}

impl MonitorCapture {
    /// Start capturing what the default sink is playing.
    ///
    /// # Errors
    ///
    /// If the monitor cannot be named, `parec` is not installed, or the format
    /// is not one Opus can be built for.
    pub fn open(format: Format) -> Result<Self, AudioError> {
        let device = monitor_of_default_sink()?;
        Self::open_device(format, &device)
    }

    /// The same, for a named device.
    ///
    /// **The name is a monitor, and this does not check that.** It exists for
    /// tests, which need a device that is not whatever the machine happens to
    /// be pointed at. Nothing in the product calls it: a session captures the
    /// default sink's monitor, and the safety of that comes from
    /// [`monitor_of_default_sink`].
    ///
    /// # Errors
    ///
    /// If `parec` is not installed, the format is unsupported, or the process
    /// will not start.
    pub fn open_device(format: Format, device: &str) -> Result<Self, AudioError> {
        if !format.is_supported() {
            return Err(AudioError::Unsupported {
                sample_rate_hz: format.sample_rate_hz,
                channels: format.channels,
                frame_ms: format.frame_ms,
            });
        }
        let parec = program("parec").ok_or_else(|| AudioError::NoBackend {
            tried: "parec, from PulseAudio's own tools (package `pulseaudio-utils` on Debian \
                    and Ubuntu, `libpulse` on Arch). PipeWire desktops have it through \
                    pipewire-pulse."
                .to_owned(),
        })?;
        let mut child = Command::new(&parec)
            .arg(format!("--device={device}"))
            .arg("--format=s16le")
            .arg(format!("--rate={}", format.sample_rate_hz))
            .arg(format!("--channels={}", format.channels))
            .arg(format!("--latency-msec={LATENCY_MS}"))
            .arg("--raw")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            // Kept rather than inherited: a client that cannot connect writes
            // a paragraph to stderr, and a host's log is not the place for it.
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| AudioError::Backend {
                backend: BACKEND,
                detail: format!("starting parec: {e}"),
            })?;
        let stdout = child.stdout.take().ok_or_else(|| AudioError::Backend {
            backend: BACKEND,
            detail: "parec started with no pipe".to_owned(),
        })?;

        // **A thread, because a pipe read blocks and sound does not wait.**
        // The alternative is a non-blocking read and a poll loop, which is the
        // same thread with the sleep written by hand.
        let (tx, frames) = std::sync::mpsc::sync_channel(BACKLOG);
        std::thread::spawn(move || read_frames(stdout, format, &tx));

        Ok(Self {
            format,
            device: device.to_owned(),
            frames,
            child,
            ended: false,
        })
    }

    /// Which device is being captured.
    #[must_use]
    pub fn device(&self) -> &str {
        &self.device
    }
}

/// Read whole frames off the pipe until it ends.
fn read_frames(mut pipe: std::process::ChildStdout, format: Format, out: &SyncSender<Frame>) {
    let values = format.values_per_frame();
    let mut bytes = vec![0u8; values.saturating_mul(2)];
    let mut at = 0u64;
    loop {
        // **Exactly one frame, or nothing.** A pipe hands over whatever is
        // available, so a frame arrives in pieces — and a consumer given
        // three quarters of a frame would encode three quarters of a frame,
        // which Opus refuses, once per frame, for the life of the session.
        if pipe.read_exact(&mut bytes).is_err() {
            return;
        }
        let samples: Vec<i16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| i16::from_le_bytes(*pair))
            .collect();
        let frame = Frame { samples, at };
        at = at.saturating_add(format.samples_per_frame() as u64);
        match out.try_send(frame) {
            Ok(()) => {}
            // Nobody is listening any more.
            Err(TrySendError::Disconnected(_)) => return,
            // The caller is behind. Dropped rather than blocked: see BACKLOG.
            Err(TrySendError::Full(_)) => {
                tracing::debug!("a frame of audio was dropped: the session is not keeping up");
            }
        }
    }
}

impl Source for MonitorCapture {
    fn backend(&self) -> &'static str {
        BACKEND
    }

    fn format(&self) -> Format {
        self.format
    }

    fn next_frame(&mut self, timeout: Duration) -> Result<Captured, AudioError> {
        if self.ended {
            return Ok(Captured::Finished);
        }
        match self.frames.recv_timeout(timeout) {
            Ok(frame) => Ok(Captured::Frame(frame)),
            Err(RecvTimeoutError::Timeout) => Ok(Captured::Nothing),
            Err(RecvTimeoutError::Disconnected) => {
                self.ended = true;
                Ok(Captured::Finished)
            }
        }
    }
}

impl Drop for MonitorCapture {
    fn drop(&mut self) {
        // **Killed, not waited for.** `parec` streams until its pipe closes,
        // and its pipe is held by the reader thread — so waiting here would
        // wait for a program that is waiting for us.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Sound out of this machine's speakers.
#[derive(Debug)]
pub struct Playback {
    format: Format,
    child: Child,
    pipe: Option<std::process::ChildStdin>,
}

impl Playback {
    /// Start playing to the default sink.
    ///
    /// # Errors
    ///
    /// If `pacat` is not installed, the format is unsupported, or the process
    /// will not start.
    pub fn open(format: Format) -> Result<Self, AudioError> {
        Self::start(format, None)
    }

    /// The same, to a named sink.
    ///
    /// **For tests**, which need to play somewhere that is not whatever this
    /// machine is pointed at — a sink created for the test, so a suite makes
    /// no noise on somebody's speakers. A session plays to the default sink,
    /// because that is where the person watching expects sound to come from.
    ///
    /// # Errors
    ///
    /// As [`Self::open`].
    pub fn open_device(format: Format, device: &str) -> Result<Self, AudioError> {
        Self::start(format, Some(device))
    }

    fn start(format: Format, device: Option<&str>) -> Result<Self, AudioError> {
        if !format.is_supported() {
            return Err(AudioError::Unsupported {
                sample_rate_hz: format.sample_rate_hz,
                channels: format.channels,
                frame_ms: format.frame_ms,
            });
        }
        let pacat = program("pacat").ok_or_else(|| AudioError::NoBackend {
            tried: "pacat, from PulseAudio's own tools (package `pulseaudio-utils` on Debian \
                    and Ubuntu, `libpulse` on Arch)."
                .to_owned(),
        })?;
        let mut cmd = Command::new(&pacat);
        if let Some(device) = device {
            cmd.arg(format!("--device={device}"));
        }
        let mut child = cmd
            .arg("--playback")
            .arg("--format=s16le")
            .arg(format!("--rate={}", format.sample_rate_hz))
            .arg(format!("--channels={}", format.channels))
            .arg(format!("--latency-msec={LATENCY_MS}"))
            .arg("--raw")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| AudioError::Backend {
                backend: BACKEND,
                detail: format!("starting pacat: {e}"),
            })?;
        let pipe = child.stdin.take();
        Ok(Self {
            format,
            child,
            pipe,
        })
    }
}

impl Sink for Playback {
    fn backend(&self) -> &'static str {
        BACKEND
    }

    fn format(&self) -> Format {
        self.format
    }

    fn play(&mut self, frame: &Frame) -> Result<(), AudioError> {
        let want = self.format.values_per_frame();
        if frame.samples.len() != want {
            return Err(AudioError::WrongSize {
                got: frame.samples.len(),
                want,
            });
        }
        let pipe = self.pipe.as_mut().ok_or_else(|| AudioError::Backend {
            backend: BACKEND,
            detail: "playback has already stopped".to_owned(),
        })?;
        let mut bytes = Vec::with_capacity(want.saturating_mul(2));
        for s in &frame.samples {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        std::io::Write::write_all(pipe, &bytes).map_err(|e| AudioError::Backend {
            backend: BACKEND,
            detail: format!("writing to pacat: {e}"),
        })
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        // The pipe first: `pacat` plays what it has and exits, so closing the
        // pipe is how the last frame gets heard rather than cut off.
        self.pipe = None;
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    /// **The refusal names the program**, because "no audio" is not something
    /// an operator can act on and "parec is not installed" is.
    #[test]
    fn a_missing_program_is_reported_by_name() {
        // Whatever this machine has, one of the two paths below is exercised:
        // either the tools are present and a device name comes back, or they
        // are not and the message says which one is missing.
        match monitor_of_default_sink() {
            Ok(device) => {
                assert!(
                    device.ends_with(".monitor"),
                    "{device} is not a monitor, and a monitor is the only thing this may capture"
                );
            }
            Err(AudioError::NoBackend { tried }) => {
                assert!(
                    tried.contains("pactl"),
                    "the refusal should name what was tried, got {tried:?}"
                );
            }
            Err(e) => panic!("unexpected error: {e}"),
        }
    }

    /// A format Opus cannot be built for is refused before a process is
    /// started, so nothing is left running for a session that cannot happen.
    #[test]
    fn an_unsupported_format_starts_no_process() {
        let bad = Format {
            sample_rate_hz: 44_100,
            channels: 2,
            frame_ms: 20,
        };
        assert!(matches!(
            MonitorCapture::open_device(bad, "nowhere.monitor"),
            Err(AudioError::Unsupported { .. })
        ));
        assert!(matches!(
            Playback::open(bad),
            Err(AudioError::Unsupported { .. })
        ));
    }
}
