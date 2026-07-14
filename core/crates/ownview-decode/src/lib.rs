//! Software H.264 decoding. The one place a peer's bytes reach a large C
//! parser.

pub mod child;
pub mod decoder;
pub mod openh264;
pub mod protocol;
pub mod sandbox;

pub use decoder::{DecodeError, DecodeStats, DecoderConfig, MAX_PACKET, MAX_PIXELS, VideoDecoder};
pub use openh264::OpenH264Decoder;
pub use sandbox::{Sandbox, SandboxError, SandboxReport};
