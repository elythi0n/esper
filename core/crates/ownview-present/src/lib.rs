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
//! [`ownview_color::rgb`]. Converting before this boundary would force the
//! GPU path to undo the work.
//!
//! **Damage, because repainting a caret should not cost a frame.** A presenter
//! is free to ignore it and repaint everything; one that honours it is why a
//! static screen costs nothing all the way to the glass.

use ownview_capture::damage::Damage;
use ownview_color::I420;

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
    Convert(#[from] ownview_color::ConvertError),
    /// The platform failed.
    #[error("{backend}: {detail}")]
    Backend {
        backend: &'static str,
        detail: String,
    },
}

/// Something the person watching did.
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
}

/// A presenter that shows nothing and remembers everything.
///
/// Not a stub for the sake of one: it is how the pipeline's presenting path is
/// tested where there is no display, and how a caller measures what a
/// presenter *would* have been asked to do without a window in the way.
#[derive(Debug, Default)]
pub struct NullPresenter {
    size: (u32, u32),
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
        let bounds = ownview_capture::geometry::Rect::sized(picture.width(), picture.height());
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use ownview_capture::damage::Regions;
    use ownview_capture::geometry::Rect;

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
