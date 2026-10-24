//! Waking a paired device that is asleep on this segment.
//!
//! # Why this is here at all
//!
//! Every other verb needs the far end to be running. This is the one that does
//! not: a machine somebody wants to reach is a machine that may be asleep, and
//! "turn it on first" is not an answer somebody on the other side of a house —
//! or a country — can act on.
//!
//! # What a magic packet is, and what it is not
//!
//! Six `0xFF` bytes followed by the target's hardware address sixteen times,
//! in a UDP datagram to a broadcast address. The network card watches for that
//! pattern while the machine is off. There is no reply, no acknowledgement and
//! no error: sending one is the whole operation, and a machine that does not
//! wake looks exactly like one that did not hear.
//!
//! **It is not authentication.** Anybody on the segment can send one to any
//! address they know, and this product neither adds to nor subtracts from
//! that. What it adds is knowing the address: a hardware address learned from
//! a session, so somebody does not have to write it down.
//!
//! # Why the address is a hint
//!
//! It is learned from the kernel's neighbour table after a session, the same
//! way the last-known IP address is. A stale one costs a packet that nothing
//! answers, which is the same thing that happens when the machine is unplugged.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};

/// Which device to wake, and where to send to.
#[derive(Debug)]
pub struct WakeOptions {
    /// A prefix of the device's fingerprint.
    pub who: String,
    /// Where to send the packet. `None` is the broadcast address.
    ///
    /// A directed address is for a segment this machine is not on — a router
    /// that forwards directed broadcasts — and it is also what lets this be
    /// tested without a second machine.
    pub to: Option<SocketAddr>,
}

/// The port a magic packet goes to.
///
/// Nine, the discard port, which is what everything else uses. There is no
/// standard and no listener: the card watches the wire rather than a socket,
/// so the port only has to be one nothing else is doing anything with.
pub const PORT: u16 = 9;

/// Wake a device.
///
/// # Errors
///
/// If no paired device matches, none has a hardware address recorded, or the
/// packet cannot be sent.
pub fn run(o: &WakeOptions) -> Result<(), String> {
    let (store, dropped) = crate::paths::devices()?;
    for line in &dropped {
        eprintln!("  · {line}");
    }
    // **The same matcher every other command that names a device uses.** A
    // second rule here means `esper lock kit` finds the kitchen machine and
    // `esper wake kit` does not, and a second wording means one refusal reads
    // as a sentence and the other as a count of nought. Two matchers for one
    // question is also two places to be ambiguous in, and this one resolved
    // nothing — it took an exact name where the other takes a prefix.
    let device = crate::ladder::device_named(store.iter(), &o.who)?;

    let Some(mac) = device.last_mac.as_deref() else {
        return Err(format!(
            "no hardware address is known for {} — connect to it once from this \
             machine while it is awake, and this will remember one",
            device.name
        ));
    };
    let bytes = magic_packet(mac)?;
    let to =
        o.to.unwrap_or_else(|| SocketAddr::from((Ipv4Addr::BROADCAST, PORT)));
    send(&bytes, to)?;

    println!();
    println!("📡 esper wake");
    println!();
    println!("  device     {} ({mac})", device.name);
    println!("  sent       {} bytes to {to}", bytes.len());
    println!();
    // **Said, because there is nothing to wait for.** A magic packet has no
    // reply: a machine that is unplugged, on another segment, or has the
    // feature switched off in its firmware all look the same from here.
    println!("  Nothing answers a magic packet. If it does not come back, the");
    println!("  machine may be unplugged, on another segment, or have wake-on-LAN");
    println!("  switched off in its firmware.");
    Ok(())
}

/// The bytes of a magic packet for a hardware address.
///
/// # Errors
///
/// If the address is not six hex bytes.
pub fn magic_packet(mac: &str) -> Result<Vec<u8>, String> {
    let address = parse_mac(mac)?;
    // Six 0xFF, then the address sixteen times. 6 + 96 = 102 bytes, always.
    let mut out = vec![0xFF; 6];
    for _ in 0..16 {
        out.extend_from_slice(&address);
    }
    Ok(out)
}

/// Read a hardware address written any of the ways people write them.
///
/// Colons, hyphens, dots or nothing at all: a person copying one out of
/// another tool should not have to reformat it, and every separator here means
/// the same thing.
///
/// # Errors
///
/// If it is not six bytes of hex.
pub fn parse_mac(mac: &str) -> Result<[u8; 6], String> {
    let hex: String = mac
        .chars()
        .filter(|c| !matches!(c, ':' | '-' | '.' | ' '))
        .collect();
    if hex.len() != 12 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!(
            "{mac:?} is not a hardware address: six bytes of hex, however they are separated"
        ));
    }
    let mut out = [0u8; 6];
    for (i, slot) in out.iter_mut().enumerate() {
        let at = i * 2;
        let pair = hex
            .get(at..at + 2)
            .ok_or_else(|| "short address".to_owned())?;
        *slot = u8::from_str_radix(pair, 16).map_err(|e| format!("{mac:?}: {e}"))?;
    }
    Ok(out)
}

/// Send the packet.
///
/// Bound to every address rather than one, and broadcast-enabled: a machine
/// with more than one network is a machine where the sleeping device is on
/// exactly one of them, and there is no way to know which.
fn send(bytes: &[u8], to: SocketAddr) -> Result<(), String> {
    let socket = UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)))
        .map_err(|e| format!("opening a socket: {e}"))?;
    socket
        .set_broadcast(true)
        .map_err(|e| format!("asking for broadcast: {e}"))?;
    socket
        .send_to(bytes, to)
        .map_err(|e| format!("sending to {to}: {e}"))?;
    Ok(())
}

/// The hardware address the kernel has for an IP address, if it has one.
///
/// # Why the kernel's table rather than asking the network
///
/// The answer is already there: a session that just ended used that address,
/// so the neighbour entry is fresh. Sending an ARP request instead would mean
/// a raw socket, which needs a capability this product does not want and does
/// not otherwise need.
///
/// Linux only, because `/proc/net/arp` is. Every platform has an equivalent
/// and none of them agree on how to ask; until the one for this platform is
/// written, a device on it simply has no address recorded and `wake` says so.
#[must_use]
pub fn mac_for(ip: IpAddr) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let table = std::fs::read_to_string("/proc/net/arp").ok()?;
        mac_in(&table, ip)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = ip;
        None
    }
}

/// The same, over a table's text, so it can be tested without a network.
///
/// **Linux, and the tests.** The columns are `/proc/net/arp`'s, so this is not
/// a parser another platform could reuse — Windows reads its neighbour table
/// through `GetIpNetTable2`, which is a structure rather than text. Gated
/// rather than allowed as dead code, because off Linux it is not code waiting
/// for a caller: it is code for a file that is not there.
#[cfg(any(target_os = "linux", test))]
#[must_use]
pub fn mac_in(table: &str, ip: IpAddr) -> Option<String> {
    let wanted = ip.to_string();
    for line in table.lines().skip(1) {
        let mut fields = line.split_whitespace();
        let address = fields.next()?;
        if address != wanted {
            continue;
        }
        // IP address, HW type, Flags, HW address, Mask, Device.
        let mac = fields.nth(2)?;
        // **An incomplete entry is not an address.** The kernel keeps a row
        // with `00:00:00:00:00:00` for a neighbour it asked about and never
        // heard from, and remembering that would be remembering a packet that
        // can never wake anything.
        if mac.chars().all(|c| matches!(c, '0' | ':')) {
            return None;
        }
        return parse_mac(mac).ok().map(|_| mac.to_owned());
    }
    None
}

/// What to say when somebody asks, or gets it wrong.
#[must_use]
pub fn usage() -> String {
    let me = crate::net::me();
    format!(
        "usage: {me} wake <fingerprint>|<name> [--to <address>]\n\
         \n\
         Send a magic packet to a paired device that is asleep on this network.\n\
         The hardware address is one this machine learned from a session, so\n\
         connect to the device once while it is awake first.\n\
         \n\
         Nothing answers a magic packet: there is no reply to wait for and no\n\
         way to tell a machine that woke from one that never heard.\n\
         \n\
         options:\n  \
         \x20 --to <addr>   send here instead of the broadcast address, for a\n  \
         \x20               segment this machine is not on\n\
         \n\
         for example: {me} wake elythion"
    )
}

/// Read the arguments.
///
/// # Errors
///
/// On an unknown option, a missing value, or no device named.
pub fn parse(args: &[String]) -> Result<WakeOptions, String> {
    let mut who: Option<String> = None;
    let mut to: Option<SocketAddr> = None;
    let mut i = 0usize;
    while let Some(arg) = args.get(i) {
        match arg.as_str() {
            "--to" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--to needs an address".to_owned())?;
                // **Not `net::parse_addr`**, which fills in this product's
                // own port: a magic packet goes to the discard port, and
                // defaulting to 7645 would send it somewhere no card is
                // watching. A bare address means [`PORT`].
                to = Some(
                    raw.parse::<SocketAddr>()
                        .or_else(|_| raw.parse::<IpAddr>().map(|ip| SocketAddr::from((ip, PORT))))
                        .map_err(|_| format!("--to wants an address, got {raw:?}"))?,
                );
                i += 2;
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown option {other:?}\n\n{}", usage()));
            }
            other => {
                who = Some(other.to_owned());
                i += 1;
            }
        }
    }
    Ok(WakeOptions {
        who: who.ok_or_else(usage)?,
        to,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    /// **A magic packet is one shape and always that shape.**
    ///
    /// Six `0xFF` and the address sixteen times: 102 bytes, whatever the
    /// address is. A card watching the wire matches that pattern and nothing
    /// else, so a packet that is nearly right is a packet that does nothing.
    #[test]
    fn a_magic_packet_is_six_ones_and_the_address_sixteen_times() {
        let bytes = magic_packet("01:23:45:67:89:ab").expect("a valid address");
        assert_eq!(bytes.len(), 102);
        assert_eq!(bytes.get(..6), Some(&[0xFF; 6][..]));
        let rest = bytes.get(6..).expect("102 bytes has a tail");
        assert_eq!(rest.len(), 96);
        for chunk in rest.chunks(6) {
            assert_eq!(chunk, &[0x01, 0x23, 0x45, 0x67, 0x89, 0xAB]);
        }
    }

    /// However somebody writes the address, it is the same packet.
    #[test]
    fn every_way_of_writing_an_address_means_the_same_one() {
        let ways = [
            "01:23:45:67:89:ab",
            "01-23-45-67-89-AB",
            "0123.4567.89ab",
            "0123456789ab",
            " 01:23:45:67:89:AB ",
        ];
        let first = magic_packet(ways[0]).expect("a valid address");
        for way in ways {
            assert_eq!(magic_packet(way).expect(way), first, "{way}");
        }
    }

    /// And something that is not one is refused rather than padded.
    #[test]
    fn something_that_is_not_an_address_is_refused() {
        for wrong in ["", "01:23:45", "01:23:45:67:89:ab:cd", "zz:23:45:67:89:ab"] {
            assert!(magic_packet(wrong).is_err(), "{wrong:?} was accepted");
        }
    }

    /// **The kernel's table, read the way the kernel writes it.**
    #[test]
    fn a_hardware_address_is_read_out_of_the_neighbour_table() {
        let table = "IP address       HW type     Flags       HW address            Mask     Device\n\
                     192.168.1.9      0x1         0x2         aa:bb:cc:dd:ee:ff     *        eth0\n\
                     192.168.1.10     0x1         0x0         00:00:00:00:00:00     *        eth0\n";
        assert_eq!(
            mac_in(table, "192.168.1.9".parse().unwrap()),
            Some("aa:bb:cc:dd:ee:ff".to_owned())
        );
        // **An incomplete entry is not an address.** The kernel keeps a row of
        // zeroes for a neighbour it asked about and never heard from, and
        // remembering that would be remembering a packet that cannot wake
        // anything.
        assert_eq!(mac_in(table, "192.168.1.10".parse().unwrap()), None);
        assert_eq!(mac_in(table, "192.168.1.11".parse().unwrap()), None);
        assert_eq!(mac_in("", "192.168.1.9".parse().unwrap()), None);
    }

    /// `--to` takes an address with or without a port.
    #[test]
    fn a_destination_may_leave_the_port_out() {
        let with = parse(&[
            "elythion".to_owned(),
            "--to".to_owned(),
            "10.0.0.255:9".to_owned(),
        ])
        .expect("an address with a port");
        let without = parse(&[
            "elythion".to_owned(),
            "--to".to_owned(),
            "10.0.0.255".to_owned(),
        ])
        .expect("an address without one");
        assert_eq!(with.to, without.to);
        assert_eq!(without.to.map(|a| a.port()), Some(PORT));
        assert!(
            parse(&[
                "elythion".to_owned(),
                "--to".to_owned(),
                "nowhere".to_owned()
            ])
            .is_err()
        );
        assert!(parse(&[]).is_err());
    }
}
