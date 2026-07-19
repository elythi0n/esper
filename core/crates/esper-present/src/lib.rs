//! What puts a decoded picture in front of a person.
//!
//! # Why this is a trait with more than one implementation
//!
//! The default viewer hands the media to a webview and lets it render in a
//! `<video>` element, which is the right default: it gets the platform's
//! hardware decoder and its compositor for free, and it keeps the browser
//! viewer possible. But a single-implementation trait is a webview-shaped
//! hole. The point of a second one existing from the start is that the
//! webview's constraints do not quietly become the architecture's.
//!
//! # What a presenter is handed
//!
//! **Planar YUV, not RGB.** The colour conversion belongs to the presenter
//! because where it should happen depends on the presenter: a GPU one does it
//! in a shader for free, and a software one does it on the CPU with
//! [`esper_color::rgb`]. Converting before this boundary would force the
//! GPU path to undo the work.
//!
//! **Damage, because repainting a caret should not cost a frame.** A presenter
//! is free to ignore it and repaint everything; one that honours it is why a
//! static screen costs nothing all the way to the glass.

use esper_capture::cursor::CursorImage;
use esper_capture::damage::Damage;
use esper_color::I420;

/// Why a picture could not be shown.
#[derive(Debug, thiserror::Error)]
pub enum PresentError {
    /// No presenter works here.
    ///
    /// Names what was tried, because "cannot display" on a headless machine
    /// tells an operator nothing they can act on.
    #[error("no presenter is available ({tried})")]
    Unavailable { tried: String },
    /// The picture does not fit what the presenter was built for.
    #[error("this presenter shows {want_w}x{want_h}, not {got_w}x{got_h}")]
    WrongSize {
        want_w: u32,
        want_h: u32,
        got_w: u32,
        got_h: u32,
    },
    /// The window went away.
    ///
    /// Its own variant rather than a generic failure: a person closing a
    /// window is not an error, and a caller needs to be able to tell the
    /// difference between that and a display that broke.
    #[error("the window was closed")]
    Closed,
    /// Converting the picture failed.
    #[error(transparent)]
    Convert(#[from] esper_color::ConvertError),
    /// The platform failed.
    #[error("{backend}: {detail}")]
    Backend {
        backend: &'static str,
        detail: String,
    },
}

/// Something the person watching did.
///
/// # Why input is reported here
///
/// A presenter owns the window, and a window is where a person's keys and
/// mouse arrive. Something else reading them would need its own connection to
/// somebody else's window, which is worse than this being a slightly wider
/// job than "show pictures" — and the trait was already the channel for what
/// the person did, since a close request is exactly that.
///
/// **Nothing here interprets them.** A presenter reports what its window was
/// told, in the window's own coordinates and this platform's own key codes;
/// turning that into protocol messages is the engine's job, because only the
/// engine knows which monitor is on screen and what the picture's size is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentEvent {
    /// They asked for the window to go away.
    CloseRequested,
    /// The window is a different size now.
    ///
    /// Not the same as the *picture* being a different size. What a presenter
    /// does about the mismatch — fit, fill, one-to-one — is a product decision
    /// that belongs with the rest of the viewer's chrome, not here.
    Resized { width: u32, height: u32 },
    /// The window needs repainting whether or not the picture changed.
    Exposed,
    /// A key went down or came up, in **this platform's** code.
    ///
    /// Not a HID usage: the translation needs the same table the injecting
    /// side uses, and putting it here would mean two copies of it.
    Key { code: u32, pressed: bool },
    /// A pointer moved to a position **in the window**, in pixels from its
    /// top-left corner.
    ///
    /// In the window rather than on the screen, because the window is what
    /// shows the picture and the picture is what the position has to be a
    /// fraction of.
    Pointer { x: i32, y: i32 },
    /// A button went down or came up, in this platform's numbering.
    Button { code: u32, pressed: bool },
    /// A wheel or touchpad scrolled, in notches.
    Scroll { dx: i16, dy: i16 },
    /// The window lost the keyboard.
    ///
    /// **The moment everything held has to be let go**, and the reason this is
    /// reported at all: a person who alt-tabs away mid-keystroke has released
    /// nothing, and the host would go on holding the key for ever.
    FocusLost,
}

/// A place to put pictures.
pub trait Presenter: std::fmt::Debug {
    /// Which presenter this is, for the log and the session HUD.
    fn backend(&self) -> &'static str;

    /// The size of picture this presenter is showing.
    fn size(&self) -> (u32, u32);

    /// Show a picture.
    ///
    /// `damage` is in picture coordinates and says what changed since the last
    /// call. A presenter may repaint more than that but must not repaint less.
    fn present(&mut self, picture: &I420, damage: &Damage) -> Result<(), PresentError>;

    /// Whatever the person did since the last call.
    ///
    /// Non-blocking: a presenter shares a thread with the media loop, and one
    /// that blocked waiting for input would stall the pictures.
    fn poll(&mut self) -> Result<Vec<PresentEvent>, PresentError>;

    /// Whether the window is still there.
    fn is_open(&self) -> bool;

    /// Wear the remote pointer, or hide it.
    ///
    /// # Why this is a cursor and not a composite
    ///
    /// The obvious implementation draws the pointer over the picture. The
    /// better one hands it to the window server as **the window's own
    /// cursor**: the server then draws it at the local pointer's position,
    /// which is where the person's hand is, so the pointer moves at the speed
    /// of the hand and not at the speed of the link. Nothing is composited,
    /// nothing has to be repainted behind it, and there is no position to keep
    /// up to date.
    ///
    /// That works because in a session being *driven* the local pointer and
    /// the remote one are the same pointer. A session only being *watched* is
    /// the other case — the host's pointer moves on its own there — and it
    /// needs the position drawn into the picture, which is a different job.
    ///
    /// `None` hides it, which is what a game or a drawing tool asks for.
    fn set_cursor(&mut self, image: Option<&CursorImage>) -> Result<(), PresentError>;

    /// Report the person's keys and pointer, or keep them.
    ///
    /// **Off by default, and the default is not the interesting case.** A
    /// viewer with nothing to send them to is a debug window, and a debug
    /// window needs a way to close itself — `Escape` — which a session
    /// forwarding input must not swallow, because a remote desktop that
    /// cannot send `Escape` is broken.
    ///
    /// So the caller says which it is. On, every key goes out and the window
    /// is closed by the window manager; off, `Escape` closes it and no input
    /// is reported at all.
    fn forward_input(&mut self, on: bool);

    /// Show pictures of a different size from now on.
    ///
    /// A session's picture size changes when the host switches monitor, and a
    /// presenter that only refused the new size would leave the viewer showing
    /// the last frame of the old screen for ever — which reads as a host that
    /// has hung.
    ///
    /// Resizing to the size already in use does nothing, so a re-announcement
    /// that changed something else costs no flicker.
    fn resize(&mut self, width: u32, height: u32) -> Result<(), PresentError>;
}

/// A presenter that shows nothing and remembers everything.
///
/// Not a stub for the sake of one: it is how the pipeline's presenting path is
/// tested where there is no display, and how a caller measures what a
/// presenter *would* have been asked to do without a window in the way.
#[derive(Debug, Default)]
pub struct NullPresenter {
    size: (u32, u32),
    /// Whether input would be forwarded, for a caller checking that a session
    /// turned it on.
    pub forwarding: bool,
    /// The last cursor this was asked to wear: `None` if never asked,
    /// `Some(None)` if asked to hide it.
    pub cursor: Option<Option<CursorImage>>,
    /// Every picture's damage, in order.
    pub shown: Vec<Damage>,
    /// Luma samples that would have been converted, damage taken into account.
    pub pixels: u64,
    open: bool,
}

impl NullPresenter {
    /// A presenter for pictures of this size.
    #[must_use]
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            size: (width, height),
            forwarding: false,
            cursor: None,
            shown: Vec::new(),
            pixels: 0,
            open: true,
        }
    }

    /// Pretend the person closed the window.
    pub fn close(&mut self) {
        self.open = false;
    }
}

impl Presenter for NullPresenter {
    fn backend(&self) -> &'static str {
        "null"
    }

    fn size(&self) -> (u32, u32) {
        self.size
    }

    fn present(&mut self, picture: &I420, damage: &Damage) -> Result<(), PresentError> {
        if (picture.width(), picture.height()) != self.size {
            return Err(PresentError::WrongSize {
                want_w: self.size.0,
                want_h: self.size.1,
                got_w: picture.width(),
                got_h: picture.height(),
            });
        }
        let bounds = esper_capture::geometry::Rect::sized(picture.width(), picture.height());
        self.pixels = self.pixels.saturating_add(damage.changed_pixels(&bounds));
        self.shown.push(damage.clone());
        Ok(())
    }

    fn poll(&mut self) -> Result<Vec<PresentEvent>, PresentError> {
        Ok(Vec::new())
    }

    fn is_open(&self) -> bool {
        self.open
    }

    fn resize(&mut self, width: u32, height: u32) -> Result<(), PresentError> {
        if width == 0 || height == 0 {
            return Err(PresentError::WrongSize {
                want_w: self.size.0,
                want_h: self.size.1,
                got_w: width,
                got_h: height,
            });
        }
        self.size = (width, height);
        Ok(())
    }

    fn forward_input(&mut self, on: bool) {
        self.forwarding = on;
    }

    fn set_cursor(&mut self, image: Option<&CursorImage>) -> Result<(), PresentError> {
        self.cursor = Some(image.cloned());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use esper_capture::damage::Regions;
    use esper_capture::geometry::Rect;

    fn picture(w: u32, h: u32) -> I420 {
        I420::new(w, h).expect("a valid size")
    }

    #[test]
    fn the_null_presenter_records_what_it_was_asked_to_show() {
        let mut p = NullPresenter::new(64, 32);
        assert_eq!(p.backend(), "null");
        assert_eq!(p.size(), (64, 32));
        assert!(p.is_open());

        let img = picture(64, 32);
        p.present(&img, &Damage::Everything).expect("shows");
        let rect = Rect {
            x: 0,
            y: 0,
            width: 8,
            height: 4,
        };
        let some = Regions::coalesce(&[rect], &Rect::sized(64, 32));
        p.present(&img, &some).expect("shows");

        assert_eq!(p.shown.len(), 2);
        assert_eq!(p.shown.first(), Some(&Damage::Everything));
        // A whole frame plus a small region, so damage is being counted rather
        // than every call treated as a full repaint.
        assert_eq!(p.pixels, 64 * 32 + 8 * 4);
    }

    #[test]
    fn a_picture_of_the_wrong_size_is_refused() {
        let mut p = NullPresenter::new(64, 32);
        let wrong = picture(64, 34);
        assert!(matches!(
            p.present(&wrong, &Damage::Everything),
            Err(PresentError::WrongSize {
                want_h: 32,
                got_h: 34,
                ..
            })
        ));
    }

    #[test]
    fn closing_is_visible_without_being_an_error() {
        let mut p = NullPresenter::new(8, 8);
        assert!(p.is_open());
        p.close();
        assert!(!p.is_open());
        // And it stays presentable: a caller that has not noticed yet should
        // not get a panic for it.
        assert!(p.present(&picture(8, 8), &Damage::Nothing).is_ok());
    }

    #[test]
    fn nothing_changed_is_recorded_rather_than_dropped() {
        // A presenter told that nothing changed should be able to skip the
        // repaint, which it can only do if it is told at all.
        let mut p = NullPresenter::new(16, 16);
        p.present(&picture(16, 16), &Damage::Nothing)
            .expect("shows");
        assert_eq!(p.shown.first(), Some(&Damage::Nothing));
        assert_eq!(p.pixels, 0);
    }
}
