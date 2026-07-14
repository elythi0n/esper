//! Software H.264 encoding, and the CPU budget that keeps it from taking the
//! host.

pub mod encoder;
pub mod openh264;

pub use encoder::{
    EncodeError, EncodeStats, Encoded, EncoderConfig, Tuning, VideoEncoder, has_assembly,
};
pub use openh264::OpenH264;
