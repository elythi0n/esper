//! Screen capture on X11.
//!
//! Three extensions do the work: **RandR** says what the monitors are,
//! **XDamage** says what changed, and **MIT-SHM** moves the pixels without
//! putting them through the socket.
//!
//! # Why this is its own crate
//!
//! Platform capture backends are where bindings live, and bindings are where
//! undefined behavior would live if there were any. Keeping each one in its
//! own crate means the memory-safety tools can be pointed at it specifically
//! rather than at a workspace where it is one module among thirty.
//!
//! In this case there is nothing for them to find: `x11rb` speaks the X11
//! protocol over a socket rather than binding libX11, so this crate contains
//! no `unsafe` and links no display library. A headless server can carry the
//! engine without a desktop being installed anywhere near it.

pub mod capturer;
pub mod cursor;
pub mod damage;
pub mod format;
pub mod monitors;
pub mod shm;

pub use capturer::X11Capturer;
