//! Putting back whatever a host changed on the display it was serving.
//!
//! # Why this is one function and not each backend's own
//!
//! Two backends change server-side state that outlives the process: the
//! presence backend turns screens off, and the input backend detaches this
//! machine's keyboard and pointer. Both register an [`Undo`] with the same
//! keeper, and the keeper has one applier — so something has to know which
//! backend owns which record.
//!
//! It is here rather than in the keeper because the keeper is
//! platform-neutral by design: it knows how to outlive a process and nothing
//! about displays. And it is here rather than in either backend because a
//! backend that could put the *other* one's state back would be a backend
//! reaching into state it does not own.

use esper_keeper::Undo;

/// Put one thing back, whichever backend owns it.
///
/// # Errors
///
/// If the backend that owns it refuses, or no backend on this platform does.
pub fn put_back(undo: &Undo) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        match undo {
            Undo::Crtc { .. } => esper_presence_x11::put_back(undo),
            Undo::Slave { .. } => esper_input_x11::floating::put_back(undo),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = undo;
        Err("no backend on this platform knows how to put that back".to_owned())
    }
}
