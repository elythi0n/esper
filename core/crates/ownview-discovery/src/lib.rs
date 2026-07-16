//! LAN discovery: finding other OwnViews on the same segment.
//!
//! # Why this speaks the protocol itself
//!
//! What OwnView needs from mDNS is narrow — advertise one service type, browse
//! for that same type, read back a fingerprint and a port — and the mature
//! crate for the job costs eight dependencies, about thirty thousand lines and
//! some five hundred `unsafe` occurrences to provide a general DNS-SD stack.
//! That is the same trade the capture stack already refused when it chose to
//! speak X11 on a socket rather than bind `libX11`.
//!
//! So this is the subset, in safe Rust, linking nothing. The reasoning and the
//! measurements are recorded as a decision record; what is deliberately left
//! out is listed there too.
//!
//! # What is on the wire
//!
//! `_ownview._udp.local`, one instance per device:
//!
//! ```text
//! PTR  _ownview._udp.local           -> elythion (c6a8baa2)._ownview._udp.local
//! SRV  elythion (c6a8baa2)._ownview…     -> 0 0 7645 elythion.local
//! TXT  elythion (c6a8baa2)._ownview…     -> fp=c6a8baa216f1078e pv=1
//! A    elythion.local                    -> 192.168.1.20
//! ```
//!
//! The instance name carries the first eight hex digits of the fingerprint,
//! which is what makes name-conflict probing unnecessary: two instances
//! collide only if two device keys do.
//!
//! # What a browser trusts
//!
//! **Nothing in the packet decides who may connect.** An advertisement is a
//! hint about where to look; the fingerprint in it is matched against the
//! paired devices to decide whether to *offer* a connection, and the
//! connection itself proves the key in the TLS handshake. A peer that lies in
//! its TXT record gets a session refused a moment later, having gained
//! nothing.
//!
//! The address is taken from **the packet's source**, not from the `A` record
//! it carries, so a missing or wrong address record is harmless.

pub mod browser;
pub mod name;
pub mod responder;
pub mod service;
pub mod socket;
pub mod wire;

pub use browser::{Browser, Found, Sightings};
pub use responder::Responder;
pub use service::{Advertisement, SERVICE, instance_name};

/// Anything that went wrong finding or announcing.
#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    /// A packet could not be read.
    ///
    /// Every one of these is a peer's bytes rather than a fault of ours, so
    /// they are counted and dropped rather than propagated — see
    /// [`browser::Browser`].
    #[error("malformed packet: {0}")]
    Malformed(&'static str),

    /// A name is not one this protocol can carry.
    #[error("{0}")]
    Name(#[from] name::NameError),

    /// The socket could not be set up or used.
    #[error("mDNS socket: {0}")]
    Socket(String),
}
