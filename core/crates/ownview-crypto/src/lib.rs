//! Device identity, fingerprints, pairing and the session handshake.
//!
//! This is the only crate permitted to import a cryptography library; the ban
//! is enforced by `deny.toml`. Everything else in the workspace receives keys
//! and verified results through these types.

pub mod code;
pub mod der;
pub mod device;
pub mod fingerprint;
pub mod identity;
mod wordlist;

pub use code::{CODE_LEN, CodeError, PairingCode};
pub use device::{Device, DeviceStore, Permission, StoreError};
pub use fingerprint::Fingerprint;
pub use identity::{Identity, IdentityError, KEY_LEN};
