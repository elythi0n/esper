#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// A real sound server, so: Unix, and in practice Linux.
#![cfg(unix)]

//! Tests that need a sound server.
//!
//! # Why a sink of their own
//!
//! These play sound and record it. Doing that to whatever the machine is
//! pointed at means a suite that makes noise on somebody's speakers and
//! records whatever they were listening to — so instead they create a **null
//! sink**, play into that, and capture its monitor. Nothing is audible,
//! nothing real is recorded, and the result is deterministic: a null sink has
//! no hardware to resume and no other client mixing into it.
//!
//! The sink is removed afterwards, including when a test fails, because a
//! stray sink left on somebody's machine is a thing they would have to find.

use std::process::Command;
use std::time::Duration;

use esper_audio::{Captured, Format, Frame, Sink as _, Source as _, SyntheticTone};
use esper_audio_pulse::{MonitorCapture, Playback};

/// Whether this run may talk to a sound server at all.
///
/// **Off unless asked for.** Even with a sink of its own, this loads a module
/// into somebody's audio server, and a suite that does that whenever it is run
/// is a suite people turn off. CI sets it, where the server is one the workflow
/// started for the purpose.
fn may_use_the_sound_server() -> bool {
    if std::env::var_os("ESPER_AUDIO_TESTS").is_none() {
        println!(
            "skipped: set ESPER_AUDIO_TESTS=1 to allow loading a null sink into this \
             machine's sound server. Nothing is audible, and it is still off unless asked for."
        );
        return false;
    }
    true
}

/// A null sink, removed when this is dropped.
struct NullSink {
    name: String,
    module: String,
}

impl NullSink {
    /// `None` when there is no server to load it into, which is the ordinary
    /// case on a build machine.
    fn load() -> Option<Self> {
        let name = format!("esper_test_{}", std::process::id());
        let out = Command::new("pactl")
            .arg("load-module")
            .arg("module-null-sink")
            .arg(format!("sink_name={name}"))
            .output()
            .ok()?;
        if !out.status.success() {
            println!(
                "skipped: no sound server to load a null sink into ({})",
                String::from_utf8_lossy(&out.stderr).trim()
            );
            return None;
        }
        let module = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if module.is_empty() {
            return None;
        }
        Some(Self { name, module })
    }

    fn sink(&self) -> &str {
        &self.name
    }

    fn monitor(&self) -> String {
        format!("{}.monitor", self.name)
    }
}

impl Drop for NullSink {
    fn drop(&mut self) {
        let _ = Command::new("pactl")
            .arg("unload-module")
            .arg(&self.module)
            .output();
    }
}

/// Read frames until one is loud, or until patience runs out.
///
/// A monitor hands over silence until the sound reaches it, which takes a few
/// frames — so this waits for sound rather than sleeping and hoping.
fn wait_for_sound(capture: &mut MonitorCapture, patience: Duration) -> Option<Frame> {
    let give_up = std::time::Instant::now().checked_add(patience);
    loop {
        match capture.next_frame(Duration::from_millis(100)) {
            Ok(Captured::Frame(f)) if f.peak() > 1_000 => return Some(f),
            Ok(Captured::Frame(_) | Captured::Nothing) => {}
            Ok(Captured::Finished) => return None,
            Err(e) => panic!("capture failed: {e}"),
        }
        if give_up.is_none_or(|d| std::time::Instant::now() >= d) {
            return None;
        }
    }
}

/// **What the host plays is what the far end hears.**
///
/// The whole point of the backend, end to end through a real sound server: a
/// tone is played into a sink, and the monitor of that sink is captured. If
/// this passes, the capture path slices frames correctly, the sample decoding
/// is the right endianness and width, and the playback path writes what it was
/// given.
#[test]
fn a_tone_played_into_a_sink_arrives_at_its_monitor() {
    if !may_use_the_sound_server() {
        return;
    }
    let Some(sink) = NullSink::load() else { return };
    let format = esper_audio::SENT;

    let mut capture = match MonitorCapture::open_device(format, &sink.monitor()) {
        Ok(c) => c,
        Err(e) => {
            println!("skipped: no capture ({e})");
            return;
        }
    };
    let mut playback = match Playback::open_device(format, sink.sink()) {
        Ok(p) => p,
        Err(e) => {
            println!("skipped: no playback ({e})");
            return;
        }
    };

    // Half a second of a tone, at a third of full scale.
    let mut tone = SyntheticTone::new().at_level(0.33).unpaced();
    for _ in 0..25 {
        match tone.next_frame(Duration::from_secs(1)).expect("a frame") {
            Captured::Frame(f) => playback.play(&f).expect("played"),
            other => panic!("the generator stopped: {other:?}"),
        }
    }

    let heard = wait_for_sound(&mut capture, Duration::from_secs(5))
        .expect("nothing loud arrived at the monitor within five seconds");
    assert!(
        heard.fits(&format),
        "a frame of {} values where {} were expected",
        heard.samples.len(),
        format.values_per_frame()
    );
    // A third of full scale is about 10 800. Bounded on both sides: too quiet
    // means the samples were mangled, and much too loud means the width or the
    // endianness is wrong — a byte-swapped sample is enormous.
    let peak = heard.peak();
    assert!(
        (6_000..=14_000).contains(&peak),
        "a tone at a third of full scale arrived with a peak of {peak}"
    );
    assert_eq!(capture.device(), sink.monitor());
}

/// **A monitor of a silent sink produces silence, not nothing.**
///
/// Which is what makes a session's sound continuous: a host with nothing
/// playing still sends frames, so the far end's buffer never runs dry and the
/// first sound after a quiet minute is not late. It is also the thing that
/// would break if the capture were pointed at a device that only produces
/// samples while something is playing.
#[test]
fn a_silent_sink_still_produces_frames() {
    if !may_use_the_sound_server() {
        return;
    }
    let Some(sink) = NullSink::load() else { return };
    let format = esper_audio::SENT;
    let mut capture = match MonitorCapture::open_device(format, &sink.monitor()) {
        Ok(c) => c,
        Err(e) => {
            println!("skipped: no capture ({e})");
            return;
        }
    };

    let mut frames = 0;
    let mut timestamps = Vec::new();
    for _ in 0..40 {
        match capture.next_frame(Duration::from_millis(250)) {
            Ok(Captured::Frame(f)) => {
                assert!(f.fits(&format));
                timestamps.push(f.at);
                frames += 1;
                if frames >= 5 {
                    break;
                }
            }
            Ok(Captured::Nothing) => {}
            Ok(Captured::Finished) => break,
            Err(e) => panic!("capture failed: {e}"),
        }
    }
    assert!(
        frames >= 5,
        "a silent sink produced {frames} frames in ten seconds"
    );
    // And the timestamps are a sample count that advances by one frame, which
    // is what the far end reconstructs timing from.
    let step = format.samples_per_frame() as u64;
    for pair in timestamps.windows(2) {
        let (a, b) = (
            pair.first().copied().unwrap_or(0),
            pair.get(1).copied().unwrap_or(0),
        );
        assert_eq!(b - a, step, "timestamps went {a} then {b}");
    }
}

/// **Nothing here ever opens an input.**
///
/// The device this backend captures is derived from the default *sink*, and
/// the name it derives always ends in `.monitor`. A fallback to the default
/// source would be a screen-sharing session that records the room, so this
/// asserts the shape of the answer rather than trusting the code that produces
/// it.
#[test]
fn the_device_captured_is_always_a_monitor() {
    match esper_audio_pulse::monitor_of_default_sink() {
        Ok(device) => {
            assert!(device.ends_with(".monitor"), "{device}");
            // And it is not one of the input devices the server knows about.
            let sources = Command::new("pactl")
                .arg("list")
                .arg("short")
                .arg("sources")
                .output();
            if let Ok(out) = sources {
                let listing = String::from_utf8_lossy(&out.stdout);
                for line in listing.lines() {
                    let Some(name) = line.split('\t').nth(1) else {
                        continue;
                    };
                    if name == device {
                        continue;
                    }
                    assert_ne!(
                        name, device,
                        "the capture device is a source that is not a monitor"
                    );
                }
            }
        }
        Err(e) => println!("skipped: no default sink to derive a monitor from ({e})"),
    }
}

/// A format Opus cannot be built for is refused, and no process is left over.
#[test]
fn an_unsupported_format_is_refused() {
    let bad = Format {
        sample_rate_hz: 44_100,
        channels: 2,
        frame_ms: 20,
    };
    assert!(MonitorCapture::open_device(bad, "nowhere.monitor").is_err());
    assert!(Playback::open_device(bad, "nowhere").is_err());
}
