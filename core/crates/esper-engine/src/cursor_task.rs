//! Reading the host's pointer and saying what changed.
//!
//! # Why a thread of its own
//!
//! A mouse moves faster than a screen changes. The sending loop is driven by
//! capture, which blocks until something on the screen moves — on a quiet
//! desktop that is once every couple of hundred milliseconds — so a pointer
//! read there would be a pointer sampled at five hertz.
//!
//! So the cursor gets a thread and its own X connection, polls at a rate a
//! pointer deserves, and hands over only the messages worth sending. Almost
//! all of the time there are none: a pointer sitting still says nothing.

// Nothing in this module runs on a platform with no backend for it: `start`
// refuses and the session says so. It is still compiled everywhere, which is
// deliberate — the thread below is generic over the trait, so it is the part a
// new platform reuses rather than rewrites, and compiling it everywhere is
// what keeps it honest.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use esper_capture::cursor::CursorSource;
use esper_proto::CursorMessage;
use esper_stream::CursorTracker;

/// How often the pointer is looked at.
///
/// Eight milliseconds. The reading itself is one round trip to the X server on
/// a socket — tens of microseconds — and the tracker throws away everything
/// that has not changed, so the cost of looking often is a syscall and a
/// comparison.
const TICK: Duration = Duration::from_millis(8);

/// What the session tells the thread.
#[derive(Debug)]
enum Command {
    /// The monitor being captured changed, or its geometry did.
    Watch {
        monitor: u32,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
    },
    Stop,
}

/// The handle the session holds.
#[derive(Debug)]
pub struct CursorWatch {
    to_thread: Sender<Command>,
    backend: &'static str,
}

impl CursorWatch {
    /// Which backend is reading the pointer.
    #[must_use]
    pub const fn backend(&self) -> &'static str {
        self.backend
    }

    /// Watch this monitor: positions are a fraction of it.
    ///
    /// **Called again whenever the session's monitor changes.** Without it a
    /// switch would leave every position expressed as a fraction of the
    /// monitor that is no longer on screen — a pointer drawn at the right
    /// fraction of the wrong screen.
    pub fn watch(&self, monitor: u32, x: i32, y: i32, width: u32, height: u32) {
        let _ = self.to_thread.send(Command::Watch {
            monitor,
            x,
            y,
            width,
            height,
        });
    }
}

impl Drop for CursorWatch {
    fn drop(&mut self) {
        let _ = self.to_thread.send(Command::Stop);
    }
}

/// What the thread produces, for the session to put on the control stream.
pub type Messages = tokio::sync::mpsc::Receiver<CursorMessage>;

/// Start reading the pointer.
///
/// `Err` when this machine has no way to see one — a synthetic screen has no
/// pointer, and a session over it should say so rather than invent one.
pub fn start(monitor: (u32, i32, i32, u32, u32)) -> Result<(CursorWatch, Messages), String> {
    #[cfg(target_os = "linux")]
    {
        let source = esper_capture_x11::cursor::X11Cursor::open().map_err(|e| e.to_string())?;
        let backend = esper_capture::cursor::CursorSource::backend(&source);
        let (to_thread, commands) = std::sync::mpsc::channel();
        // **Bounded, and a full queue drops the oldest position rather than
        // blocking.** A controller that has stopped reading its control stream
        // must not be able to stall the thread that reads the pointer — and a
        // position nobody has sent yet is worth less than the one after it.
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let _ = to_thread.send(Command::Watch {
            monitor: monitor.0,
            x: monitor.1,
            y: monitor.2,
            width: monitor.3,
            height: monitor.4,
        });
        std::thread::spawn(move || run(source, commands, tx));
        Ok((CursorWatch { to_thread, backend }, rx))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = monitor;
        Err("no cursor backend is compiled in for this platform".to_owned())
    }
}

/// The thread: read, decide, hand over.
fn run<S: CursorSource>(
    mut source: S,
    commands: Receiver<Command>,
    out: tokio::sync::mpsc::Sender<CursorMessage>,
) {
    let mut tracker = CursorTracker::new();
    let mut watching: Option<(u32, i32, i32, u32, u32)> = None;
    let mut complained = false;
    loop {
        loop {
            match commands.try_recv() {
                Ok(Command::Watch {
                    monitor,
                    x,
                    y,
                    width,
                    height,
                }) => {
                    // A different monitor means every position said so far was
                    // about somewhere else, so the far end starts again.
                    if watching.map(|w| w.0) != Some(monitor) {
                        tracker.forget();
                    }
                    watching = Some((monitor, x, y, width, height));
                }
                Ok(Command::Stop) | Err(TryRecvError::Disconnected) => return,
                Err(TryRecvError::Empty) => break,
            }
        }
        let Some(monitor) = watching else {
            std::thread::sleep(TICK);
            continue;
        };

        match source.read() {
            Ok(Some(state)) => {
                for message in tracker.update(&state, monitor, Instant::now()) {
                    // `try_send`: see the channel's own note. A dropped
                    // position is superseded by the next one a few
                    // milliseconds later; a dropped *shape* would not be, so
                    // the tracker is told to say it again.
                    if out.try_send(message).is_err() {
                        tracker.forget();
                    }
                }
            }
            // Nothing to see: a backend that cannot report a pointer, which is
            // not a fault.
            Ok(None) => {}
            Err(e) => {
                if !complained {
                    complained = true;
                    tracing::warn!("cannot read the pointer any more: {e}");
                }
            }
        }
        std::thread::sleep(TICK);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use esper_capture::cursor::{CursorImage, CursorState, ScriptedCursor};
    use std::sync::mpsc::channel;

    fn state(serial: u32, x: i32, y: i32) -> CursorState {
        CursorState {
            serial,
            image: Some(CursorImage {
                width: 1,
                height: 1,
                hotspot_x: 0,
                hotspot_y: 0,
                rgba: vec![9, 9, 9, 255],
            }),
            x,
            y,
        }
    }

    /// The thread reads, decides and hands over — checked with no display.
    ///
    /// Driven the way production drives it: started, told what to watch, and
    /// then stopped once it has said something. Queueing the stop up front
    /// instead would end the thread before its first reading, which is what
    /// the first version of this test did — and it proved only that a thread
    /// told to stop stops.
    #[test]
    fn it_reports_a_pointer_it_was_told_to_watch() {
        let source = ScriptedCursor::new(vec![Some(state(1, 10, 10))]);
        let (to_thread, commands) = channel();
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        to_thread
            .send(Command::Watch {
                monitor: 0,
                x: 0,
                y: 0,
                width: 100,
                height: 100,
            })
            .expect("send");
        let thread = std::thread::spawn(move || run(source, commands, tx));

        // The first reading is one tick away; waiting far longer than that
        // costs nothing and makes this insensitive to a busy machine.
        let mut got = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while got.len() < 2 && Instant::now() < deadline {
            match rx.try_recv() {
                Ok(m) => got.push(m),
                Err(_) => std::thread::sleep(TICK),
            }
        }
        let _ = to_thread.send(Command::Stop);
        let _ = thread.join();

        assert_eq!(got.len(), 2, "a shape and a position: {got:?}");
        assert!(
            got.iter().any(|m| matches!(m, CursorMessage::Shape(_))),
            "one of them is the shape: {got:?}"
        );
        assert!(
            got.iter().any(|m| matches!(m, CursorMessage::Moved(_))),
            "and one is the position: {got:?}"
        );
    }

    /// **Told nothing to watch, it says nothing.** A session that has not
    /// announced its monitor yet must not have positions attributed to a
    /// monitor nobody named.
    ///
    /// Run for real ticks rather than stopped up front: a thread that stopped
    /// before its first reading would pass either way, which is what makes
    /// that shape of test worthless.
    #[test]
    fn with_no_monitor_to_watch_it_says_nothing() {
        let source = ScriptedCursor::new(vec![Some(state(1, 10, 10))]);
        let (to_thread, commands) = channel();
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let thread = std::thread::spawn(move || run(source, commands, tx));
        std::thread::sleep(TICK * 20);
        let _ = to_thread.send(Command::Stop);
        let _ = thread.join();
        assert!(
            rx.try_recv().is_err(),
            "nothing should be said about a monitor nobody named"
        );
    }

    /// A backend that cannot see a pointer produces nothing, and that is not
    /// an error — a synthetic screen has no pointer.
    #[test]
    fn a_source_with_no_pointer_produces_nothing() {
        let source = ScriptedCursor::new(vec![None]);
        let (to_thread, commands) = channel();
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        to_thread
            .send(Command::Watch {
                monitor: 0,
                x: 0,
                y: 0,
                width: 100,
                height: 100,
            })
            .expect("send");
        let thread = std::thread::spawn(move || run(source, commands, tx));
        std::thread::sleep(TICK * 20);
        let _ = to_thread.send(Command::Stop);
        let _ = thread.join();
        assert!(rx.try_recv().is_err());
    }

    /// **A pointer sitting still says it once and then nothing**, which is
    /// what keeps a quiet desktop quiet: twenty ticks of reading the same
    /// pointer produce the one shape and the one position.
    #[test]
    fn a_still_pointer_is_reported_once() {
        let source = ScriptedCursor::new(vec![Some(state(1, 10, 10))]);
        let (to_thread, commands) = channel();
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        to_thread
            .send(Command::Watch {
                monitor: 0,
                x: 0,
                y: 0,
                width: 100,
                height: 100,
            })
            .expect("send");
        let thread = std::thread::spawn(move || run(source, commands, tx));
        std::thread::sleep(TICK * 25);
        let _ = to_thread.send(Command::Stop);
        let _ = thread.join();

        let mut got = 0;
        while rx.try_recv().is_ok() {
            got += 1;
        }
        assert_eq!(
            got, 2,
            "a shape and a position, and nothing for the twenty-odd readings after"
        );
    }

    /// Dropping the handle stops the thread rather than leaving it reading a
    /// pointer nobody is listening to.
    #[test]
    fn a_disconnected_command_channel_stops_the_thread() {
        let source = ScriptedCursor::new(vec![Some(state(1, 10, 10))]);
        let (to_thread, commands) = channel();
        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        drop(to_thread);
        // Returns rather than looping for ever, which is what the process
        // going away looks like from in here.
        run(source, commands, tx);
    }
}
