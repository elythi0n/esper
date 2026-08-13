//! Screen capture: the backend abstraction, damage tracking, and a synthetic
//! source that needs no display.

pub mod capturer;
pub mod cursor;
pub mod damage;
pub mod duplication;
pub mod frame;
pub mod gate;
pub mod geometry;
pub mod pattern;
pub mod pointer_shape;
pub mod pool;
pub mod synthetic;

pub use capturer::{Capture, CaptureError, Capturer, MonitorId, MonitorInfo, Numbering};
pub use damage::{Damage, Regions};
pub use duplication::{Moved, Report, damage_of};
pub use frame::{Frame, FrameError, PixelFormat};
pub use gate::{EncodeGate, Policy, Reason, Rung, Verdict};
pub use geometry::Rect;
pub use pattern::{Motion, Pattern, PatternError};
pub use pointer_shape::{Shape, ShapeError, ShapeInfo, image_of};
pub use pool::{BufferPool, Pixels};
pub use synthetic::SyntheticCapturer;
