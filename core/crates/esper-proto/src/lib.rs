//! Esper wire protocol: types, CBOR framing, and error codes.
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
pub mod input;
pub mod message;
pub mod replay;

// Glob re-exports, deliberately.
//
// These were an explicit list, and the list went stale four separate times:
// a new message type would be added, the list would not be updated, and the
// failure surfaced as an "unresolved import" in a *different crate* — with the
// real cause being that `cargo fmt` had reflowed the multi-line `pub use` so a
// scripted edit's anchor no longer matched.
//
// An explicit list is more precise in principle. In practice it is a second
// copy of the module's public surface, maintained by hand, whose only job is to
// agree with the first copy. For a crate whose entire purpose is to *be* the
// wire vocabulary — where every public item is meant to be part of the API —
// the glob is not a shortcut. It is the accurate statement: everything public
// in these modules is public here.
pub use codes::*;
pub use frame::*;
pub use input::*;
pub use message::*;
pub use replay::*;

/// The wire protocol version. Must match exactly between peers; a mismatch is a
/// clean close naming both versions, never a negotiation.
pub const PROTOCOL_VERSION: u16 = 1;

/// ALPN identifier for the QUIC connection.
pub const ALPN: &[u8] = b"esper/1";
