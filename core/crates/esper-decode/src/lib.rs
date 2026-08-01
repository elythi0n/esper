//! Software decoding, in a jail. The one place a peer's bytes reach a large C
//! parser — H.264 for the picture, Opus for the sound, one process each.

pub mod audio;
pub mod child;
pub mod decoder;
pub mod openh264;
pub mod protocol;
pub mod sandbox;
pub mod worker;

pub use decoder::{DecodeError, DecodeStats, DecoderConfig, MAX_PACKET, MAX_PIXELS, VideoDecoder};
pub use openh264::OpenH264Decoder;
pub use sandbox::{Sandbox, SandboxError, SandboxReport};
pub use worker::{SandboxedDecoder, WORKER_ARG, WORKER_ENV, WorkerError};

pub use audio::{SandboxedAudio, WORKER_ARG as AUDIO_WORKER_ARG};
