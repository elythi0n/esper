//! Shared plumbing for the commands that use the network.

use std::net::SocketAddr;
use std::sync::Once;

use esper_crypto::Device;

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
    std::env::var("ESPER_DEVICE_NAME")
        .ok()
        .filter(|n| !n.trim().is_empty())
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|h| h.trim().to_owned())
                .filter(|h| !h.is_empty())
        })
        .unwrap_or_else(|| "an esper device".to_owned())
}

/// A device's permissions, for printing.
pub fn permissions(device: &Device) -> String {
    if device.permissions.is_empty() {
        return "nothing".to_owned();
    }
    device
        .permissions
        .iter()
        .map(|p| p.name())
        .collect::<Vec<_>>()
        .join(", ")
}

/// A flag that goes true when the operator asks this process to stop.
///
/// # Why a host has to notice
///
/// Without a handler, `SIGTERM` and `SIGINT` kill the process where it
/// stands — and a host killed where it stands has told nobody. Its mDNS
/// advertisement stays in every browser's cache until it expires, so a person
/// clicks a machine that is not there; and a controller mid-session sees the
/// connection stop rather than being told the host is going, which is the
/// difference between "it shut down" and "something went wrong, we think".
/// [`Reason::HOST_SHUTTING_DOWN`] exists for this and needs somewhere to be
/// sent from.
///
/// # And why it obliges every loop
///
/// Installing a handler replaces the default action. A host that noticed the
/// signal in one loop and not another would be *worse* than one that never
/// noticed: `Ctrl-C` during a session would then do nothing at all. So the
/// receiver is shared, and every loop that can be waiting for a long time
/// waits on it too.
///
/// `SIGTERM` because that is what a service manager sends, and `SIGINT`
/// because that is what a person at a terminal sends.
pub fn stop_signal() -> Result<tokio::sync::watch::Receiver<bool>, String> {
    let (tx, rx) = tokio::sync::watch::channel(false);

    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term =
            signal(SignalKind::terminate()).map_err(|e| format!("listening for SIGTERM: {e}"))?;
        let mut interrupt =
            signal(SignalKind::interrupt()).map_err(|e| format!("listening for SIGINT: {e}"))?;
        tokio::spawn(async move {
            let which = tokio::select! {
                _ = term.recv() => "SIGTERM",
                _ = interrupt.recv() => "interrupted",
            };
            println!();
            println!("  · {which}: shutting down");
            let _ = tx.send(true);
        });
    }
    #[cfg(not(unix))]
    {
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                println!();
                println!("  · interrupted: shutting down");
                let _ = tx.send(true);
            }
        });
    }

    Ok(rx)
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
