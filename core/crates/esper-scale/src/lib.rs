//! Downscaling captured frames before they reach the encoder.

pub mod axis;
pub mod ceiling;
pub mod scaler;

pub use ceiling::Ceiling;
pub use scaler::{ScaleError, Scaler};
