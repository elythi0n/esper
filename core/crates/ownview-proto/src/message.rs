//! Control messages.
//!
//! Field names are stable: they may be added, never renamed, never repurposed.
//! Unknown fields are ignored on decode so a newer peer can talk to an older
//! one without a flag day — that is why nothing here is `deny_unknown_fields`.

use serde::{Deserialize, Serialize};

use crate::codes::Reason;
use serde_bytes;

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

/// What a paired device is allowed to do.
///
/// Each is granted separately, and each direction of a two-way capability is
/// its own permission: a device permitted to *read* this machine's clipboard is
/// not thereby permitted to *write* to it. Two-way capabilities that are one
/// permission are how a feature nobody asked for gets enabled by a feature
/// somebody did.
///
/// This lives in the protocol crate because it is both halves of the same
/// thing: the wire vocabulary a controller uses to request access, and the
/// policy vocabulary a host uses to grant it. Defining it twice would let the
/// two drift, and a permission that means one thing on the wire and another in
/// the store is a security bug waiting for a rename.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    /// See the screen.
    View,
    /// Inject keyboard and mouse.
    Control,
    /// Read this machine's clipboard.
    ClipboardOut,
    /// Write to this machine's clipboard.
    ClipboardIn,
    /// Receive files from this machine.
    FilesOut,
    /// Send files to this machine.
    FilesIn,
    /// Hear this machine's audio.
    Audio,
    /// Open a terminal session.
    Terminal,
    /// Connect with nobody present to approve.
    Unattended,
    /// Record the session locally.
    Record,
}

impl Permission {
    /// Permissions a freshly paired device receives.
    ///
    /// View and control, prompt on connect, nothing else — and emphatically not
    /// [`Permission::Unattended`], which is the dangerous one and is granted
    /// only by a human at the host.
    #[must_use]
    pub fn defaults() -> Vec<Self> {
        vec![Self::View, Self::Control]
    }

    /// Whether granting this permission should require explicit confirmation at
    /// the host rather than a checkbox.
    #[must_use]
    pub const fn is_sensitive(self) -> bool {
        matches!(self, Self::Unattended | Self::FilesIn | Self::Record)
    }
}

/// A SPAKE2 message, sent once in each direction during pairing.
///
/// Carries the sender's public key alongside the PAKE message. The key is a
/// *claim* at this point — pairing is what establishes whether to believe it,
/// and the confirmation MAC covers it so it cannot be swapped afterwards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairSpake {
    /// The SPAKE2 message.
    #[serde(with = "serde_bytes")]
    pub spake: Vec<u8>,
    /// The sender's Ed25519 public key.
    #[serde(with = "serde_bytes_array")]
    pub device: [u8; DEVICE_KEY_LEN],
}

/// Longest SPAKE2 message accepted.
///
/// An Ed25519-group SPAKE2 message is 33 bytes. The cap is generous enough for
/// a future group and small enough that a peer cannot make us allocate: this is
/// pairing, so the peer is unauthenticated by definition.
pub const MAX_SPAKE_LEN: usize = 256;

impl PairSpake {
    /// Whether this message is within its bounds.
    ///
    /// Checked before the message reaches any cryptography. A peer at this
    /// stage has proven nothing at all.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        !self.spake.is_empty() && self.spake.len() <= MAX_SPAKE_LEN
    }
}

/// A key confirmation MAC, proving the sender derived the same SPAKE2 key.
///
/// Without this step a wrong code is indistinguishable from success: SPAKE2
/// returns a key either way. See the pairing module in `ownview-crypto`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairConfirm {
    /// HMAC-SHA-256 over the pairing transcript.
    #[serde(with = "serde_bytes_array")]
    pub mac: [u8; 32],
}

/// A controller asking to open a session.
///
/// Requests are a *maximum*, never a demand. The host grants some subset — see
/// [`SessionGrant`] — and the controller renders what it was given rather than
/// what it asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRequest {
    /// The permissions the controller would like.
    pub want: Vec<Permission>,
    /// Video codecs the controller can decode, in preference order.
    pub codecs: Vec<String>,
    /// A short human reason, shown on the host's prompt. Peer-supplied and
    /// untrusted: bounded, and never interpreted as markup.
    pub reason: String,
}

/// Longest accepted session reason, in bytes.
pub const MAX_REASON_LEN: usize = 120;

/// Most permissions a single request may name.
///
/// The set is small and fixed, so a request naming more than all of them twice
/// over is either broken or probing. Bounded because a request arrives from an
/// authenticated peer but an authenticated peer is not a trusted one.
pub const MAX_REQUESTED_PERMISSIONS: usize = 32;

impl SessionRequest {
    /// Whether this request is within its bounds.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        self.reason.len() <= MAX_REASON_LEN
            && !self.reason.chars().any(char::is_control)
            && self.want.len() <= MAX_REQUESTED_PERMISSIONS
            && self.codecs.len() <= 16
    }
}

/// The host opening a session, with the permissions it actually granted.
///
/// `granted` may be smaller than what was requested, and frequently is. **The
/// controller must render what it holds, never what it asked for**: a client
/// that assumes its request was honoured will offer a control cursor on a
/// view-only session and confuse the person using it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionGrant {
    /// An opaque identifier for this session, for logs and the audit trail.
    #[serde(with = "serde_bytes_array")]
    pub session_id: [u8; 16],
    /// What the controller may actually do.
    pub granted: Vec<Permission>,
}

impl SessionGrant {
    /// Whether a permission was granted.
    #[must_use]
    pub fn allows(&self, p: Permission) -> bool {
        self.granted.contains(&p)
    }
}

/// The host refusing a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionDeny {
    /// Why, as a code the controller can branch on.
    pub reason: Reason,
    /// Why, as a sentence a person can act on.
    pub detail: String,
}

/// The host narrowing or widening a live session.
///
/// Sent whenever policy changes mid-session — a human revoking control, a
/// schedule ending. The controller must apply it immediately; there is no
/// negotiation and no acknowledgement, because a permission the host has
/// withdrawn is withdrawn whether or not the controller replies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionChange {
    /// The complete new set, not a delta.
    ///
    /// A delta invites a controller and host that disagree about the current
    /// state to drift further apart with every message. The full set cannot
    /// drift.
    pub granted: Vec<Permission>,
}

/// The host's response to a session request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionResponse {
    /// Granted, with the permissions actually given.
    Grant(SessionGrant),
    /// Refused, with a reason.
    Deny(SessionDeny),
}

/// The host's reply to the controller's opening message.
///
/// Tagged, because the host has two honest answers and the controller must be
/// able to read either. It can continue — supplying its own SPAKE2 message — or
/// refuse before the exchange begins, which is what an expired or exhausted
/// code produces.
///
/// Without the tag the controller would be waiting for a [`PairSpake`] while
/// the host sent a [`PairResult`], and the mismatch would surface as a decode
/// failure rather than the reason the host actually gave.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairResponse {
    /// Continuing. Here is the host's SPAKE2 message and public key.
    Spake(PairSpake),
    /// Refusing before the exchange starts — an expired or exhausted code.
    Refused(PairResult),
}

/// The outcome the host reports, so the controller can tell a typo from a dead
/// code without guessing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairResult {
    /// The host's confirmation verified, and here is the host's own MAC.
    ///
    /// **The MAC is the point, not the variant.** A man in the middle can send
    /// `Ok` as easily as the host can; only a MAC over a transcript covering
    /// this connection's channel binding proves the sender knew the code. The
    /// controller must verify it before storing anything.
    Ok {
        #[serde(with = "serde_bytes_array")]
        mac: [u8; 32],
    },
    /// The confirmation did not verify — a wrong code, or interference.
    /// Deliberately does not distinguish, because nothing can.
    Failed { attempts_remaining: u8 },
    /// The code's sixty seconds elapsed.
    Expired,
    /// The attempt budget is spent; the host must show a new code.
    Exhausted,
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
