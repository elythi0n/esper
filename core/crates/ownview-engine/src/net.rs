//! Shared plumbing for the commands that use the network.

use std::net::SocketAddr;
use std::sync::Once;

use ownview_crypto::Device;

/// The default port. Unassigned by IANA and easy to remember.
pub const DEFAULT_PORT: u16 = 7645;

/// rustls needs a process-wide crypto provider, installed once.
///
/// The engine installs the same one the transport was built against. Getting
/// this wrong surfaces as a handshake failure with no useful message, so it is
/// done in one place and called by every command that opens a connection.
pub fn install_crypto() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}

/// A single-threaded runtime.
///
/// Single-threaded on purpose: the async side of a session carries about ten
/// control messages a second, and the work that actually needs a thread —
/// capture and encode — gets one of its own rather than an executor's.
pub fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("starting the runtime: {e}"))
}

/// Parse a listen address, allowing a bare port.
///
/// `--listen 7645` is what somebody types; `--listen 0.0.0.0:7645` is what it
/// means. Accepting only the second is a papercut on every first use.
pub fn parse_listen(raw: &str) -> Result<SocketAddr, String> {
    if let Ok(port) = raw.parse::<u16>() {
        return Ok(SocketAddr::from(([0, 0, 0, 0], port)));
    }
    raw.parse().map_err(|_| {
        format!("{raw} is not an address; try a port like 7645 or an address like 0.0.0.0:7645")
    })
}

/// Parse a peer address, allowing the port to be left off.
pub fn parse_addr(raw: &str) -> Result<SocketAddr, String> {
    if let Ok(addr) = raw.parse::<SocketAddr>() {
        return Ok(addr);
    }
    // A bare host or IP: use the default port rather than refusing, because
    // the port is a detail of this product rather than something a person
    // should have to know.
    if let Ok(ip) = raw.parse::<std::net::IpAddr>() {
        return Ok(SocketAddr::new(ip, DEFAULT_PORT));
    }
    // Deliberately no example IP address. A routable literal in this binary
    // is exactly what the smoke test's "nothing connectable" scan looks for,
    // and it found this one — an example in a help string is still a routable
    // address compiled into a product whose premise is that it contacts
    // nobody. Allowlisting it would have weakened the check to keep a hint.
    Err(format!(
        "{raw} is not an address; give an address or hostname, optionally \
         followed by :port (the default port is {DEFAULT_PORT})"
    ))
}

/// A local address to bind when dialling out, matching the peer's family.
///
/// Port zero, because dialling does not need a fixed one — and matching the
/// family matters: binding an IPv4 socket and dialling an IPv6 address fails
/// with an error that says nothing about why.
pub fn any_port(peer: SocketAddr) -> SocketAddr {
    if peer.is_ipv6() {
        SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, 0))
    } else {
        SocketAddr::from(([0, 0, 0, 0], 0))
    }
}

/// A label for this device, for the peer to show a person.
///
/// The hostname when there is one. It is a label and nothing more: identity is
/// the public key, and this is never matched on.
pub fn this_device() -> String {
    std::env::var("OWNVIEW_DEVICE_NAME")
        .ok()
        .filter(|n| !n.trim().is_empty())
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|h| h.trim().to_owned())
                .filter(|h| !h.is_empty())
        })
        .unwrap_or_else(|| "an ownview device".to_owned())
}

/// A device's permissions, for printing.
pub fn permissions(device: &Device) -> String {
    if device.permissions.is_empty() {
        return "nothing".to_owned();
    }
    device
        .permissions
        .iter()
        .map(|p| format!("{p:?}").to_lowercase())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_port_is_a_listen_address() {
        assert_eq!(
            parse_listen("7645").ok(),
            Some(SocketAddr::from(([0, 0, 0, 0], 7645)))
        );
        assert_eq!(
            parse_listen("127.0.0.1:9000").ok(),
            Some(SocketAddr::from(([127, 0, 0, 1], 9000)))
        );
        assert!(parse_listen("not-an-address").is_err());
        // A port out of range is not a port.
        assert!(parse_listen("99999").is_err());
    }

    #[test]
    fn a_bare_address_gets_the_default_port() {
        assert_eq!(
            parse_addr("192.168.1.5").ok(),
            Some(SocketAddr::from(([192, 168, 1, 5], DEFAULT_PORT)))
        );
        assert_eq!(
            parse_addr("192.168.1.5:1234").ok(),
            Some(SocketAddr::from(([192, 168, 1, 5], 1234)))
        );
        assert!(parse_addr("nonsense").is_err());
    }

    /// Binding an IPv4 socket and dialling an IPv6 address fails with an error
    /// that says nothing about why, so the family is matched.
    #[test]
    fn the_local_bind_matches_the_peers_family() {
        let v4: SocketAddr = "192.168.1.5:7645".parse().expect("v4");
        let v6: SocketAddr = "[2001:db8::1]:7645".parse().expect("v6");
        assert!(any_port(v4).is_ipv4());
        assert!(any_port(v6).is_ipv6());
        assert_eq!(any_port(v4).port(), 0);
        assert_eq!(any_port(v6).port(), 0);
    }

    #[test]
    fn a_device_name_is_never_empty() {
        // Whatever the environment, there is always something to show.
        assert!(!this_device().trim().is_empty());
    }
}
