//! Putting a controller's input onto a host's screen.
//!
//! # What an `InputSink` is for
//!
//! One trait, so the engine never names a platform. X11 injects through XTEST,
//! Wayland through a portal, Windows through `SendInput`, and a test through
//! [`NullSink`], which records what it was asked to do — the same arrangement
//! the capture side uses, for the same reason: a backend nothing exercises is a
//! backend that does not work.
//!
//! # Nothing here decides who may type
//!
//! A sink injects. Whether this peer is allowed to is settled before anything
//! reaches here, by the `control` permission, and the panic key is handled
//! *before* dispatch so a controller cannot swallow it. Putting either check
//! in the sink would mean every backend implementing it, and one of them
//! eventually not.
//!
//! # Everything held gets let go
//!
//! A session can end at any moment: a network drops, a process is killed, a
//! person closes a window. **A key left held on a machine nobody is driving is
//! the worst failure this path has.** A stuck Ctrl or Alt makes the host
//! unusable for its own owner, who has no way to know why and no obvious way
//! to fix it.
//!
//! So [`Held`] tracks what is down, one implementation shared by every
//! backend, and every sink releases it — on `ReleaseAll`, and again on drop,
//! because a drop is what a dropped session looks like from here.

pub mod dispatch;
pub mod held;
pub mod hid;
pub mod null;

pub use dispatch::{Dispatcher, Monitor};
pub use held::Held;
pub use hid::{Evdev, evdev_for_usage, usage_for_evdev};
pub use null::{NullSink, Recorded};

use esper_proto::{KeyEvent, PointerButton, Scroll};

/// Anything that went wrong injecting.
#[derive(Debug, thiserror::Error)]
pub enum InputError {
    /// No injection backend is available on this machine.
    ///
    /// Its own variant because it is the one an operator can act on: a
    /// Wayland session with no portal, or an X server without XTEST, is a
    /// machine that can be *viewed* and not driven, and saying which beats a
    /// session that silently ignores every keystroke.
    #[error("no input backend: {tried}")]
    NoBackend { tried: String },

    /// The key names nothing this platform can inject.
    ///
    /// Not a protocol error: the usage is legal and this platform has no key
    /// there. A Japanese conversion key on a US keyboard is the ordinary case.
    #[error("usage {usage:#06x} has no key on this platform")]
    NoSuchKey { usage: u16 },

    /// The monitor a pointer position names does not exist.
    #[error("monitor {monitor} does not exist; this host has {count}")]
    NoSuchMonitor { monitor: u32, count: usize },

    /// The backend refused or failed.
    #[error("{backend}: {detail}")]
    Backend {
        backend: &'static str,
        detail: String,
    },
}

/// Where a pointer position lands, once a host has resolved the monitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Point {
    /// Desktop coordinates, because that is what every injection API wants.
    pub x: i32,
    pub y: i32,
}

/// Somewhere for a controller's input to go.
pub trait InputSink: std::fmt::Debug + Send {
    /// Which backend this is, for the log and the session indicator.
    fn backend(&self) -> &'static str;

    /// Press or release a key by its physical position.
    fn key(&mut self, event: KeyEvent) -> Result<(), InputError>;

    /// Move the pointer to a point on the desktop.
    fn pointer(&mut self, at: Point) -> Result<(), InputError>;

    /// Press or release a button.
    fn button(&mut self, event: PointerButton) -> Result<(), InputError>;

    /// Scroll.
    fn scroll(&mut self, scroll: Scroll) -> Result<(), InputError>;

    /// Release every key and button this sink has pressed.
    ///
    /// Idempotent, and safe to call when nothing is held: it is called on
    /// teardown paths that cannot know what happened before them.
    fn release_all(&mut self) -> Result<(), InputError>;

    /// What is held right now, for a caller that wants to report it.
    fn held(&self) -> &Held;
}
