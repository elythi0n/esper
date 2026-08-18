//! What a window says the person did, on its way back to the session.
//!
//! # The other direction of the same socket
//!
//! [`crate::viewer_stream`] carries pictures to a window. This carries what
//! the person did with them back — on the same connection, because the two
//! have exactly one lifetime between them: a window that has stopped taking
//! pictures is a window whose keystrokes nobody should be sending either.
//!
//! # Why the window speaks the protocol and not a platform
//!
//! The engine's other input source is an X11 window, and it reports X11
//! keycodes and X11 button numbers, which the session then translates. A
//! webview is not X11 and pretending it is would mean inventing keycodes to
//! have them decoded again — two translations to arrive where one would.
//!
//! So a window sends what the protocol sends: **HID usages** for keys, the
//! protocol's own buttons, and a pointer in *picture* pixels. The first is
//! not a coincidence — the W3C's `KeyboardEvent.code` values are defined
//! against the same HID table, so the page's map is a name-to-number table
//! rather than a translation, and it is checked against the engine's own
//! table rather than reviewed.
//!
//! Picture pixels, not fractions: the session already maps a point in the
//! picture onto whichever monitor it lands on, including the gaps between
//! screens of different heights, and a window that sent fractions would be
//! doing that arithmetic a second time and worse.
//!
//! # The format
//!
//! ```text
//! stream  := magic version events
//! magic   := "ESPRINP\0"                       8 bytes
//! version := u8
//! event   := kind u8 | length u32 | body       as the picture stream does,
//!                                              so an unknown kind is skipped
//!   kind 1, key:      usage u16, pressed u8
//!   kind 2, pointer:  x i32, y i32             pixels in the picture
//!   kind 3, button:   button u8, pressed u8    0 left, 1 middle, 2 right,
//!                                              3 back, 4 forward
//!   kind 4, scroll:   dx i16, dy i16           notches
//!   kind 5, let go:   (no body)                the window lost the keyboard
//! ```

use std::io::{self, Write};

/// The first eight bytes. Not the picture stream's: a reader that has been
/// handed the wrong direction of the socket should say so rather than decode.
pub const MAGIC: &[u8; 8] = b"ESPRINP\0";

/// The version this module writes and reads.
pub const INPUT_VERSION: u8 = 1;

/// Event kinds.
pub mod event {
    /// A key went down or came up, as a HID usage.
    pub const KEY: u8 = 1;
    /// The pointer moved, in picture pixels.
    pub const POINTER: u8 = 2;
    /// A button went down or came up.
    pub const BUTTON: u8 = 3;
    /// A wheel turned, in notches.
    pub const SCROLL: u8 = 4;
    /// The window lost the keyboard and is letting everything go.
    pub const LET_GO: u8 = 5;
}

/// What a window can say.
///
/// Deliberately the same shape as the presenter events the session already
/// handles, so the two sources meet at the one place that tracks what is
/// held — and a key held by a window that vanished is released by the same
/// code that releases one held by an X11 window that vanished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowEvent {
    /// A key, as a HID usage on page 0x07.
    Key { usage: u16, pressed: bool },
    /// The pointer, in pixels from the picture's top-left corner.
    Pointer { x: i32, y: i32 },
    /// A button, in the protocol's own numbering.
    Button { button: u8, pressed: bool },
    /// A wheel, in notches.
    Scroll { dx: i16, dy: i16 },
    /// Everything held is let go — the window lost the keyboard.
    LetGo,
}

/// What went wrong reading a window's events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputStreamError {
    /// The first bytes are not this stream's.
    NotThisStream,
    /// A version this build does not read.
    Version(u8),
    /// A record whose body is shorter than its kind requires.
    Truncated(u8),
    /// A record claiming more bytes than any event in this format has.
    TooBig(u32),
}

impl std::fmt::Display for InputStreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotThisStream => write!(f, "this is not a window's input stream"),
            Self::Version(v) => write!(
                f,
                "an input stream at version {v}, and this build reads {INPUT_VERSION}"
            ),
            Self::Truncated(kind) => write!(f, "an event of kind {kind} shorter than it has to be"),
            Self::TooBig(len) => write!(
                f,
                "an event claiming {len} bytes, and the largest this format has is {MOST_EVENT_BYTES}"
            ),
        }
    }
}

impl std::error::Error for InputStreamError {}

/// The largest a single event may claim to be.
///
/// The biggest event this format has is nine bytes. A page is generous by
/// three orders of magnitude and still small enough that a wrong length is
/// refused in the turn it arrives rather than buffered.
pub const MOST_EVENT_BYTES: usize = 4096;

/// Writes a window's events. The page has its own writer; this one is for
/// tests and for anything in Rust that stands in for a window.
#[derive(Debug)]
pub struct Writer<W: Write> {
    out: W,
    started: bool,
}

impl<W: Write> Writer<W> {
    pub const fn new(out: W) -> Self {
        Self {
            out,
            started: false,
        }
    }

    /// One event.
    pub fn event(&mut self, what: WindowEvent) -> io::Result<()> {
        if !self.started {
            self.out.write_all(MAGIC)?;
            self.out.write_all(&[INPUT_VERSION])?;
            self.started = true;
        }
        let (kind, body): (u8, Vec<u8>) = match what {
            WindowEvent::Key { usage, pressed } => {
                let mut b = usage.to_be_bytes().to_vec();
                b.push(u8::from(pressed));
                (event::KEY, b)
            }
            WindowEvent::Pointer { x, y } => {
                let mut b = x.to_be_bytes().to_vec();
                b.extend_from_slice(&y.to_be_bytes());
                (event::POINTER, b)
            }
            WindowEvent::Button { button, pressed } => {
                (event::BUTTON, vec![button, u8::from(pressed)])
            }
            WindowEvent::Scroll { dx, dy } => {
                let mut b = dx.to_be_bytes().to_vec();
                b.extend_from_slice(&dy.to_be_bytes());
                (event::SCROLL, b)
            }
            WindowEvent::LetGo => (event::LET_GO, Vec::new()),
        };
        let len = u32::try_from(body.len()).unwrap_or(u32::MAX);
        self.out.write_all(&[kind])?;
        self.out.write_all(&len.to_be_bytes())?;
        self.out.write_all(&body)?;
        self.out.flush()
    }

    pub fn into_inner(self) -> W {
        self.out
    }
}

/// Reads a window's events out of whatever has arrived so far.
///
/// # Why it keeps a buffer rather than taking a reader
///
/// Because a socket delivers bytes, not events: a keystroke can arrive in two
/// pieces and two can arrive together. Anything short of a whole event is
/// kept and re-examined when more arrives, which is the only way a reader of
/// a live stream can be written.
#[derive(Debug, Default)]
pub struct Events {
    buffer: Vec<u8>,
    begun: bool,
    /// Events of a kind this build does not know, walked past and counted.
    skipped: u64,
}

impl Events {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many events of an unknown kind have been walked past.
    #[must_use]
    pub const fn skipped(&self) -> u64 {
        self.skipped
    }

    /// Take whatever arrived, and return every complete event in it.
    ///
    /// # Errors
    ///
    /// When the stream does not begin the way this one does, or is a version
    /// this build does not read. Both are fatal for the connection: a reader
    /// that guessed at either would be injecting keystrokes it invented.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<WindowEvent>, InputStreamError> {
        self.buffer.extend_from_slice(bytes);
        let mut out = Vec::new();

        if !self.begun {
            if self.buffer.len() < 9 {
                return Ok(out);
            }
            if self.buffer.get(..8) != Some(MAGIC.as_slice()) {
                return Err(InputStreamError::NotThisStream);
            }
            match self.buffer.get(8) {
                Some(&INPUT_VERSION) => {}
                Some(&other) => return Err(InputStreamError::Version(other)),
                None => return Ok(out),
            }
            self.buffer.drain(..9);
            self.begun = true;
        }

        while let Some(kind) = self.buffer.first().copied() {
            let Some(len_bytes) = self
                .buffer
                .get(1..5)
                .and_then(|b| <[u8; 4]>::try_from(b).ok())
            else {
                break;
            };
            let claimed = u32::from_be_bytes(len_bytes);
            // Refused rather than waited for.
            //
            // Every event in this format is nine bytes or fewer, so a record
            // claiming more than a page of them is a length that has gone
            // wrong — and a reader that waits for it is a reader that holds
            // everything sent after it, for ever, on the word of the thing at
            // the far end. The picture stream's reader refuses the same way.
            if claimed as usize > MOST_EVENT_BYTES {
                return Err(InputStreamError::TooBig(claimed));
            }
            let len = claimed as usize;
            let Some(body) = self.buffer.get(5..5 + len) else {
                break;
            };
            let event = decode(kind, body);
            match event {
                Ok(Some(e)) => out.push(e),
                Ok(None) => self.skipped = self.skipped.saturating_add(1),
                Err(e) => return Err(e),
            }
            self.buffer.drain(..5 + len);
        }
        Ok(out)
    }
}

fn decode(kind: u8, body: &[u8]) -> Result<Option<WindowEvent>, InputStreamError> {
    let short = || InputStreamError::Truncated(kind);
    Ok(match kind {
        event::KEY => {
            let usage = u16::from_be_bytes([
                *body.first().ok_or_else(short)?,
                *body.get(1).ok_or_else(short)?,
            ]);
            Some(WindowEvent::Key {
                usage,
                pressed: *body.get(2).ok_or_else(short)? != 0,
            })
        }
        event::POINTER => {
            let x = i32::from_be_bytes(
                body.get(..4)
                    .ok_or_else(short)?
                    .try_into()
                    .map_err(|_| short())?,
            );
            let y = i32::from_be_bytes(
                body.get(4..8)
                    .ok_or_else(short)?
                    .try_into()
                    .map_err(|_| short())?,
            );
            Some(WindowEvent::Pointer { x, y })
        }
        event::BUTTON => Some(WindowEvent::Button {
            button: *body.first().ok_or_else(short)?,
            pressed: *body.get(1).ok_or_else(short)? != 0,
        }),
        event::SCROLL => {
            let dx = i16::from_be_bytes([
                *body.first().ok_or_else(short)?,
                *body.get(1).ok_or_else(short)?,
            ]);
            let dy = i16::from_be_bytes([
                *body.get(2).ok_or_else(short)?,
                *body.get(3).ok_or_else(short)?,
            ]);
            Some(WindowEvent::Scroll { dx, dy })
        }
        event::LET_GO => Some(WindowEvent::LetGo),
        // A kind from a later window. Walked past by its length, which is the
        // entire reason every event carries one.
        _ => None,
    })
}

#[cfg(test)]
#[expect(
    clippy::indexing_slicing,
    reason = "a test that indexes past its own fixture is a failing test, which is the point"
)]
mod tests {
    use super::*;

    fn written(events: &[WindowEvent]) -> Vec<u8> {
        let mut w = Writer::new(Vec::new());
        for e in events {
            w.event(*e).expect("a buffer does not fail");
        }
        w.into_inner()
    }

    /// A length nothing will ever satisfy is refused, not waited for.
    ///
    /// Without this, one wrong length turns every byte sent afterwards into
    /// memory this process holds until the session ends — on the word of the
    /// thing at the far end, which is the one place a bound is worth having.
    #[test]
    fn an_event_claiming_more_than_any_event_is_refused_rather_than_buffered() {
        let mut events = Events::new();
        let mut stream = Vec::new();
        stream.extend_from_slice(MAGIC);
        stream.push(INPUT_VERSION);
        stream.push(event::KEY);
        stream.extend_from_slice(&u32::MAX.to_be_bytes());
        assert!(matches!(
            events.feed(&stream),
            Err(InputStreamError::TooBig(u32::MAX))
        ));
    }

    #[test]
    fn what_a_window_says_is_what_the_engine_hears() {
        let said = [
            WindowEvent::Key {
                usage: 0x04,
                pressed: true,
            },
            WindowEvent::Pointer { x: 640, y: -3 },
            WindowEvent::Button {
                button: 2,
                pressed: false,
            },
            WindowEvent::Scroll { dx: 0, dy: -3 },
            WindowEvent::LetGo,
        ];
        let mut heard = Events::new();
        assert_eq!(
            heard
                .feed(&written(&said))
                .expect("a stream this build reads"),
            said
        );
    }

    /// A socket delivers bytes, not events.
    #[test]
    fn an_event_split_across_two_arrivals_still_arrives() {
        let bytes = written(&[
            WindowEvent::Key {
                usage: 0x1E,
                pressed: true,
            },
            WindowEvent::Key {
                usage: 0x1E,
                pressed: false,
            },
        ]);
        let mut heard = Events::new();
        let mut out = Vec::new();
        // One byte at a time, which is the worst a socket can do and the
        // shape every off-by-one in a reader like this shows up as.
        for byte in &bytes {
            out.extend(heard.feed(&[*byte]).expect("a stream this build reads"));
        }
        assert_eq!(
            out,
            vec![
                WindowEvent::Key {
                    usage: 0x1E,
                    pressed: true
                },
                WindowEvent::Key {
                    usage: 0x1E,
                    pressed: false
                },
            ]
        );
    }

    #[test]
    fn a_stream_that_is_not_this_one_is_refused_rather_than_decoded() {
        let mut heard = Events::new();
        // The picture stream's magic, which is the mistake worth catching:
        // the two directions share a socket.
        let wrong = b"ESPRVID\x00\x01\x01\x00\x00\x00\x03";
        assert_eq!(heard.feed(wrong), Err(InputStreamError::NotThisStream));
    }

    #[test]
    fn a_version_this_build_does_not_read_is_refused() {
        let mut bytes = MAGIC.to_vec();
        bytes.push(INPUT_VERSION + 7);
        let mut heard = Events::new();
        assert_eq!(
            heard.feed(&bytes),
            Err(InputStreamError::Version(INPUT_VERSION + 7))
        );
    }

    /// The reason every event carries a length.
    #[test]
    fn an_event_from_a_later_window_is_walked_past() {
        let mut w = Writer::new(Vec::new());
        w.event(WindowEvent::Key {
            usage: 0x04,
            pressed: true,
        })
        .expect("write");
        w.into_inner();

        let mut bytes = written(&[WindowEvent::Key {
            usage: 0x04,
            pressed: true,
        }]);
        // A kind from a window newer than this engine, in the middle.
        bytes.extend_from_slice(&[200, 0, 0, 0, 4, b'l', b'a', b't', b'e']);
        bytes.extend_from_slice(&written(&[WindowEvent::LetGo])[9..]);

        let mut heard = Events::new();
        let out = heard.feed(&bytes).expect("a stream this build reads");
        assert_eq!(
            out,
            vec![
                WindowEvent::Key {
                    usage: 0x04,
                    pressed: true
                },
                WindowEvent::LetGo
            ],
            "the unknown event is walked past and the one after it still arrives"
        );
        assert_eq!(
            heard.skipped(),
            1,
            "and it is counted rather than forgotten"
        );
    }

    #[test]
    fn a_truncated_event_is_an_error_and_not_a_guess() {
        let mut bytes = MAGIC.to_vec();
        bytes.push(INPUT_VERSION);
        // A key event that says three bytes and carries two.
        bytes.extend_from_slice(&[event::KEY, 0, 0, 0, 2, 0x00, 0x04]);
        let mut heard = Events::new();
        assert_eq!(
            heard.feed(&bytes),
            Err(InputStreamError::Truncated(event::KEY))
        );
    }
}
