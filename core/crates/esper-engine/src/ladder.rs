//! Working out how to reach a device you named.
//!
//! Somebody says "connect to the machine in the study". Where it *is* is the
//! program's problem, and the answer is different every time: on the same
//! network it is a name being multicast; at home it is where it was last time;
//! from a hotel it is whatever a broker can arrange.
//!
//! # The order, and why it is not only about cost
//!
//! The obvious argument for trying a direct path first is that it is cheaper
//! and faster. The stronger one is about what each rung tells somebody else:
//!
//! - **On the segment** — a multicast question and an answer. Nobody outside
//!   the network learns anything, and neither does anybody on it beyond the
//!   fact that Esper is here, which is already true.
//! - **Where it was last time** — one packet to one address. Nobody is told
//!   anything at all.
//! - **Through a broker** — the broker learns which device asked to meet
//!   which, from where, and when. Unavoidable for an introducer, which is
//!   exactly why it is the *last* rung and not the first: a path that tells
//!   nobody anything is worth trying before one that does.
//!
//! So the direct rungs go together, in parallel, first to answer wins; and the
//! broker is asked only when none of them answered.
//!
//! # A remembered address is a hint
//!
//! Never an authority. Every candidate is dialled the same way and the key
//! check is what decides, so a stale entry costs a failed dial. That is also
//! why an address heard over multicast is safe to try: a device that lies
//! about where it is gets a connection that refuses it.

use std::net::SocketAddr;
use std::time::Duration;

use esper_crypto::Device;

/// How a device was reached, for saying so afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Named on the command line.
    Given,
    /// Heard on this network.
    Lan,
    /// Where it was last reached.
    Remembered,
    /// Over a network the person already runs — WireGuard, Tailscale, ZeroTier.
    ///
    /// Not where the address *came from*, which is the question every other
    /// rung answers, but how a packet to it actually leaves this machine. It
    /// wins over the others when it applies, because "it went over your VPN"
    /// is the more useful of the two facts: it says why no hole was punched
    /// and why no broker was asked.
    Overlay,
    /// Introduced by a broker, on a direct path.
    Broker,
}

impl Tier {
    /// A phrase for a person reading a line of output.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Given => "the address you gave",
            Self::Lan => "this network",
            Self::Remembered => "where it was last time",
            Self::Overlay => "a private network you already run",
            Self::Broker => "an introduction",
        }
    }
}

/// Somewhere to try, and which rung it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rung {
    pub tier: Tier,
    pub addr: SocketAddr,
}

/// How long to listen for a device to answer on the segment.
///
/// A responder answers a multicast question within milliseconds; this is the
/// pause before giving up on it and moving on, and it is spent once. Long
/// enough for a loaded machine on a wireless network, short enough that
/// somebody with no LAN path barely notices.
pub const LISTEN_FOR: Duration = Duration::from_millis(600);

/// Everywhere to try before asking anybody, in the order to try it.
///
/// Deduplicated, because the same address can be both remembered and heard,
/// and dialling it twice would race a machine against itself.
#[must_use]
pub fn direct_rungs(device: &Device, heard: &[(String, SocketAddr)]) -> Vec<Rung> {
    let mut out: Vec<Rung> = Vec::new();
    let wanted = device.fingerprint().hex();

    // The segment first: it is the freshest thing anybody knows, and a device
    // that has just moved is right here and wrong in the store.
    for (fingerprint, addr) in heard {
        if *fingerprint == wanted {
            out.push(Rung {
                tier: Tier::Lan,
                addr: *addr,
            });
        }
    }

    if let Some(addr) = device
        .last_address
        .as_deref()
        .and_then(|a| a.parse::<SocketAddr>().ok())
        && !out.iter().any(|r| r.addr == addr)
    {
        out.push(Rung {
            tier: Tier::Remembered,
            addr,
        });
    }

    // **And where it was over a private network, which is a different
    // question.** A machine reached on the LAN this morning and over
    // WireGuard last week is reachable both ways; keeping only the last one
    // used means the sofa erases the answer that works from a hotel.
    if let Some(addr) = device
        .overlay_address
        .as_deref()
        .and_then(|a| a.parse::<SocketAddr>().ok())
        && !out.iter().any(|r| r.addr == addr)
    {
        out.push(Rung {
            tier: Tier::Overlay,
            addr,
        });
    }

    out
}

/// Mark the rungs whose path leaves through an overlay.
///
/// Applied after the rungs are built rather than while building them, because
/// it is a fact about *this machine's* routing table and not about where the
/// address came from: an address somebody typed, one heard on the segment and
/// one remembered from last week are all private if the packet goes out `wg0`.
pub fn label_private(rungs: &mut [Rung], overlays: &esper_overlay::Overlays) {
    if overlays.is_empty() {
        return;
    }
    for rung in rungs {
        if let Some(over) = overlays.path_to(rung.addr.ip()) {
            tracing::debug!(addr = %rung.addr, interface = %over.name, "a private path");
            rung.tier = Tier::Overlay;
        }
    }
}

/// Which paired device somebody meant.
///
/// A name or a fingerprint prefix, matched case-insensitively, and an
/// **ambiguous answer is refused rather than resolved**: picking one of two
/// machines for somebody who was clearly not sure which they meant is how a
/// screen gets shown to the wrong person.
pub fn device_named<'a>(
    devices: impl Iterator<Item = &'a Device>,
    named: &str,
) -> Result<&'a Device, String> {
    let wanted = named.trim().to_ascii_lowercase();
    if wanted.is_empty() {
        return Err("no device named".to_owned());
    }
    let matches: Vec<&Device> = devices
        .filter(|d| {
            d.name.to_ascii_lowercase() == wanted
                || d.fingerprint().hex().starts_with(&wanted)
                // A name somebody half-typed, which is the common case at a
                // prompt.
                || d.name.to_ascii_lowercase().starts_with(&wanted)
        })
        .collect();

    match matches.as_slice() {
        [only] => Ok(only),
        [] => Err(format!(
            "no paired device is called {named:?} — `esper-engine devices` lists them"
        )),
        several => Err(format!(
            "{} paired devices match {named:?}; say which:\n{}",
            several.len(),
            several
                .iter()
                .map(|d| format!("      {}  {}", d.fingerprint().hex(), d.name))
                .collect::<Vec<_>>()
                .join("\n")
        )),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::net::Ipv4Addr;

    use super::*;

    fn device(name: &str, seed: u8) -> Device {
        let id = esper_crypto::Identity::generate().expect("a key");
        let _ = seed;
        Device::new(id.public_key(), name.to_owned(), 0)
    }

    fn addr(n: u8) -> SocketAddr {
        (Ipv4Addr::new(192, 168, 1, n), 7645).into()
    }

    #[test]
    fn the_segment_comes_before_what_was_remembered() {
        // A device that has just moved is right here and wrong in the store.
        let mut d = device("study", 1);
        d.remember(&addr(9).to_string(), 0);
        let heard = vec![(d.fingerprint().hex(), addr(5))];

        let rungs = direct_rungs(&d, &heard);
        assert_eq!(
            rungs,
            vec![
                Rung {
                    tier: Tier::Lan,
                    addr: addr(5)
                },
                Rung {
                    tier: Tier::Remembered,
                    addr: addr(9)
                },
            ]
        );
    }

    #[test]
    fn an_address_that_is_both_heard_and_remembered_is_tried_once() {
        // Dialling it twice would race a machine against itself, and the
        // second connection is one the host would have to serve.
        let mut d = device("study", 1);
        d.remember(&addr(5).to_string(), 0);
        let heard = vec![(d.fingerprint().hex(), addr(5))];
        assert_eq!(
            direct_rungs(&d, &heard),
            vec![Rung {
                tier: Tier::Lan,
                addr: addr(5)
            }]
        );
    }

    #[test]
    fn somebody_else_on_the_segment_is_not_this_device() {
        // The fingerprint is what identifies it. A name is a label anybody may
        // choose, including somebody who chose yours.
        let d = device("study", 1);
        let other = device("study", 2);
        let heard = vec![(other.fingerprint().hex(), addr(5))];
        assert!(direct_rungs(&d, &heard).is_empty());
    }

    #[test]
    fn a_remembered_address_that_no_longer_parses_is_simply_not_offered() {
        // It is a hint. A store somebody edited by hand should not stop a
        // connection that has other ways to go.
        let mut d = device("study", 1);
        d.last_address = Some("not-an-address".to_owned());
        assert!(direct_rungs(&d, &[]).is_empty());
    }

    #[test]
    fn a_device_is_found_by_name_or_by_fingerprint() {
        let study = device("study", 1);
        let phone = device("phone", 2);
        let all = [study.clone(), phone.clone()];

        assert_eq!(
            device_named(all.iter(), "study").unwrap().public_key,
            study.public_key
        );
        assert_eq!(
            device_named(all.iter(), "STUDY").unwrap().public_key,
            study.public_key
        );
        // Half a name, which is what somebody types at a prompt.
        assert_eq!(
            device_named(all.iter(), "stu").unwrap().public_key,
            study.public_key
        );
        // And a fingerprint prefix, which is what a script pastes.
        let prefix: String = phone.fingerprint().hex().chars().take(8).collect();
        assert_eq!(
            device_named(all.iter(), &prefix).unwrap().public_key,
            phone.public_key
        );
    }

    #[test]
    fn an_ambiguous_name_is_refused_and_lists_what_it_could_have_meant() {
        // Picking one of two machines for somebody who was not sure is how a
        // screen gets shown to the wrong person.
        let a = device("study", 1);
        let b = device("studio", 2);
        let all = [a, b];
        let e = device_named(all.iter(), "stud").unwrap_err();
        assert!(e.contains("say which"), "{e}");
        assert!(e.contains("study") && e.contains("studio"), "{e}");
    }

    #[test]
    fn a_name_nobody_has_says_where_to_look() {
        let all = [device("study", 1)];
        let e = device_named(all.iter(), "kitchen").unwrap_err();
        assert!(e.contains("esper-engine devices"), "{e}");
        assert!(device_named(all.iter(), "").is_err());
        assert!(device_named(all.iter(), "   ").is_err());
    }

    #[test]
    fn every_tier_has_something_to_say() {
        for tier in [Tier::Given, Tier::Lan, Tier::Remembered, Tier::Broker] {
            assert!(!tier.describe().is_empty(), "{tier:?}");
        }
    }

    #[test]
    fn a_private_address_is_its_own_rung_alongside_the_last_one() {
        // The two answer different questions: where it was last time, and
        // where it is from anywhere. Keeping one slot means the sofa erases
        // the answer that works from a hotel.
        let mut d = device("study", 1);
        d.remember(&addr(9).to_string(), 0);
        d.overlay_address = Some("10.125.225.7:7645".to_owned());

        let rungs = direct_rungs(&d, &[]);
        assert_eq!(rungs.len(), 2, "{rungs:?}");
        assert_eq!(rungs.first().map(|r| r.tier), Some(Tier::Remembered));
        assert_eq!(rungs.get(1).map(|r| r.tier), Some(Tier::Overlay));
    }

    #[test]
    fn the_same_address_in_both_slots_is_one_rung() {
        let mut d = device("study", 1);
        d.remember_private("10.125.225.7:7645", 0);
        assert_eq!(direct_rungs(&d, &[]).len(), 1);
    }

    #[test]
    fn a_machine_with_no_overlay_changes_no_rung() {
        let mut rungs = vec![Rung {
            tier: Tier::Remembered,
            addr: addr(5),
        }];
        label_private(&mut rungs, &esper_overlay::Overlays::none());
        assert_eq!(rungs.first().map(|r| r.tier), Some(Tier::Remembered));
    }

    /// A rung whose packets would leave through an overlay is relabelled.
    ///
    /// The overlay is invented — this machine's own address, called `wg0` —
    /// because a test that needed a real tunnel would run on one machine in
    /// the world. What is *not* invented is the routing answer: the kernel is
    /// asked which address it would send from, exactly as in production.
    #[test]
    fn an_address_reached_through_an_overlay_is_labelled_private() {
        // TEST-NET-1, which exists to be routed nowhere in particular: asking
        // for it gets this machine's own outward address, and no packet is
        // sent to find out.
        let Some(me) = esper_overlay::source_towards("192.0.2.1".parse().expect("an address"))
            .filter(|a| !a.is_loopback())
        else {
            println!("no route off this machine, so there is no path to classify");
            return;
        };
        let overlays = esper_overlay::Overlays::from_interfaces(&[("wg0".to_owned(), me)], &[]);

        let mut rungs = vec![
            Rung {
                tier: Tier::Remembered,
                addr: SocketAddr::new(me, 7645),
            },
            Rung {
                tier: Tier::Lan,
                addr: (Ipv4Addr::LOCALHOST, 7645).into(),
            },
        ];
        label_private(&mut rungs, &overlays);
        assert_eq!(
            rungs.first().map(|r| r.tier),
            Some(Tier::Overlay),
            "the address on the overlay was not labelled"
        );
        assert_eq!(
            rungs.get(1).map(|r| r.tier),
            Some(Tier::Lan),
            "loopback is not somebody's private network"
        );
    }
}
