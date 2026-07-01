//! OwnView wire protocol: types, CBOR framing, and error codes.
//!
//! This crate is the canonical Rust half of the contract specified in the
//! project's protocol documentation. It imports nothing else in the workspace,
//! so it can never grow a dependency on transport or platform code.
//!
//! The Go control plane implements the same framing independently. A shared
//! fixture corpus keeps the two honest: every message in `tests/fixtures` is
//! decoded and re-encoded by both languages and must round-trip byte-identical.

pub mod codes;
pub mod frame;
pub mod message;

pub use codes::Reason;
pub use frame::{FrameError, MAX_FRAME_LEN, read_frame, write_frame};
pub use message::{Hello, Role};

/// The wire protocol version. Must match exactly between peers; a mismatch is a
/// clean close naming both versions, never a negotiation.
pub const PROTOCOL_VERSION: u16 = 1;

/// ALPN identifier for the QUIC connection.
pub const ALPN: &[u8] = b"ownview/1";
