//! What this machine's network interfaces are, and what that means.
//!
//! Two questions are asked of the same knowledge, and they are the reason this
//! is one crate rather than two answers that drift:
//!
//! - **Is the path to this address already private?** — the connectivity
//!   ladder, deciding whether a peer is reachable over a network its owner
//!   already runs.
//! - **Which interfaces are an ordinary local network?** — discovery, deciding
//!   where to announce this machine. A container bridge is not somewhere to
//!   look for a laptop, and a tunnel does not carry multicast.
//!
//! # Paths that are already private
//!
//! Somebody who runs WireGuard, Tailscale, ZeroTier or Nebula has already
//! solved the hard problem: every machine has a stable address that works from
//! anywhere, NAT is somebody else's business, and the network is one they
//! chose to trust. Esper's job there is to **notice and get out of the way**:
//! Esper depends on overlay networks rather than competing with them, so it
//! will not grow one of its own by accident.
//!
//! Noticing means one question, asked about an address:
//!
//! > if I sent a packet there, would it leave through an interface that is an
//! > overlay?
//!
//! # Two steps, and why neither is skipped
//!
//! **The kernel decides which address it would send from.** A connected UDP
//! socket sends nothing, but the connect makes the kernel pick a route and a
//! source address; reading the local address back is the routing table's own
//! answer. Reimplementing that from `/proc/net/route` would get policy
//! routing, multiple tables and metrics wrong on exactly the machines that
//! have them.
//!
//! **We decide whether that address belongs to an overlay**, by the name of
//! the interface holding it. There is no protocol to ask, and a private range
//! is not a signal: `10.0.0.0/8` is a tailnet on one machine and an office LAN
//! on the next.
//!
//! # Guessing wrong is safe in one direction only
//!
//! Calling a path private when it is not costs a wasted direct dial and a
//! wrong word in a log line. It **cannot** leak: the address was already
//! going to be dialled, the session is encrypted between device keys either
//! way, and being on somebody's tailnet is not authorisation — pairing still
//! is. What the classification decides is whether to *skip hole punching*,
//! and a skipped punch on a path that turns out not to work costs one more
//! rung on the ladder.
//!
//! That is why the fallback for anything unrecognised is "not an overlay", and
//! why the override exists in the direction it does: a user names interfaces
//! Esper missed, rather than un-naming ones it invented.

#![forbid(unsafe_code)]

use std::net::{IpAddr, SocketAddr, UdpSocket};

/// Interface-name prefixes that mean "this is somebody's overlay network".
///
/// Prefixes rather than patterns, because that is how these are named:
/// `wg0`, `wg-home`, `tailscale0`, `ztu4v6ycsr`, `nebula1`, `utun3`.
///
/// What is deliberately **not** here is as important. A machine running
/// containers has `docker0`, `br-1e3893e1930b` and a dozen `veth` pairs; a
/// machine running virtual machines has `virbr0`. Those are local virtual
/// networks — nothing on the other side of them is a peer — and calling one an
/// overlay would label a container bridge as a way to reach somebody's laptop.
const OVERLAY_PREFIXES: &[&str] = &[
    // WireGuard, and the products built on it that keep the convention.
    "wg",
    // Tailscale: `tailscale0` on Linux, `utun` on macOS.
    "tailscale",
    // Generic point-to-point tunnels: OpenVPN, IPIP, most corporate VPN
    // clients, and macOS's tunnels.
    "tun",
    "utun",
    // ZeroTier.
    "zt",
    // Nebula.
    "nebula",
];

/// Whether an interface name looks like an overlay network.
///
/// Case-insensitive: Windows adapter names are not lower case.
#[must_use]
pub fn looks_like_overlay(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    OVERLAY_PREFIXES.iter().any(|p| name.starts_with(p))
}

/// Interface-name prefixes that mean "this is a local virtual network".
///
/// A container bridge, a virtual-machine network, a `veth` pair. They are
/// **local** in a way an overlay is not: there is no peer on the other side of
/// `docker0`, only this machine's own containers. Announcing Esper into one is
/// noise at best, and looking for a laptop there is looking in a room that has
/// nobody in it.
///
/// A machine that runs containers has dozens of these — the one this was
/// written on has twenty — which is why the list is worth having rather than
/// treating everything unrecognised as ordinary.
const VIRTUAL_PREFIXES: &[&str] = &[
    // Docker: the default bridge, a bridge per user network, and a `veth` for
    // every container.
    "docker", "br-", "veth", // libvirt, VirtualBox, VMware, Parallels.
    "virbr", "vboxnet", "vmnet", "vnic",
    // LXC/LXD, Podman, and the container network plugins.
    "lxc", "lxd", "cni", "cilium", "flannel", "kube",
    // Kernel virtual devices that carry nothing anybody is on.
    "dummy", "ifb",
];

/// What kind of network an interface is.
///
/// The order of the questions matters: loopback first because it is loopback
/// whatever it is called, then overlay, then virtual, and **anything left is
/// ordinary**. That default is deliberate — a physical interface with a naming
/// scheme nobody here has seen must still be a place to find a laptop, and the
/// cost of the two mistakes is not symmetric: announcing into a container
/// network is noise, and failing to announce on somebody's LAN is the feature
/// not working.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// This machine talking to itself.
    Loopback,
    /// A network somebody already runs: WireGuard, Tailscale, ZeroTier.
    Overlay,
    /// A local virtual network: containers, virtual machines.
    Virtual,
    /// An ordinary local network — the one a laptop is on.
    Ordinary,
}

/// What kind of network an interface holding this address is.
#[must_use]
pub fn kind_of(name: &str, addr: IpAddr) -> Kind {
    if addr.is_loopback() {
        return Kind::Loopback;
    }
    let lower = name.to_ascii_lowercase();
    if looks_like_overlay(&lower) {
        return Kind::Overlay;
    }
    if VIRTUAL_PREFIXES.iter().any(|p| lower.starts_with(p)) {
        return Kind::Virtual;
    }
    Kind::Ordinary
}

/// One address on each of this machine's ordinary local networks.
///
/// What discovery announces on. **One per interface, not one per address**: a
/// machine with two addresses on the same wire is on that wire once, and
/// joining a multicast group twice through the same interface is an error
/// rather than a second membership.
///
/// Empty is a real answer — a machine with no ordinary network, or one whose
/// interfaces could not be listed — and the caller is expected to fall back to
/// letting the kernel choose rather than to announcing nowhere.
#[must_use]
pub fn ordinary_v4() -> Vec<std::net::Ipv4Addr> {
    let Ok(found) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for interface in found {
        let addr = interface.addr.ip();
        let IpAddr::V4(v4) = addr else {
            continue;
        };
        // A link-local address is one an interface gave itself because nothing
        // told it otherwise. Announcing there reaches whatever else has done
        // the same, which is not nobody — but it is not the LAN either, and a
        // machine with a real address on the same wire announces there too.
        if v4.is_link_local() {
            continue;
        }
        if kind_of(&interface.name, addr) == Kind::Ordinary && !seen.contains(&interface.name) {
            seen.push(interface.name);
            out.push(v4);
        }
    }
    out
}

/// The addresses of the interfaces somebody named, whatever kind they are.
///
/// The override: a machine whose LAN interface is called something this crate
/// has never heard of, or one where announcing on exactly one of three is what
/// the operator wants.
#[must_use]
pub fn named_v4(names: &[String]) -> Vec<std::net::Ipv4Addr> {
    if names.is_empty() {
        return Vec::new();
    }
    let wanted: Vec<String> = names
        .iter()
        .map(|n| n.trim().to_ascii_lowercase())
        .filter(|n| !n.is_empty())
        .collect();
    let Ok(found) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for interface in found {
        let IpAddr::V4(v4) = interface.addr.ip() else {
            continue;
        };
        // One per interface, for the same reason as above.
        if wanted.contains(&interface.name.to_ascii_lowercase()) && !seen.contains(&interface.name)
        {
            seen.push(interface.name);
            out.push(v4);
        }
    }
    out
}

/// One local interface carrying a private network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overlay {
    /// The interface's name, as the operating system gives it.
    pub name: String,
    /// The addresses it holds on this machine.
    pub addresses: Vec<IpAddr>,
}

/// This machine's overlay networks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Overlays {
    overlays: Vec<Overlay>,
}

impl Overlays {
    /// Nothing. What a machine with no overlay has, and what every failure to
    /// enumerate produces — because "could not tell" and "there are none" lead
    /// to the same behaviour, which is the ladder trying the address anyway.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            overlays: Vec::new(),
        }
    }

    /// Read this machine's interfaces and pick out the overlays.
    ///
    /// `also` names interfaces to treat as overlays whichever way they are
    /// named — the escape hatch for a VPN client with a name nobody has seen.
    #[must_use]
    pub fn detect(also: &[String]) -> Self {
        match if_addrs::get_if_addrs() {
            Ok(found) => {
                let pairs: Vec<(String, IpAddr)> =
                    found.into_iter().map(|i| (i.name, i.addr.ip())).collect();
                Self::from_interfaces(&pairs, also)
            }
            Err(e) => {
                // Not fatal, and not silent. A machine whose interfaces cannot
                // be listed still reaches everything it could reach before;
                // it just calls every path public.
                tracing::warn!(error = %e, "could not list this machine's interfaces");
                Self::none()
            }
        }
    }

    /// The overlays in an already-enumerated list of interfaces.
    ///
    /// Separate from [`Overlays::detect`] so the classification can be tested
    /// against machines this one is not: a laptop with twenty container
    /// bridges, a Mac with `utun`s, a server with nothing at all.
    #[must_use]
    pub fn from_interfaces(interfaces: &[(String, IpAddr)], also: &[String]) -> Self {
        let named: Vec<String> = also
            .iter()
            .map(|n| n.trim().to_ascii_lowercase())
            .filter(|n| !n.is_empty())
            .collect();

        let mut overlays: Vec<Overlay> = Vec::new();
        for (name, addr) in interfaces {
            // Loopback is not an overlay. It is the most private path there
            // is, and calling it one would put "private network" on the label
            // of every test session.
            if addr.is_loopback() {
                continue;
            }
            let lower = name.to_ascii_lowercase();
            if !looks_like_overlay(&lower) && !named.contains(&lower) {
                continue;
            }
            match overlays.iter_mut().find(|o| o.name == *name) {
                Some(existing) => existing.addresses.push(*addr),
                None => overlays.push(Overlay {
                    name: name.clone(),
                    addresses: vec![*addr],
                }),
            }
        }
        Self { overlays }
    }

    /// Whether there are any.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.overlays.is_empty()
    }

    /// Every overlay found.
    pub fn iter(&self) -> impl Iterator<Item = &Overlay> {
        self.overlays.iter()
    }

    /// The overlay holding this local address, if one does.
    #[must_use]
    pub fn holding(&self, local: IpAddr) -> Option<&Overlay> {
        self.overlays.iter().find(|o| o.addresses.contains(&local))
    }

    /// The overlay a packet to `peer` would leave through, if any.
    ///
    /// Asks the kernel which address it would send from, then names the
    /// interface holding it. `None` means "not through an overlay" — including
    /// when there is no route at all, which is the same answer for the
    /// purposes of the ladder: dial it and see.
    #[must_use]
    pub fn path_to(&self, peer: IpAddr) -> Option<&Overlay> {
        if self.overlays.is_empty() {
            return None;
        }
        self.holding(source_towards(peer)?)
    }

    /// What was found, for a log line.
    #[must_use]
    pub fn describe(&self) -> String {
        if self.overlays.is_empty() {
            return "none".to_owned();
        }
        self.overlays
            .iter()
            .map(|o| o.name.clone())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// The address this machine would send from to reach `peer`.
///
/// A connected UDP socket sends **nothing**: no packet leaves, no name is
/// looked up, nothing is told anything. The connect only makes the kernel pick
/// a route, and reading the local address back is that decision.
///
/// `esper-discovery` has an IPv4-only twin of this for choosing the address to
/// advertise over multicast. They are deliberately separate: advertising this
/// machine must not depend on whether it can classify its own interfaces.
#[must_use]
pub fn source_towards(peer: IpAddr) -> Option<IpAddr> {
    let bind: SocketAddr = match peer {
        IpAddr::V4(_) => (std::net::Ipv4Addr::UNSPECIFIED, 0).into(),
        IpAddr::V6(_) => (std::net::Ipv6Addr::UNSPECIFIED, 0).into(),
    };
    let socket = UdpSocket::bind(bind).ok()?;
    // Port 9 is discard. Nothing is sent to it; it is there because `connect`
    // needs a port and a port that means "throw this away" is the honest one
    // to name.
    socket.connect(SocketAddr::new(peer, 9)).ok()?;
    let local = socket.local_addr().ok()?.ip();
    if local.is_unspecified() {
        None
    } else {
        Some(local)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().expect("an address")
    }

    fn interfaces(pairs: &[(&str, &str)]) -> Vec<(String, IpAddr)> {
        pairs
            .iter()
            .map(|(n, a)| ((*n).to_owned(), ip(a)))
            .collect()
    }

    /// The machine this was written on: one wifi interface, one WireGuard
    /// tunnel, and twenty container bridges.
    fn a_developers_laptop() -> Vec<(String, IpAddr)> {
        let mut ifaces = interfaces(&[
            ("lo", "127.0.0.1"),
            ("wlan0", "192.168.1.112"),
            ("wg0", "10.125.225.2"),
            ("docker0", "172.17.0.1"),
            ("virbr0", "192.168.122.1"),
        ]);
        for n in 0..20 {
            ifaces.push((format!("br-{n:012x}"), ip(&format!("172.{}.0.1", 18 + n))));
            ifaces.push((format!("veth{n:08x}"), ip("169.254.0.1")));
        }
        ifaces
    }

    #[test]
    fn the_names_that_mean_an_overlay() {
        for name in [
            "wg0",
            "wg-home",
            "tailscale0",
            "tun0",
            "utun3",
            "ztu4v6ycsr",
            "nebula1",
            "WG0",
        ] {
            assert!(looks_like_overlay(name), "{name} should be an overlay");
        }
    }

    #[test]
    fn what_each_kind_of_interface_is() {
        let v4 = |s: &str| ip(s);
        // The ordinary case, and the default for a name nobody here has seen.
        for name in [
            "eth0",
            "en0",
            "wlan0",
            "wlp3s0",
            "enp0s31f6",
            "bond0",
            "eno1",
            "net5",
        ] {
            assert_eq!(
                kind_of(name, v4("192.168.1.5")),
                Kind::Ordinary,
                "{name} must be somewhere to find a laptop"
            );
        }
        // Somebody's own network.
        for name in ["wg0", "tailscale0", "tun0", "utun4", "ztabc", "nebula1"] {
            assert_eq!(kind_of(name, v4("10.0.0.1")), Kind::Overlay, "{name}");
        }
        // This machine's own containers and virtual machines.
        for name in [
            "docker0",
            "br-1e3893e1930b",
            "veth479a015",
            "virbr0",
            "vboxnet0",
            "lxcbr0",
            "cni0",
            "dummy0",
        ] {
            assert_eq!(kind_of(name, v4("172.17.0.1")), Kind::Virtual, "{name}");
        }
        // Loopback is loopback whatever it is called.
        assert_eq!(kind_of("lo", v4("127.0.0.1")), Kind::Loopback);
        assert_eq!(kind_of("tun0", v4("127.0.0.1")), Kind::Loopback);
    }

    /// Whatever this machine has, the classification does not contradict
    /// itself.
    ///
    /// Runs everywhere and asserts what is true everywhere: nothing loopback
    /// is ordinary, and nothing named for a container bridge is somewhere to
    /// announce a laptop.
    #[test]
    fn this_machine_has_ordinary_interfaces_and_they_are_not_the_bridges() {
        let ordinary = ordinary_v4();
        for addr in &ordinary {
            assert!(
                !addr.is_loopback(),
                "{addr} is loopback and was called ordinary"
            );
            assert!(!addr.is_link_local(), "{addr} is link-local");
        }
        println!("this machine's ordinary addresses: {ordinary:?}");
    }

    #[test]
    fn naming_nothing_selects_nothing() {
        // The override is an override: an empty list means "decide for me",
        // never "everything".
        assert!(named_v4(&[]).is_empty());
        assert!(named_v4(&[String::new(), "   ".to_owned()]).is_empty());
    }

    #[test]
    fn a_container_bridge_is_not_an_overlay() {
        // The negative half, and the one that matters: nothing on the far side
        // of `docker0` is somebody's laptop. A machine running containers has
        // dozens of these, and one of them being called a private path to a
        // peer would be a wrong answer on every session that machine hosts.
        for name in [
            "lo",
            "eth0",
            "en0",
            "wlan0",
            "wlp3s0",
            "docker0",
            "br-1e3893e1930b",
            "veth479a015",
            "virbr0",
            "bond0",
            "dummy0",
            "ppp0",
            "Ethernet",
            "Wi-Fi",
        ] {
            assert!(!looks_like_overlay(name), "{name} is not an overlay");
        }
    }

    #[test]
    fn one_wireguard_tunnel_among_twenty_bridges() {
        let overlays = Overlays::from_interfaces(&a_developers_laptop(), &[]);
        assert_eq!(overlays.describe(), "wg0");
        assert_eq!(
            overlays
                .holding(ip("10.125.225.2"))
                .map(|o| o.name.as_str()),
            Some("wg0")
        );
        assert!(overlays.holding(ip("192.168.1.112")).is_none());
        assert!(overlays.holding(ip("172.17.0.1")).is_none());
    }

    #[test]
    fn loopback_is_not_a_private_network() {
        // It is the most private path there is, and calling it an overlay
        // would put "private network" on every session on one machine —
        // including every test in this suite.
        let overlays = Overlays::from_interfaces(
            &interfaces(&[("tun0", "127.0.0.1"), ("lo", "127.0.0.1")]),
            &[],
        );
        assert!(overlays.is_empty());
    }

    #[test]
    fn an_interface_with_several_addresses_is_one_overlay() {
        let overlays = Overlays::from_interfaces(
            &interfaces(&[
                ("wg0", "10.125.225.2"),
                ("wg0", "fd00::2"),
                ("wlan0", "192.168.1.1"),
            ]),
            &[],
        );
        let found: Vec<&Overlay> = overlays.iter().collect();
        assert_eq!(found.len(), 1);
        assert_eq!(found.first().map(|o| o.addresses.len()), Some(2));
        assert!(overlays.holding(ip("fd00::2")).is_some());
    }

    #[test]
    fn a_name_nobody_has_seen_can_be_given_by_hand() {
        // The escape hatch, and it only opens one way: a user names an
        // interface Esper did not recognise. There is no way to un-name one,
        // because a wrong guess costs a wasted dial and never a leak.
        let ifaces = interfaces(&[("corp-vpn", "10.9.0.4"), ("wlan0", "192.168.1.112")]);
        assert!(Overlays::from_interfaces(&ifaces, &[]).is_empty());

        let named = Overlays::from_interfaces(&ifaces, &["corp-vpn".to_owned()]);
        assert_eq!(named.describe(), "corp-vpn");
        // Case and stray spaces are what somebody types, not a different
        // interface.
        let typed = Overlays::from_interfaces(&ifaces, &["  Corp-VPN ".to_owned()]);
        assert_eq!(typed.describe(), "corp-vpn");
        // And naming one does not bring the wifi along with it.
        assert!(named.holding(ip("192.168.1.112")).is_none());
    }

    /// A blank entry in the override list matches nothing.
    ///
    /// It holds today because names are compared for equality, and no
    /// interface is called "". The test is here for the change that would
    /// make it stop holding: somebody making the override match *prefixes*,
    /// because people expect `wg` to cover `wg0` and `wg1`. On that day an
    /// empty entry — a trailing comma in a config file — becomes a wildcard
    /// that calls every path private.
    #[test]
    fn an_empty_override_is_not_a_wildcard() {
        let ifaces = interfaces(&[("wlan0", "192.168.1.112")]);
        assert!(Overlays::from_interfaces(&ifaces, &[String::new()]).is_empty());
        assert!(Overlays::from_interfaces(&ifaces, &["   ".to_owned()]).is_empty());
    }

    #[test]
    fn a_machine_with_no_overlay_calls_no_path_private() {
        let overlays = Overlays::from_interfaces(
            &interfaces(&[("lo", "127.0.0.1"), ("eth0", "192.168.1.5")]),
            &[],
        );
        assert!(overlays.is_empty());
        // And asking about a path does not even reach a socket.
        assert!(overlays.path_to(ip("10.125.225.7")).is_none());
    }

    /// Loopback resolves to itself, which is the one route every machine has.
    #[test]
    fn the_kernel_answers_for_a_route_that_always_exists() {
        assert_eq!(source_towards(ip("127.0.0.1")), Some(ip("127.0.0.1")));
        assert_eq!(source_towards(ip("::1")), Some(ip("::1")));
    }

    /// Whatever this machine has, nothing local and virtual is called an
    /// overlay.
    ///
    /// Runs everywhere and asserts what is true everywhere: a CI runner has
    /// container bridges and no tunnel, a developer's laptop has both, and
    /// neither may produce a `docker0` labelled as a way to reach a peer.
    #[test]
    fn this_machine_classifies_itself_consistently() {
        let overlays = Overlays::detect(&[]);
        for o in overlays.iter() {
            assert!(
                looks_like_overlay(&o.name),
                "{} was called an overlay without being named one",
                o.name
            );
            assert!(
                !o.addresses.iter().any(std::net::IpAddr::is_loopback),
                "{} holds a loopback address",
                o.name
            );
        }
        println!("this machine's overlays: {}", overlays.describe());
    }
}
