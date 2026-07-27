//! One suite, run against every [`Clipboard`] there is.
//!
//! # Why this is not each backend's own tests
//!
//! There will be three of these — X11, Windows, macOS — and they will be
//! written months apart by whoever has that machine. Left to themselves they
//! would each test what their author happened to worry about, and the session
//! would behave differently on each platform in ways nobody noticed until a
//! person pasted something and got a different thing.
//!
//! So the promises live here, in prose and in code, and a backend is finished
//! when it passes them. A backend that cannot run — no display, no session —
//! reports that rather than passing quietly, which is the other half of the
//! same idea.
//!
//! # Running it
//!
//! ```no_run
//! # use esper_clipboard::{NullClipboard, conformance};
//! let mut clipboard = NullClipboard::new();
//! let report = conformance::check(&mut clipboard);
//! assert!(report.passed(), "{report}");
//! ```

use esper_proto::ClipboardKind;

use crate::Clipboard;

/// What every backend has to get right, and whether it did.
#[derive(Debug, Default)]
pub struct Report {
    /// Each promise and what became of it, in the order they were checked.
    pub checks: Vec<Check>,
}

/// One promise.
#[derive(Debug)]
pub struct Check {
    /// What was being checked, as a sentence.
    pub what: &'static str,
    /// What went wrong, if anything.
    pub failure: Option<String>,
}

impl Report {
    /// Whether every promise held.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.checks.iter().all(|c| c.failure.is_none())
    }

    /// How many were checked.
    #[must_use]
    pub fn len(&self) -> usize {
        self.checks.len()
    }

    /// Whether nothing ran at all, which is itself a failure worth seeing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.checks.is_empty()
    }

    fn record(&mut self, what: &'static str, failure: Option<String>) {
        self.checks.push(Check { what, failure });
    }
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for check in &self.checks {
            match &check.failure {
                None => writeln!(f, "  ok    {}", check.what)?,
                Some(why) => writeln!(f, "  FAILED {} — {why}", check.what)?,
            }
        }
        let failed = self.checks.iter().filter(|c| c.failure.is_some()).count();
        write!(f, "{} checks, {failed} failed", self.checks.len())
    }
}

/// A marker no other program on the machine will have copied.
///
/// The suite runs on a real desktop, where other programs own the clipboard
/// before and after it does. Asserting "the clipboard is empty" would be
/// asserting something about the rest of the machine; asserting "what this
/// wrote is no longer there" is about the backend.
const MARKER: &str = "esper-conformance-4f2a9c";

/// The eight bytes every PNG begins with, followed by enough to be a payload.
fn a_png() -> Vec<u8> {
    let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    v.extend((0..512u32).map(|i| u8::try_from(i % 251).unwrap_or(0)));
    v
}

/// Text with everything that has ever broken a clipboard in it.
fn awkward_text() -> Vec<u8> {
    format!("{MARKER}\nline two\ttabbed\r\nnaïve — ünïcödé 日本語 🎛\u{0}after a nul\n").into_bytes()
}

/// Run every promise against this clipboard.
///
/// The clipboard is left cleared. Nothing here asserts; the report says what
/// held and what did not, so a backend's test can print all of it at once
/// rather than one failure per run.
pub fn check<C: Clipboard>(clipboard: &mut C) -> Report {
    let mut report = Report::default();

    // Whether it works at all. Everything after this is meaningless if it does
    // not, but the rest still runs so one report shows the whole picture.
    let reachable = clipboard.clear();
    report.record(
        "the clipboard can be reached",
        reachable.err().map(|e| e.to_string()),
    );

    for (kind, content) in [
        (ClipboardKind::Text, awkward_text()),
        (ClipboardKind::Image, a_png()),
    ] {
        let name = kind.name();
        match clipboard.write(kind, &content) {
            Err(e) => report.record(
                match kind {
                    ClipboardKind::Text => "text can be written",
                    ClipboardKind::Image => "an image can be written",
                },
                Some(e.to_string()),
            ),
            Ok(()) => {
                report.record(
                    match kind {
                        ClipboardKind::Text => "text can be written",
                        ClipboardKind::Image => "an image can be written",
                    },
                    None,
                );
                // **Byte for byte.** A backend that round-trips text through a
                // platform string will lose the NUL, and one that re-encodes an
                // image will hand back a different PNG — both of which look
                // like success until somebody pastes.
                let failure = match clipboard.read(kind) {
                    Ok(Some(got)) if got == content => None,
                    Ok(Some(got)) => Some(format!(
                        "wrote {} bytes of {name} and read back {} that differ",
                        content.len(),
                        got.len()
                    )),
                    Ok(None) => Some(format!("wrote {name} and read back nothing")),
                    Err(e) => Some(e.to_string()),
                };
                report.record(
                    match kind {
                        ClipboardKind::Text => "text comes back exactly as it went in",
                        ClipboardKind::Image => "an image comes back exactly as it went in",
                    },
                    failure,
                );
            }
        }
    }

    // Writing the image must not have disturbed the text: the kinds are
    // separate, and a backend that keeps one slot would silently lose one.
    let failure = match clipboard.read(ClipboardKind::Text) {
        Ok(Some(got)) if got == awkward_text() => None,
        Ok(Some(_)) => Some("writing an image changed what text reads back".to_owned()),
        Ok(None) => Some("writing an image lost the text".to_owned()),
        Err(e) => Some(e.to_string()),
    };
    report.record("the kinds do not overwrite each other", failure);

    // A generation that never changes makes a session read the clipboard on
    // every tick; one that changes when nothing did makes it read constantly.
    let before = clipboard.generation();
    let steady = clipboard.generation();
    report.record(
        "the generation is steady while nothing happens",
        (before != steady).then(|| format!("{before} then {steady} with no write between")),
    );
    let mut moved = format!("{MARKER} again").into_bytes();
    moved.push(b'!');
    let failure = match clipboard.write(ClipboardKind::Text, &moved) {
        Err(e) => Some(e.to_string()),
        Ok(()) => {
            let after = clipboard.generation();
            (after == before).then(|| format!("still {after} after a write"))
        }
    };
    report.record("the generation moves when the clipboard does", failure);

    // Larger than a datagram, larger than a page, and not a round number.
    let big: Vec<u8> = MARKER
        .bytes()
        .cycle()
        .take(ClipboardKind::Text.cap() / 4 + 7)
        .collect();
    let failure = match clipboard.write(ClipboardKind::Text, &big) {
        Err(e) => Some(e.to_string()),
        Ok(()) => match clipboard.read(ClipboardKind::Text) {
            Ok(Some(got)) if got == big => None,
            Ok(Some(got)) => Some(format!(
                "wrote {} bytes and read back {}",
                big.len(),
                got.len()
            )),
            Ok(None) => Some("wrote a large paste and read back nothing".to_owned()),
            Err(e) => Some(e.to_string()),
        },
    };
    report.record("a large paste survives whole", failure);

    // And the end of a session takes back what this process put there.
    let failure = match clipboard.clear() {
        Err(e) => Some(e.to_string()),
        Ok(()) => match clipboard.read(ClipboardKind::Text) {
            Ok(Some(got)) if got == big => {
                Some("what this process wrote is still on the clipboard".to_owned())
            }
            // Something else owning the clipboard afterwards is the normal
            // state of a desktop, and not this backend's failure.
            Ok(_) => None,
            Err(_) => None,
        },
    };
    report.record("clearing gives up what this process wrote", failure);

    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NullClipboard;

    /// The suite has to pass against the one backend that cannot be wrong, or
    /// it is testing itself rather than a clipboard.
    #[test]
    fn the_null_clipboard_conforms() {
        let mut clipboard = NullClipboard::new();
        let report = check(&mut clipboard);
        assert!(report.passed(), "\n{report}");
        assert!(report.len() >= 9, "the suite stopped early:\n{report}");
        assert!(!report.is_empty());
    }

    /// And it has to fail against a backend that is wrong, or passing means
    /// nothing. This one loses everything after a NUL, which is exactly what a
    /// backend that round-trips through a C string does.
    #[test]
    fn a_backend_that_truncates_at_a_nul_is_caught() {
        #[derive(Default)]
        struct Truncating(NullClipboard);
        impl Clipboard for Truncating {
            fn read(
                &mut self,
                kind: ClipboardKind,
            ) -> Result<Option<Vec<u8>>, crate::ClipboardError> {
                self.0.read(kind)
            }
            fn write(
                &mut self,
                kind: ClipboardKind,
                content: &[u8],
            ) -> Result<(), crate::ClipboardError> {
                let cut = content
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(content.len());
                self.0.write(kind, content.get(..cut).unwrap_or(content))
            }
            fn generation(&mut self) -> u64 {
                self.0.generation()
            }
            fn clear(&mut self) -> Result<(), crate::ClipboardError> {
                self.0.clear()
            }
        }

        let report = check(&mut Truncating::default());
        assert!(!report.passed(), "a truncating backend passed:\n{report}");
        let named = report
            .checks
            .iter()
            .filter(|c| c.failure.is_some())
            .map(|c| c.what)
            .collect::<Vec<_>>();
        assert!(
            named.contains(&"text comes back exactly as it went in"),
            "the failure was not the one that happened: {named:?}"
        );
    }

    /// A backend whose generation never moves makes a session either read the
    /// clipboard constantly or never notice a copy.
    #[test]
    fn a_backend_with_a_frozen_generation_is_caught() {
        #[derive(Default)]
        struct Frozen(NullClipboard);
        impl Clipboard for Frozen {
            fn read(
                &mut self,
                kind: ClipboardKind,
            ) -> Result<Option<Vec<u8>>, crate::ClipboardError> {
                self.0.read(kind)
            }
            fn write(
                &mut self,
                kind: ClipboardKind,
                content: &[u8],
            ) -> Result<(), crate::ClipboardError> {
                self.0.write(kind, content)
            }
            fn generation(&mut self) -> u64 {
                7
            }
            fn clear(&mut self) -> Result<(), crate::ClipboardError> {
                self.0.clear()
            }
        }

        let report = check(&mut Frozen::default());
        assert!(!report.passed(), "a frozen generation passed:\n{report}");
        assert!(report.checks.iter().any(|c| c.what
            == "the generation moves when the clipboard does"
            && c.failure.is_some()));
    }
}
