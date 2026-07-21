//! Raw mode on a platform whose console this crate cannot drive yet.
//!
//! The API is the same shape as the Unix one so that a controller needs no
//! `cfg` of its own — and [`RawMode::enable`] **fails** rather than returning
//! an inert guard. That is the important part: a terminal client that thought
//! it had raw mode when it did not would echo every keystroke twice, buffer
//! lines until Enter, and send Ctrl-C to itself instead of to the remote
//! shell. Refusing with a reason is a worse experience than working and a far
//! better one than appearing to work.
//!
//! Windows has `SetConsoleMode` with `ENABLE_VIRTUAL_TERMINAL_INPUT`, which is
//! the implementation this arm is shaped to be replaced by.

use std::io;

/// A guard that would keep the terminal in raw mode.
///
/// Never constructed on this platform: [`RawMode::enable`] is the only thing
/// that makes one and it always fails.
#[derive(Debug)]
pub struct RawMode {
    _private: (),
}

impl RawMode {
    /// Refuse, naming the platform rather than pretending.
    pub fn enable() -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "this platform's console cannot be put into raw mode by Esper yet",
        ))
    }

    /// Nothing to restore, so nothing to install.
    pub fn install_panic_hook() {}

    /// The size a terminal is assumed to be when it cannot be asked.
    ///
    /// The same fallback the Unix path uses when standard input is not a
    /// terminal at all: 80x24 is what every terminal emulator opens at, and a
    /// shell started at that size and never resized is usable.
    #[must_use]
    pub const fn window_size() -> (u16, u16) {
        (80, 24)
    }

    /// Nothing to restore.
    pub const fn restore(&mut self) {}
}
