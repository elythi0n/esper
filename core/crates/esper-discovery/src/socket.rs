//! The multicast socket, and the two options without which nothing works.
//!
//! # `SO_REUSEPORT`
//!
//! Port 5353 is shared. On an ordinary desktop `avahi-daemon` holds it, and so
//! does every browser doing local device discovery — on the machine this was
//! written on, Chrome had it open before Esper existed. A socket that
//! insisted on the port to itself would simply fail to start, so the option is
//! not a nicety: it is the difference between working on a real machine and
//! working only on an empty one.
//!
//! # Joining the group
//!
//! Multicast is opt-in per socket: without joining `224.0.0.251` the kernel
//! delivers nothing, and the symptom is a browser that hears silence on a
//! segment full of announcements.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket as StdSocket};

use socket2::{Domain, Protocol, Socket, Type};

/// The group and port every multicast DNS message goes to.
pub const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
pub const PORT: u16 = 5353;

/// Where a message is sent.
#[must_use]
pub fn destination() -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(GROUP, PORT))
}

/// A socket, and the interfaces it speaks on.
///
/// One socket with several memberships rather than one socket per interface:
/// the same socket can join the same group through as many interfaces as it
/// likes, and receiving from all of them on one socket is what keeps the
/// read loop a read loop instead of a select over a list that changes.
///
/// Sending is the part that is per-interface. A multicast datagram leaves
/// through exactly one interface — whichever `IP_MULTICAST_IF` names — so an
/// announcement on three networks is three sends.
#[derive(Debug)]
pub struct Multicast {
    socket: tokio::net::UdpSocket,
    /// The interface addresses to send through, in order. Empty means the one
    /// the kernel would choose, which is the right answer on a machine with
    /// one route to the LAN.
    through: Vec<Ipv4Addr>,
}

impl Multicast {
    /// The socket underneath, for reading.
    #[must_use]
    pub const fn socket(&self) -> &tokio::net::UdpSocket {
        &self.socket
    }

    /// Where this machine announces itself, for a log line.
    #[must_use]
    pub fn describe(&self) -> String {
        if self.through.is_empty() {
            return "the interface the system chooses".to_owned();
        }
        self.through
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Send to one address.
    ///
    /// To the group, that means **once through each interface**: a datagram
    /// goes out one of them, so a machine on two networks that sent once would
    /// be invisible on one of them. To anything else it is one send, because a
    /// unicast reply follows the routing table like any other packet.
    pub async fn send_to(&self, bytes: &[u8], to: SocketAddr) -> std::io::Result<()> {
        if to != destination() || self.through.is_empty() {
            self.socket.send_to(bytes, to).await?;
            return Ok(());
        }
        let mut sent = 0usize;
        let mut last = None;
        for interface in &self.through {
            // Set per send rather than once: the socket is shared by every
            // interface it speaks on, and the option is what says which.
            if let Err(e) = set_multicast_if(&self.socket, *interface) {
                last = Some(e);
                continue;
            }
            match self.socket.send_to(bytes, to).await {
                Ok(_) => sent = sent.saturating_add(1),
                Err(e) => last = Some(e),
            }
        }
        // One interface working is a working announcement. All of them failing
        // is a failure, and it is the last error that says why.
        match (sent, last) {
            (0, Some(e)) => Err(e),
            _ => Ok(()),
        }
    }
}

/// Point this socket's multicast sends at one interface.
fn set_multicast_if(socket: &tokio::net::UdpSocket, interface: Ipv4Addr) -> std::io::Result<()> {
    // socket2 borrows the file descriptor rather than owning it, so the socket
    // is not closed when this goes out of scope.
    let borrowed = socket2::SockRef::from(socket);
    borrowed.set_multicast_if_v4(&interface)
}

/// Open a socket bound to the multicast port, sharing it with whatever else is
/// there, and joined to the group on each of `interfaces`.
///
/// An empty list joins on the interface the kernel chooses, which is what a
/// machine with one route to the LAN wants and what happens when this machine's
/// interfaces cannot be listed at all.
///
/// A join that fails is **reported and skipped**, not fatal: an interface that
/// went away between being listed and being joined is ordinary, and so is a
/// second address on a wire already joined. What would not be ordinary is a
/// host that refuses to start because one of five networks was busy.
pub fn open(interfaces: &[Ipv4Addr]) -> std::io::Result<Multicast> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    // Not available everywhere; where it is not, `SO_REUSEADDR` alone is
    // enough for multicast on that platform.
    #[cfg(all(unix, not(any(target_os = "solaris", target_os = "illumos"))))]
    socket.set_reuse_port(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, PORT)).into())?;

    let mut joined: Vec<Ipv4Addr> = Vec::new();
    for interface in interfaces {
        match socket.join_multicast_v4(&GROUP, interface) {
            Ok(()) => joined.push(*interface),
            Err(e) => tracing::debug!(%interface, error = %e, "could not join the group there"),
        }
    }
    if joined.is_empty() {
        // Either nobody named an interface, or none of them could be joined.
        // The kernel's choice is the honest fallback: it is what this did
        // before there was a choice at all.
        socket.join_multicast_v4(&GROUP, &Ipv4Addr::UNSPECIFIED)?;
    }

    // 255 because the documents say so. It never leaves the segment — routers
    // do not forward this group — but a lower value is a way to be ignored by
    // an implementation that checks.
    socket.set_multicast_ttl_v4(255)?;
    // Loopback on, so two Espers on one machine can find each other, which
    // is how this gets tested at all.
    socket.set_multicast_loop_v4(true)?;

    let std: StdSocket = socket.into();
    Ok(Multicast {
        socket: tokio::net::UdpSocket::from_std(std)?,
        through: joined,
    })
}

/// The address this machine would use to reach `peer`, learned without asking
/// the operating system to enumerate its interfaces.
///
/// A connected UDP socket sends nothing, but the kernel picks a route and a
/// source address to satisfy the connect — so reading the local address back
/// is the routing table's own answer for "which of my addresses reaches that".
///
/// `None` when there is no route, which is the honest answer on a machine with
/// no network: the `A` record is then left out rather than filled with
/// something invented, and a browser learns the address from the packet's
/// source anyway.
#[must_use]
pub fn address_towards(peer: Ipv4Addr) -> Option<Ipv4Addr> {
    let socket = StdSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect(SocketAddrV4::new(peer, PORT)).ok()?;
    match socket.local_addr().ok()? {
        SocketAddr::V4(v4) if !v4.ip().is_unspecified() => Some(*v4.ip()),
        _ => None,
    }
}

/// The address to advertise: whichever of this machine's addresses reaches the
/// multicast group.
#[must_use]
pub fn local_address() -> Option<Ipv4Addr> {
    address_towards(GROUP).filter(|a| !a.is_loopback())
}
