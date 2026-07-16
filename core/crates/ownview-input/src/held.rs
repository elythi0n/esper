//! What is currently held down, so it can be let go of.
//!
//! # Why this is not each backend's business
//!
//! Every backend needs it, and it is the same in all of them: a set of what
//! has been pressed and not released, and a way to release the lot. Written
//! once here, a backend that forgets to track a press cannot exist; written
//! per backend, one of them eventually forgets.
//!
//! # Why it matters more than it looks
//!
//! A key left held on a machine nobody is driving any more is the worst thing
//! this path can do. A stuck Ctrl makes every keystroke a shortcut; a stuck
//! Alt swallows the window manager; a stuck mouse button selects everything
//! the pointer passes over. The person at the host has no way to know why, and
//! the session that caused it is gone.
//!
//! So the release happens on **every** ending, not just the tidy one: the
//! protocol's `ReleaseAll`, the session closing, and `Drop` — because a
//! process being torn down is what a dropped session looks like from in here.

use ownview_proto::Button;

/// The keys and buttons a sink has pressed and not released.
///
/// Insertion order is kept, and releases go out in reverse: modifiers are
/// pressed before the key they modify, so releasing in reverse lets the key go
/// before the modifier. The other order briefly leaves a modifier held with
/// nothing under it, which some applications act on.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Held {
    keys: Vec<u16>,
    buttons: Vec<Button>,
}

impl Held {
    /// Nothing held.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            keys: Vec::new(),
            buttons: Vec::new(),
        }
    }

    /// Record a key press or release.
    ///
    /// Returns whether this changed anything: a second press of a key already
    /// held is a key *repeat*, which is a real thing a keyboard does and must
    /// still be injected, but it must not be recorded twice or one release
    /// would leave it held.
    pub fn key(&mut self, usage: u16, pressed: bool) -> bool {
        let at = self.keys.iter().position(|k| *k == usage);
        match (pressed, at) {
            (true, None) => {
                self.keys.push(usage);
                true
            }
            (true, Some(_)) => false,
            (false, Some(i)) => {
                self.keys.remove(i);
                true
            }
            (false, None) => false,
        }
    }

    /// Record a button press or release.
    pub fn button(&mut self, button: Button, pressed: bool) -> bool {
        let at = self.buttons.iter().position(|b| *b == button);
        match (pressed, at) {
            (true, None) => {
                self.buttons.push(button);
                true
            }
            (true, Some(_)) => false,
            (false, Some(i)) => {
                self.buttons.remove(i);
                true
            }
            (false, None) => false,
        }
    }

    /// Whether a key is held.
    #[must_use]
    pub fn holds_key(&self, usage: u16) -> bool {
        self.keys.contains(&usage)
    }

    /// Whether a button is held.
    #[must_use]
    pub fn holds_button(&self, button: Button) -> bool {
        self.buttons.contains(&button)
    }

    /// Whether anything is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty() && self.buttons.is_empty()
    }

    /// How many keys and buttons are held.
    #[must_use]
    pub fn count(&self) -> usize {
        self.keys.len().saturating_add(self.buttons.len())
    }

    /// Everything held, in the order it should be released: most recent first.
    #[must_use]
    pub fn to_release(&self) -> (Vec<u16>, Vec<Button>) {
        let mut keys = self.keys.clone();
        keys.reverse();
        let mut buttons = self.buttons.clone();
        buttons.reverse();
        (keys, buttons)
    }

    /// Forget everything, once a backend has actually released it.
    pub fn clear(&mut self) {
        self.keys.clear();
        self.buttons.clear();
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn a_press_and_release_leaves_nothing_held() {
        let mut held = Held::new();
        assert!(held.key(0x04, true));
        assert!(held.holds_key(0x04));
        assert_eq!(held.count(), 1);
        assert!(held.key(0x04, false));
        assert!(held.is_empty());
    }

    /// **A key repeat is a real event and must still be injected**, but it
    /// must not be recorded twice: if it were, the single release that follows
    /// would leave the key held.
    #[test]
    fn a_repeat_is_not_a_second_press() {
        let mut held = Held::new();
        assert!(held.key(0x04, true), "the first press is new");
        assert!(!held.key(0x04, true), "the repeat is not");
        assert_eq!(held.count(), 1);
        held.key(0x04, false);
        assert!(held.is_empty(), "one release must be enough");
    }

    /// A release for something never pressed is not an error — a controller
    /// whose window lost focus mid-keystroke sends exactly that — but it must
    /// not go negative or record anything.
    #[test]
    fn a_release_of_something_never_pressed_does_nothing() {
        let mut held = Held::new();
        assert!(!held.key(0x04, false));
        assert!(!held.button(Button::Left, false));
        assert!(held.is_empty());
    }

    /// **Releases go out in reverse.** Ctrl is pressed before C, so C must be
    /// released before Ctrl — the other order leaves a modifier held over
    /// nothing, which some applications act on.
    #[test]
    fn releases_come_out_in_the_reverse_of_the_order_pressed() {
        let mut held = Held::new();
        // Ctrl, Shift, then C.
        held.key(0xE0, true);
        held.key(0xE1, true);
        held.key(0x06, true);
        held.button(Button::Left, true);
        held.button(Button::Right, true);

        let (keys, buttons) = held.to_release();
        assert_eq!(keys, vec![0x06, 0xE1, 0xE0]);
        assert_eq!(buttons, vec![Button::Right, Button::Left]);
    }

    #[test]
    fn clearing_forgets_everything() {
        let mut held = Held::new();
        held.key(0x04, true);
        held.button(Button::Middle, true);
        assert_eq!(held.count(), 2);
        held.clear();
        assert!(held.is_empty());
        let (keys, buttons) = held.to_release();
        assert!(keys.is_empty() && buttons.is_empty());
    }

    #[test]
    fn keys_and_buttons_are_tracked_apart() {
        let mut held = Held::new();
        held.key(0x04, true);
        held.button(Button::Left, true);
        assert!(held.holds_key(0x04) && held.holds_button(Button::Left));
        assert!(!held.holds_key(0x05) && !held.holds_button(Button::Right));
        held.key(0x04, false);
        assert!(held.holds_button(Button::Left), "the button is still down");
    }
}
