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

/// Open a socket bound to the multicast port, sharing it with whatever else is
/// there, and joined to the group.
pub fn open() -> std::io::Result<tokio::net::UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    // Not available everywhere; where it is not, `SO_REUSEADDR` alone is
    // enough for multicast on that platform.
    #[cfg(all(unix, not(any(target_os = "solaris", target_os = "illumos"))))]
    socket.set_reuse_port(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, PORT)).into())?;
    // The unspecified interface lets the kernel choose, which is the right
    // answer on a machine with one route to the LAN and an acceptable one
    // elsewhere until `discovery.mdns_interfaces` is implemented.
    socket.join_multicast_v4(&GROUP, &Ipv4Addr::UNSPECIFIED)?;
    // 255 because the documents say so. It never leaves the segment — routers
    // do not forward this group — but a lower value is a way to be ignored by
    // an implementation that checks.
    socket.set_multicast_ttl_v4(255)?;
    // Loopback on, so two Espers on one machine can find each other, which
    // is how this gets tested at all.
    socket.set_multicast_loop_v4(true)?;

    let std: StdSocket = socket.into();
    tokio::net::UdpSocket::from_std(std)
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
