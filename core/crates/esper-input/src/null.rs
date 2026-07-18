//! A sink that injects nothing and remembers everything.
//!
//! Not a stub for the sake of one. It is how the input path is tested on a
//! machine with no display and no permission to type anywhere, and how a
//! caller checks what a session *would* have done — including the thing that
//! matters most, which is whether everything held got released.

use esper_proto::{Button, KeyEvent, PointerButton, Scroll};

use crate::{Held, InputError, InputSink, Point};

/// Something a sink was asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recorded {
    Key { usage: u16, pressed: bool },
    Pointer(Point),
    Button { button: Button, pressed: bool },
    Scroll(Scroll),
    ReleaseAll,
}

/// An input sink that records rather than injects.
#[derive(Debug, Default)]
pub struct NullSink {
    /// Everything asked for, in order.
    pub asked: Vec<Recorded>,
    held: Held,
    /// Set when this sink should refuse, for testing what a caller does then.
    fail: Option<String>,
}

impl NullSink {
    /// A sink that records.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A sink that refuses everything, for testing the failure path.
    #[must_use]
    pub fn failing(detail: &str) -> Self {
        Self {
            asked: Vec::new(),
            held: Held::new(),
            fail: Some(detail.to_owned()),
        }
    }

    /// Every key press and release, in order, as usages.
    #[must_use]
    pub fn keys(&self) -> Vec<(u16, bool)> {
        self.asked
            .iter()
            .filter_map(|r| match r {
                Recorded::Key { usage, pressed } => Some((*usage, *pressed)),
                _ => None,
            })
            .collect()
    }

    /// Every pointer position, in order.
    #[must_use]
    pub fn positions(&self) -> Vec<Point> {
        self.asked
            .iter()
            .filter_map(|r| match r {
                Recorded::Pointer(p) => Some(*p),
                _ => None,
            })
            .collect()
    }

    /// Forget what was recorded, keeping what is held.
    pub fn clear(&mut self) {
        self.asked.clear();
    }

    fn refuse(&self) -> Result<(), InputError> {
        match &self.fail {
            Some(detail) => Err(InputError::Backend {
                backend: "null",
                detail: detail.clone(),
            }),
            None => Ok(()),
        }
    }
}

impl InputSink for NullSink {
    fn backend(&self) -> &'static str {
        "null"
    }

    fn key(&mut self, event: KeyEvent) -> Result<(), InputError> {
        self.refuse()?;
        self.held.key(event.usage, event.pressed);
        self.asked.push(Recorded::Key {
            usage: event.usage,
            pressed: event.pressed,
        });
        Ok(())
    }

    fn pointer(&mut self, at: Point) -> Result<(), InputError> {
        self.refuse()?;
        self.asked.push(Recorded::Pointer(at));
        Ok(())
    }

    fn button(&mut self, event: PointerButton) -> Result<(), InputError> {
        self.refuse()?;
        self.held.button(event.button, event.pressed);
        self.asked.push(Recorded::Button {
            button: event.button,
            pressed: event.pressed,
        });
        Ok(())
    }

    fn scroll(&mut self, scroll: Scroll) -> Result<(), InputError> {
        self.refuse()?;
        self.asked.push(Recorded::Scroll(scroll));
        Ok(())
    }

    fn release_all(&mut self) -> Result<(), InputError> {
        // **Recorded before the refusal check**, deliberately. A caller has to
        // be able to see that a release was attempted even by a sink that
        // cannot do anything, because "did anybody try to let go of the keys"
        // is the question this exists to answer.
        self.asked.push(Recorded::ReleaseAll);
        let (keys, buttons) = self.held.to_release();
        for usage in keys {
            self.asked.push(Recorded::Key {
                usage,
                pressed: false,
            });
        }
        for button in buttons {
            self.asked.push(Recorded::Button {
                button,
                pressed: false,
            });
        }
        self.held.clear();
        self.refuse()
    }

    fn held(&self) -> &Held {
        &self.held
    }
}

impl Drop for NullSink {
    /// **A dropped sink releases what it held.**
    ///
    /// Nothing observes this — the recording goes with it — but the shape is
    /// the point: every sink releases on drop, and one that did not would be
    /// the one place a session ending abruptly leaves a key down. Written here
    /// too so the trait's contract is not something only the real backends
    /// keep.
    fn drop(&mut self) {
        if !self.held.is_empty() {
            tracing::debug!(
                "a recording sink was dropped holding {} keys",
                self.held.count()
            );
            self.held.clear();
        }
    }
}
