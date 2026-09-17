//! Asking a router to forward a port, and taking it back.
//!
//! # What this is for, and what it is not
//!
//! Two machines behind different NATs need an introducer — a broker — unless
//! one of them is directly reachable. A host that can persuade its own router
//! to forward a port becomes directly reachable, and then the broker is not
//! needed at all: the connection is peer to peer, and nobody learns which
//! device asked to meet which.
//!
//! **It is off unless somebody turns it on, and that is not timidity.** A port
//! mapping is a hole in a firewall, opened by a program, on a router the
//! person may share with a household or an office. The gain is skipping an
//! introducer; the cost is that this machine is now reachable from the whole
//! internet rather than from whoever a broker introduced. That is a decision
//! about a network, and it belongs to whoever runs the network — so
//! `network.port_mapping` defaults to off and the threat model says why.
//!
//! # Two protocols, and the one that is missing
//!
//! [`wire`] speaks PCP (RFC 6887) and NAT-PMP (RFC 6886). They share a port,
//! and a PCP server is required to answer a NAT-PMP request — so one socket
//! tries the newer one and falls back without a second round of discovery.
//!
//! **UPnP-IGD is deliberately not here**, and it is the one most consumer
//! routers speak, so the reason has to be good. It is not one protocol: it is
//! SSDP discovery over multicast, then an HTTP fetch of a device description,
//! then XML parsing, then SOAP. That is an XML parser and an HTTP client
//! pointed at a device on the local network that answered a multicast — which
//! is a meaningful amount of attack surface for a convenience, in a product
//! whose position on parsers is that each one gets a fuzz target. It is worth
//! adding, with that surface accounted for, and it is worth adding *after*
//! this rather than instead of it.
//!
//! # The mapping is given back
//!
//! A mapping has a lifetime and expires on its own, so a host that vanishes
//! leaves a hole that closes itself — eventually. "Eventually" is the wrong
//! amount of time for a hole in somebody's firewall, so the lifetime is short
//! and renewed, and the mapping is released on the way out.
//!
//! A host that is *killed* releases nothing, which is the same problem the
//! screen resolution has and it has the same answer: what was asked for is
//! written to a file before it is asked for, and the next start gives back
//! anything the last one did not. See [`Outstanding`].

pub mod wire;

use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub use wire::{Granted, Nonce, Rejected};

/// How a mapping was obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum How {
    /// RFC 6887. Carries the external address and a nonce.
    Pcp,
    /// RFC 6886. Older, more widely implemented, says less.
    NatPmp,
}

impl How {
    /// A phrase for a line of output.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Pcp => "PCP",
            Self::NatPmp => "NAT-PMP",
        }
    }
}

/// Why a port could not be mapped.
#[derive(Debug, thiserror::Error)]
pub enum MapError {
    /// There is no default route, so there is no router to ask.
    #[error("this machine has no default route, so there is nothing to ask")]
    NoGateway,
    /// The socket could not be opened or used.
    #[error("talking to the router at {gateway}: {why}")]
    Socket { gateway: Ipv4Addr, why: String },
    /// Nothing answered, in either protocol.
    #[error("the router at {gateway} answered neither PCP nor NAT-PMP")]
    NoAnswer { gateway: Ipv4Addr },
    /// It answered and said no.
    #[error("{how}: {why}")]
    Rejected { how: &'static str, why: Rejected },
    /// The record of what to give back could not be kept.
    #[error("could not record the mapping to give back: {0}")]
    Record(String),
}

/// A mapping this machine holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Mapping {
    pub how: How,
    /// The router that granted it.
    pub gateway: Ipv4Addr,
    /// The port on this machine.
    pub internal_port: u16,
    /// Where the world can reach it.
    pub external: SocketAddrV4,
    /// How long the router said it would keep it.
    pub lifetime_secs: u32,
    /// PCP's mapping nonce. Meaningless for NAT-PMP and kept as zero there, so
    /// one record shape covers both.
    pub nonce: Nonce,
}

impl Mapping {
    /// When to ask again.
    ///
    /// **Half the lifetime**, which is what both RFCs advise and for the same
    /// reason: a refresh that is late by a network hiccup should still land
    /// before the mapping is gone, and a mapping that lapses is a session that
    /// stops being reachable without anything having failed.
    #[must_use]
    pub const fn renew_after(&self) -> Duration {
        Duration::from_secs((self.lifetime_secs / 2) as u64)
    }
}

/// A mapping a previous run asked for and did not give back.
///
/// The same shape as the mapping itself: what is written before the request is
/// what would have to be released, and writing it *before* means a host that
/// dies between the write and the router's answer releases something that was
/// never granted — which is a no-op, and the harmless direction to be wrong in.
pub type Outstanding = Mapping;

/// The file a pending release lives in, inside the state directory.
const RECORD: &str = "port-mapping.json";

/// How long a mapping is asked for.
///
/// **Two minutes, renewed after one.** Short on purpose: it is the longest a
/// hole in somebody's firewall stays open after this machine stops asking,
/// which is the number that matters when the machine is unplugged rather than
/// shut down. The cost of short is a packet a minute to the router.
pub const LIFETIME: u32 = 120;

/// How long to wait for a router to answer one attempt.
const PATIENCE: Duration = Duration::from_millis(250);

/// How many times to ask before deciding nothing is listening.
///
/// Both RFCs specify exponential backoff from 250ms. Three attempts is under a
/// second and a half in total, which is the right budget for something
/// opportunistic: a host whose router says nothing should not be a host that
/// takes two seconds longer to start.
const ATTEMPTS: u32 = 3;

/// This machine's default gateway, if it has one.
///
/// Read from `/proc/net/route`, which is the kernel's own answer and needs no
/// privilege. The destination `00000000` with the `UG` flags is the default
/// route; its gateway field is little-endian hex, because the file reports the
/// address in host byte order.
#[must_use]
pub fn default_gateway() -> Option<Ipv4Addr> {
    let text = std::fs::read_to_string("/proc/net/route").ok()?;
    for line in text.lines().skip(1) {
        let mut fields = line.split_whitespace();
        let _iface = fields.next()?;
        let destination = fields.next()?;
        let gateway = fields.next()?;
        let flags = fields.next().unwrap_or("0");
        if destination != "00000000" {
            continue;
        }
        // RTF_UP | RTF_GATEWAY.
        let flags = u32::from_str_radix(flags, 16).unwrap_or(0);
        if flags & 0x0003 != 0x0003 {
            continue;
        }
        let raw = u32::from_str_radix(gateway, 16).ok()?;
        // The file writes the address in the machine's own byte order, so the
        // octets come out reversed on a little-endian host — which every host
        // this runs on is, and reversing unconditionally would be wrong on the
        // one that is not.
        return Some(Ipv4Addr::from(raw.to_le_bytes()));
    }
    None
}

/// The address this machine has on the way to `gateway`.
///
/// **Found by asking the routing table, not by listing interfaces.** A machine
/// with a VPN, a container bridge and a wireless card has several addresses and
/// only one of them is the one the router will see. Connecting a UDP socket
/// picks the route without sending anything.
fn address_towards(gateway: Ipv4Addr) -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((gateway, wire::PORT)).ok()?;
    match socket.local_addr().ok()? {
        std::net::SocketAddr::V4(v4) => Some(*v4.ip()),
        std::net::SocketAddr::V6(_) => None,
    }
}

/// Twelve bytes nothing else will guess.
///
/// From the operating system, not from a clock: two hosts started by the same
/// script at the same moment would otherwise pick the same nonce, and the
/// nonce is the only thing that makes a mapping one client's.
fn nonce() -> Nonce {
    let mut bytes = [0u8; 12];
    if let Ok(mut file) = std::fs::File::open("/dev/urandom") {
        use std::io::Read as _;
        let _ = file.read_exact(&mut bytes);
    }
    bytes
}

/// Ask the router to forward `internal_port`, and remember what was asked.
///
/// Tries PCP first and falls back to NAT-PMP. The record is written **before**
/// the request, so a host that dies mid-conversation still gives the mapping
/// back on its next start.
///
/// # Errors
///
/// [`MapError`], which says which stage it got to.
pub fn map(state_dir: &Path, internal_port: u16) -> Result<Mapping, MapError> {
    let gateway = default_gateway().ok_or(MapError::NoGateway)?;
    let client = address_towards(gateway).ok_or(MapError::NoGateway)?;
    let socket = talk_to(gateway)?;
    let nonce = nonce();

    // Written before either protocol is spoken, naming the port this machine
    // will have asked about. Both protocols release by asking again with a
    // lifetime of zero, so the record does not need to know which one worked.
    let pending = Mapping {
        how: How::Pcp,
        gateway,
        internal_port,
        external: SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, internal_port),
        lifetime_secs: LIFETIME,
        nonce,
    };
    remember(state_dir, &pending)?;

    let mut refusal = None;
    match ask_pcp(&socket, client, &nonce, internal_port, LIFETIME) {
        Ok(granted) => {
            let mapping = Mapping {
                how: How::Pcp,
                external: granted.external,
                lifetime_secs: granted.lifetime_secs,
                ..pending
            };
            remember_over(state_dir, &mapping)?;
            return Ok(mapping);
        }
        Err(Speaking::Silence) => {}
        Err(Speaking::Said(why)) => refusal = Some(("PCP", why)),
    }

    match ask_natpmp(&socket, internal_port, LIFETIME) {
        Ok((external_port, lifetime_secs)) => {
            // NAT-PMP does not say where the mapping is; a second question
            // does. A router that will not answer it has still made the
            // mapping, so the port is kept and the address is left unknown
            // rather than the whole thing being thrown away.
            let external_ip = ask_natpmp_address(&socket).unwrap_or(Ipv4Addr::UNSPECIFIED);
            let mapping = Mapping {
                how: How::NatPmp,
                external: SocketAddrV4::new(external_ip, external_port),
                lifetime_secs,
                ..pending
            };
            remember_over(state_dir, &mapping)?;
            Ok(mapping)
        }
        Err(Speaking::Silence) => {
            // Nothing answered either protocol, so there is nothing to give
            // back and the record would be a lie.
            let _ = forget(state_dir);
            Err(match refusal {
                Some((how, why)) => MapError::Rejected { how, why },
                None => MapError::NoAnswer { gateway },
            })
        }
        Err(Speaking::Said(why)) => {
            let _ = forget(state_dir);
            Err(MapError::Rejected {
                how: "NAT-PMP",
                why,
            })
        }
    }
}

/// Give a mapping back, and forget it.
///
/// Both protocols release the same way: ask again for the same port with a
/// lifetime of zero. Best effort — a router that has already forgotten the
/// mapping, or that is no longer there, has answered the question.
///
/// # Errors
///
/// [`MapError::Record`] if the record cannot be removed, which is the only
/// failure worth reporting: an unreleased mapping expires on its own, but a
/// record that will not go makes every future start try again.
pub fn unmap(state_dir: &Path, mapping: &Mapping) -> Result<(), MapError> {
    if let Ok(socket) = talk_to(mapping.gateway) {
        let client = address_towards(mapping.gateway).unwrap_or(Ipv4Addr::UNSPECIFIED);
        match mapping.how {
            How::Pcp => {
                let _ = ask_pcp(&socket, client, &mapping.nonce, mapping.internal_port, 0);
            }
            How::NatPmp => {
                let _ = ask_natpmp(&socket, mapping.internal_port, 0);
            }
        }
    }
    forget(state_dir)
}

/// Give back anything a previous run did not.
///
/// Called at startup. Quiet when there is nothing to do, which is almost
/// always.
///
/// # Errors
///
/// [`MapError::Record`] if the record is there and cannot be read or removed.
pub fn release_anything_left_behind(state_dir: &Path) -> Result<Option<Mapping>, MapError> {
    let Some(outstanding) = recorded(state_dir)? else {
        return Ok(None);
    };
    unmap(state_dir, &outstanding)?;
    Ok(Some(outstanding))
}

/// A socket bound for talking to one router.
fn talk_to(gateway: Ipv4Addr) -> Result<UdpSocket, MapError> {
    let trouble = |why: std::io::Error| MapError::Socket {
        gateway,
        why: why.to_string(),
    };
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).map_err(trouble)?;
    socket.set_read_timeout(Some(PATIENCE)).map_err(trouble)?;
    // Connected, so the kernel drops anything from an address that is not the
    // router — a reply to a port-mapping request is a thing worth being fussy
    // about the source of.
    socket.connect((gateway, wire::PORT)).map_err(trouble)?;
    Ok(socket)
}

/// What came of asking.
enum Speaking {
    /// Nothing answered.
    Silence,
    /// Something answered, and it was not a yes.
    Said(Rejected),
}

/// Send a request and read the reply, with the RFCs' backoff.
fn exchange(socket: &UdpSocket, request: &[u8]) -> Option<Vec<u8>> {
    let mut wait = PATIENCE;
    for _ in 0..ATTEMPTS {
        let _ = socket.set_read_timeout(Some(wait));
        if socket.send(request).is_err() {
            return None;
        }
        let mut buffer = [0u8; 1100];
        if let Ok(got) = socket.recv(&mut buffer) {
            return buffer.get(..got).map(<[u8]>::to_vec);
        }
        wait = wait.saturating_mul(2);
    }
    None
}

fn ask_pcp(
    socket: &UdpSocket,
    client: Ipv4Addr,
    nonce: &Nonce,
    internal_port: u16,
    lifetime: u32,
) -> Result<Granted, Speaking> {
    // The external port asked for is the internal one: a host whose public
    // port matches its private one is a host whose remembered address stays
    // true, and a router free to choose another will say so in its answer.
    let request = wire::pcp_request(client, nonce, internal_port, internal_port, lifetime);
    let reply = exchange(socket, &request).ok_or(Speaking::Silence)?;
    wire::pcp_response(&reply, nonce, internal_port).map_err(Speaking::Said)
}

fn ask_natpmp(
    socket: &UdpSocket,
    internal_port: u16,
    lifetime: u32,
) -> Result<(u16, u32), Speaking> {
    let request = wire::natpmp_request(internal_port, internal_port, lifetime);
    let reply = exchange(socket, &request).ok_or(Speaking::Silence)?;
    wire::natpmp_response(&reply, internal_port).map_err(Speaking::Said)
}

fn ask_natpmp_address(socket: &UdpSocket) -> Option<Ipv4Addr> {
    let reply = exchange(socket, &wire::natpmp_address_request())?;
    wire::natpmp_address_response(&reply).ok()
}

fn record_path(state_dir: &Path) -> PathBuf {
    state_dir.join(RECORD)
}

/// Write the record, unless one is already there.
fn remember(state_dir: &Path, what: &Mapping) -> Result<(), MapError> {
    if recorded(state_dir)?.is_some() {
        return Ok(());
    }
    remember_over(state_dir, what)
}

/// Write the record, replacing whatever was there.
fn remember_over(state_dir: &Path, what: &Mapping) -> Result<(), MapError> {
    std::fs::create_dir_all(state_dir).map_err(|e| MapError::Record(e.to_string()))?;
    let body = serde_json::to_vec_pretty(what).map_err(|e| MapError::Record(e.to_string()))?;
    std::fs::write(record_path(state_dir), body).map_err(|e| MapError::Record(e.to_string()))
}

fn recorded(state_dir: &Path) -> Result<Option<Outstanding>, MapError> {
    match std::fs::read(record_path(state_dir)) {
        Ok(body) => match serde_json::from_slice(&body) {
            Ok(mapping) => Ok(Some(mapping)),
            Err(why) => {
                // A record that cannot be read would block every future
                // release. Said out loud and dropped.
                tracing::warn!(%why, "the port-mapping record is unreadable; discarding it");
                let _ = std::fs::remove_file(record_path(state_dir));
                Ok(None)
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(MapError::Record(e.to_string())),
    }
}

fn forget(state_dir: &Path) -> Result<(), MapError> {
    match std::fs::remove_file(record_path(state_dir)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(MapError::Record(e.to_string())),
    }
}
