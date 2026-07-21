//! Two peers meeting through a broker they own.
//!
//! Two machines behind ordinary routers cannot dial each other: a router drops
//! unsolicited inbound packets. Every remote desktop product answers that the
//! same way — run a server that introduces them — and that server is precisely
//! the thing this project refuses to be. So the introducer is **one static
//! binary the user runs**, there is no address for it anywhere in Esper, and if
//! nobody has configured one then the tiers that need it simply do not exist.
//!
//! # What is here
//!
//! - [`wire`] — what a peer says to a broker, signed, and what it hears back.
//! - [`table`] — who is waiting and who may meet whom. The broker's decisions,
//!   pure and testable without a socket.
//! - [`server`] — the broker: a QUIC listener that accepts any key, holds
//!   nothing across a restart, and forwards introductions.
//! - [`relay`] — tier 4: forwarding opaque UDP for two peers that could not
//!   find a path, with a ceiling because it is the one tier that spends
//!   somebody's bandwidth.
//! - [`client`] — the engine's half: wait to be found, or go looking.
//!
//! # What a broker is not trusted with
//!
//! The session. Every tier is end-to-end encrypted between device keys, and a
//! broker that swapped a candidate address for one of its own would find the
//! QUIC handshake refusing it, because the key would not match. What it
//! unavoidably learns — which device keys asked to meet which, from which
//! addresses, and when — is the whole argument for it being yours.

#![forbid(unsafe_code)]

pub mod client;
pub mod relay;
pub mod server;
pub mod table;
pub mod wire;

pub use client::{Client, ClientError, Met, Told, Via, local_candidates, punch_at, race_to, reach};
pub use relay::{Pairs, RelayLimits};
pub use server::{Broker, BrokerError, Limits};
pub use table::{Arrival, Table};
pub use wire::{Answer, Introduction, Meet, RENDEZVOUS_VERSION, Refusal};

/// Seconds since the epoch, as this machine sees it.
///
/// The broker checks it against its own clock with a window, so two machines
/// that disagree by a minute still meet.
#[must_use]
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
