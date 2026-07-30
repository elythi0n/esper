//! Colour conversion from captured RGB to the planar YUV an encoder wants.

pub mod convert;
pub mod i420;
pub mod mosaic;
pub mod rgb;
pub mod space;

pub use convert::{Coefficients, ConvertError};
pub use i420::{I420, I420Error};
pub use rgb::RgbCoefficients;
pub use space::{ColorSpace, Matrix, Range, Vui};
