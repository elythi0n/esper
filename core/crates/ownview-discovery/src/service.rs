//! What OwnView puts on the wire, and how to read it back.

use crate::name::{MAX_LABEL, Name, NameError};
use crate::wire::{Data, Record};

/// The service type, as it appears in every name here.
pub const SERVICE: &str = "_ownview._udp.local";

/// The TXT key carrying the device fingerprint, as hex.
pub const KEY_FINGERPRINT: &str = "fp";

/// The TXT key carrying the protocol version.
pub const KEY_PROTOCOL: &str = "pv";

/// How long a receiver may cache the records that point at an instance.
///
/// Two minutes. Long enough that a browser opened twice in a minute does not
/// re-query, short enough that a machine that has gone stops being offered
/// soon after — and a responder that leaves cleanly sends a goodbye anyway,
/// so this only covers the machines that did not.
pub const TTL: u32 = 120;

/// The TTL on a pointer record, which the documents suggest be longer.
pub const TTL_PTR: u32 = 4500;

/// What this device says about itself.
#[derive(Debug, Clone)]
pub struct Advertisement {
    /// The device name, as a person set it.
    pub device: String,
    /// The fingerprint, hex, as it appears in the TXT record.
    pub fingerprint: String,
    /// The port the QUIC endpoint is listening on.
    pub port: u16,
    /// The protocol version this build speaks.
    pub protocol: u16,
    /// The address to offer in an `A` record, when there is one worth
    /// offering.
    pub address: Option<std::net::Ipv4Addr>,
    /// The host name to point the `SRV` record at, without `.local`.
    pub host: String,
}

impl Advertisement {
    /// The instance name: `<device> (<fp>)._ownview._udp.local`.
    pub fn instance(&self) -> Result<Name, NameError> {
        let label = instance_name(&self.device, &self.fingerprint);
        Name::parse(SERVICE).and_then(|service| {
            Name::from_labels(vec![label.into_bytes()]).and_then(|first| first.join(&service))
        })
    }

    /// The host name the `SRV` record points at.
    pub fn target(&self) -> Result<Name, NameError> {
        Name::parse(&format!("{}.local", sanitise(&self.host)))
    }

    /// The records that answer a query about this service.
    ///
    /// `PTR` is the answer; the rest are offered alongside it, because a
    /// browser that had to ask three more times to learn where an instance is
    /// would take three more round trips to show anything.
    pub fn records(&self) -> Result<(Record, Vec<Record>), NameError> {
        let instance = self.instance()?;
        let service = Name::parse(SERVICE)?;
        let target = self.target()?;

        let ptr = Record {
            name: service,
            ttl: TTL_PTR,
            // Deliberately not flushed: several instances share this name, and
            // flushing would tell a receiver to forget the others.
            cache_flush: false,
            data: Data::Ptr(instance.clone()),
        };

        let mut extra = vec![
            Record {
                name: instance.clone(),
                ttl: TTL,
                cache_flush: true,
                data: Data::Srv {
                    priority: 0,
                    weight: 0,
                    port: self.port,
                    target: target.clone(),
                },
            },
            Record {
                name: instance,
                ttl: TTL,
                cache_flush: true,
                data: Data::Txt(self.txt()),
            },
        ];
        if let Some(address) = self.address {
            extra.push(Record {
                name: target,
                ttl: TTL,
                cache_flush: true,
                data: Data::A(address),
            });
        }
        Ok((ptr, extra))
    }

    /// The TXT strings, in `key=value` form.
    #[must_use]
    pub fn txt(&self) -> Vec<Vec<u8>> {
        vec![
            format!("{KEY_FINGERPRINT}={}", self.fingerprint).into_bytes(),
            format!("{KEY_PROTOCOL}={}", self.protocol).into_bytes(),
        ]
    }
}

/// The instance label for a device.
///
/// **The fingerprint prefix is why there is no conflict probing.** A responder
/// claiming a name that might already be taken has to probe for it and rename
/// itself if it loses; with the fingerprint in the name, two instances collide
/// only if two device keys do, and a conflict that cannot arise needs no
/// protocol to resolve it.
///
/// The device name is truncated rather than the label refused: a person's
/// machine name is not something this should reject, and the part that matters
/// for uniqueness is at the end.
#[must_use]
pub fn instance_name(device: &str, fingerprint: &str) -> String {
    let short: String = fingerprint.chars().take(8).collect();
    let suffix = format!(" ({short})");
    let room = MAX_LABEL.saturating_sub(suffix.len());
    let mut name = String::new();
    for c in sanitise(device).chars() {
        if name.len().saturating_add(c.len_utf8()) > room {
            break;
        }
        name.push(c);
    }
    if name.is_empty() {
        name.push_str("ownview");
    }
    format!("{name}{suffix}")
}

/// Strip what cannot go in a label.
///
/// A dot would split one label into two and change the name's shape, and a
/// control byte would be escaped by every tool that displayed it.
fn sanitise(text: &str) -> String {
    text.chars()
        .filter(|c| *c != '.' && !c.is_control())
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn ad(device: &str) -> Advertisement {
        Advertisement {
            device: device.to_owned(),
            fingerprint: "c6a8baa216f1078e".to_owned(),
            port: 7645,
            protocol: 1,
            address: Some(std::net::Ipv4Addr::new(192, 168, 1, 20)),
            host: device.to_owned(),
        }
    }

    #[test]
    fn an_instance_name_carries_the_fingerprint_prefix() {
        assert_eq!(instance_name("elythion", "c6a8baa216f1078e"), "elythion (c6a8baa2)");
    }

    /// **A label cannot be longer than 63 bytes**, and a person's machine name
    /// is not something to refuse. The name is truncated and the part that
    /// makes it unique is kept, because a name that collides is a name that
    /// needs a conflict protocol.
    #[test]
    fn a_long_device_name_is_truncated_and_the_fingerprint_survives() {
        let long = "a".repeat(200);
        let name = instance_name(&long, "c6a8baa216f1078e");
        assert!(name.len() <= MAX_LABEL, "{} bytes", name.len());
        assert!(name.ends_with(" (c6a8baa2)"), "{name}");
        // And it is still a legal name once the service is appended.
        let mut ad = ad("x");
        ad.device = long;
        ad.instance().expect("a legal instance name");
    }

    /// Truncation counts bytes, not characters: a label's limit is in bytes,
    /// and cutting a multi-byte character in half would produce a label no
    /// implementation can display.
    #[test]
    fn truncation_does_not_split_a_character() {
        let name = instance_name(&"é".repeat(100), "c6a8baa216f1078e");
        assert!(name.len() <= MAX_LABEL);
        assert!(
            std::str::from_utf8(name.as_bytes()).is_ok(),
            "the name must still be text"
        );
    }

    #[test]
    fn a_name_with_nothing_usable_in_it_still_produces_an_instance() {
        let name = instance_name("...", "c6a8baa216f1078e");
        assert_eq!(name, "ownview (c6a8baa2)");
    }

    /// A dot in a device name would split one label into two and change the
    /// name's shape — `my.laptop` would look like a subdomain rather than a
    /// machine.
    #[test]
    fn a_dot_in_a_device_name_does_not_become_a_label_boundary() {
        let ad = ad("my.laptop");
        let instance = ad.instance().expect("an instance name");
        assert_eq!(
            instance.labels().len(),
            4,
            "one label for the instance, three for the service: {instance}"
        );
        assert!(instance.to_string().starts_with("mylaptop ("), "{instance}");
    }

    #[test]
    fn the_records_describe_the_service_completely() {
        let ad = ad("elythion");
        let (ptr, extra) = ad.records().expect("records");
        assert!(matches!(ptr.data, Data::Ptr(_)));
        assert_eq!(ptr.name, Name::parse(SERVICE).expect("the service"));
        assert!(
            !ptr.cache_flush,
            "the shared pointer must not tell a receiver to forget the others"
        );

        let kinds: Vec<u16> = extra.iter().map(|r| r.data.kind()).collect();
        assert!(kinds.contains(&crate::wire::TYPE_SRV));
        assert!(kinds.contains(&crate::wire::TYPE_TXT));
        assert!(kinds.contains(&crate::wire::TYPE_A));
        assert!(extra.iter().all(|r| r.cache_flush));
    }

    #[test]
    fn a_device_with_no_address_still_advertises() {
        let mut ad = ad("elythion");
        ad.address = None;
        let (_, extra) = ad.records().expect("records");
        assert!(
            extra.iter().all(|r| r.data.kind() != crate::wire::TYPE_A),
            "no address record is offered rather than an invented one"
        );
        // The port is still there, which with the packet's source address is
        // everything a browser needs.
        assert!(
            extra
                .iter()
                .any(|r| matches!(r.data, Data::Srv { port: 7645, .. }))
        );
    }

    #[test]
    fn the_text_record_carries_the_fingerprint_and_the_version() {
        let txt = ad("elythion").txt();
        let strings: Vec<String> = txt
            .iter()
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect();
        assert_eq!(strings, vec!["fp=c6a8baa216f1078e", "pv=1"]);
    }
}
