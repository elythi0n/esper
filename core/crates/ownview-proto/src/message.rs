//! Control messages.
//!
//! Field names are stable: they may be added, never renamed, never repurposed.
//! Unknown fields are ignored on decode so a newer peer can talk to an older
//! one without a flag day — that is why nothing here is `deny_unknown_fields`.

use serde::{Deserialize, Serialize};

/// Which side of a session a peer is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// The machine being viewed and controlled.
    Host,
    /// The machine doing the viewing and controlling.
    Controller,
}

/// Length of an Ed25519 public key.
pub const DEVICE_KEY_LEN: usize = 32;

/// Longest accepted peer-supplied display name, in bytes.
///
/// Peer-supplied and therefore untrusted: it is bounded here so a hostile name
/// cannot become a memory or layout problem, and it is never interpreted as
/// markup, a path, or a shell word.
pub const MAX_NAME_LEN: usize = 64;

/// The first control message in both directions.
///
/// Feature evolution happens through [`Hello::caps`], which is intersected —
/// never through a version comparison in feature code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    /// Wire protocol version. Must match exactly.
    pub proto: u16,
    /// Engine version. Informational; never used for feature decisions.
    pub engine: String,
    /// Ed25519 public key identifying this device.
    #[serde(with = "serde_bytes_array")]
    pub device: [u8; DEVICE_KEY_LEN],
    /// Human label. Peer-supplied and untrusted.
    pub name: String,
    /// Which side this peer is.
    pub role: Role,
    /// Capability tokens, e.g. `"clipboard"`, `"files"`, `"audio"`.
    pub caps: Vec<String>,
}

impl Hello {
    /// Build a `Hello` for this build, truncating an over-long name to
    /// [`MAX_NAME_LEN`] on a character boundary.
    #[must_use]
    pub fn new(role: Role, device: [u8; DEVICE_KEY_LEN], name: &str) -> Self {
        Self {
            proto: crate::PROTOCOL_VERSION,
            engine: env!("CARGO_PKG_VERSION").to_owned(),
            device,
            name: truncate_on_char_boundary(name, MAX_NAME_LEN).to_owned(),
            role,
            caps: Vec::new(),
        }
    }

    /// Whether this peer's protocol version matches ours.
    #[must_use]
    pub const fn version_matches(&self) -> bool {
        self.proto == crate::PROTOCOL_VERSION
    }

    /// Capabilities both peers advertise, sorted and deduplicated.
    ///
    /// Set intersection is the only sanctioned way to decide what a session
    /// supports. Comparing versions in feature code is how a protocol rots.
    #[must_use]
    pub fn common_caps(&self, other: &Self) -> Vec<String> {
        let theirs: std::collections::BTreeSet<&str> =
            other.caps.iter().map(String::as_str).collect();
        let mut common: Vec<String> = self
            .caps
            .iter()
            .filter(|c| theirs.contains(c.as_str()))
            .cloned()
            .collect();
        common.sort();
        common.dedup();
        common
    }

    /// Reject a name that is over-long or contains control characters.
    ///
    /// Applied to a *received* `Hello`: the sender is not trusted to have used
    /// [`Hello::new`].
    #[must_use]
    pub fn name_is_acceptable(&self) -> bool {
        self.name.len() <= MAX_NAME_LEN && !self.name.chars().any(char::is_control)
    }
}

/// Truncate to at most `max` bytes without splitting a UTF-8 character.
fn truncate_on_char_boundary(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.get(..end).unwrap_or("")
}

/// CBOR-encodes a fixed-size byte array as a byte string rather than an array
/// of integers. Without this a 32-byte key costs ~33 bytes instead of ~34 as an
/// array of 32 separately-tagged integers, and — more importantly — the Go side
/// would have to agree on which representation was meant.
mod serde_bytes_array {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer, const N: usize>(v: &[u8; N], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(v)
    }

    pub fn deserialize<'de, D: Deserializer<'de>, const N: usize>(
        d: D,
    ) -> Result<[u8; N], D::Error> {
        let v = serde_bytes::ByteBuf::deserialize(d)?;
        <[u8; N]>::try_from(v.as_ref()).map_err(|_| {
            serde::de::Error::invalid_length(v.len(), &"a byte string of the expected length")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello(caps: &[&str]) -> Hello {
        let mut h = Hello::new(Role::Host, [1u8; 32], "host");
        h.caps = caps.iter().map(|s| (*s).to_owned()).collect();
        h
    }

    #[test]
    fn new_sets_the_current_protocol_version() {
        assert!(Hello::new(Role::Host, [0u8; 32], "x").version_matches());
    }

    #[test]
    fn version_mismatch_is_detected() {
        let mut h = Hello::new(Role::Controller, [0u8; 32], "x");
        h.proto = crate::PROTOCOL_VERSION + 1;
        assert!(!h.version_matches());
    }

    #[test]
    fn caps_intersect_and_are_sorted() {
        let a = hello(&["files", "audio", "clipboard"]);
        let b = hello(&["clipboard", "audio", "multimon"]);
        assert_eq!(a.common_caps(&b), vec!["audio", "clipboard"]);
    }

    #[test]
    fn caps_intersection_is_symmetric() {
        let a = hello(&["files", "audio"]);
        let b = hello(&["audio", "multimon"]);
        assert_eq!(a.common_caps(&b), b.common_caps(&a));
    }

    #[test]
    fn an_unknown_cap_is_simply_absent_from_the_intersection() {
        let a = hello(&["telepathy"]);
        let b = hello(&["audio"]);
        assert!(a.common_caps(&b).is_empty());
    }

    #[test]
    fn long_names_are_truncated_not_rejected_at_construction() {
        let h = Hello::new(Role::Host, [0u8; 32], &"a".repeat(500));
        assert_eq!(h.name.len(), MAX_NAME_LEN);
        assert!(h.name_is_acceptable());
    }

    #[test]
    fn truncation_never_splits_a_utf8_character() {
        // 'é' is two bytes, so 64 bytes lands mid-character on odd boundaries.
        for n in 1..80 {
            let h = Hello::new(Role::Host, [0u8; 32], &"é".repeat(n));
            assert!(h.name.len() <= MAX_NAME_LEN);
            assert!(std::str::from_utf8(h.name.as_bytes()).is_ok());
        }
    }

    #[test]
    fn a_received_name_with_control_characters_is_rejected() {
        let mut h = Hello::new(Role::Host, [0u8; 32], "ok");
        h.name = "evil\u{7}\nname".to_owned();
        assert!(!h.name_is_acceptable());
    }

    #[test]
    fn a_received_over_long_name_is_rejected() {
        let mut h = Hello::new(Role::Host, [0u8; 32], "ok");
        h.name = "a".repeat(MAX_NAME_LEN + 1);
        assert!(!h.name_is_acceptable());
    }

    #[test]
    fn device_key_survives_a_cbor_round_trip() {
        let key = [0xABu8; 32];
        let h = Hello::new(Role::Controller, key, "k");
        let mut buf = Vec::new();
        ciborium::into_writer(&h, &mut buf).expect("encode");
        let back: Hello = ciborium::from_reader(buf.as_slice()).expect("decode");
        assert_eq!(back.device, key);
    }

    #[test]
    fn unknown_fields_are_ignored_so_a_newer_peer_can_connect() {
        // A future peer sends a field this build has never heard of.
        let cbor = serde_json::json!({
            "proto": crate::PROTOCOL_VERSION,
            "engine": "9.9.9",
            "device": vec![0u8; 32],
            "name": "future",
            "role": "host",
            "caps": ["audio"],
            "quantum_tunnel": true,
        });
        let mut buf = Vec::new();
        ciborium::into_writer(&cbor, &mut buf).expect("encode");
        let back: Hello = ciborium::from_reader(buf.as_slice()).expect("must ignore unknown field");
        assert_eq!(back.name, "future");
    }
}
