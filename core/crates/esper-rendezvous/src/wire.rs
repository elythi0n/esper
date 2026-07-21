//! What a peer says to a broker, and what it hears back.
//!
//! # Why this is not in `esper-proto`
//!
//! It is a different protocol with a different peer. The session protocol runs
//! between two *devices* that have paired, is implemented twice — once in Rust
//! and once in Go — and every message has a committed fixture proving both
//! halves agree. This runs between a device and a **broker**, which the Go
//! control plane never speaks to: it configures the broker's address and pins
//! its key, and the engine does the talking. A second implementation of these
//! messages would be a second implementation of nothing.
//!
//! # What the broker is trusted with, and what it is not
//!
//! It is trusted to forward. It is **not** trusted with the session: every
//! tier is end-to-end encrypted between device keys, and a broker that swapped
//! a candidate address for its own would find the QUIC handshake refusing it,
//! because the key would not match.
//!
//! What it inevitably learns is stated in the design and worth stating here:
//! **which device keys asked to meet which, from which addresses, and when.**
//! That is unavoidable for any introducer and is the whole argument for the
//! broker being yours.
//!
//! # Signed, and mutual
//!
//! Two properties do the security work, and neither needs the broker to hold
//! anything:
//!
//! 1. **A meeting request is signed by the device key it claims.** The broker
//!    verifies it against the key in the message and stores neither. Without
//!    this, anyone who knew two fingerprints could claim to be one of them and
//!    collect the other's candidate addresses — and could fill the broker's
//!    table with meetings that will never happen.
//! 2. **Both sides must name each other.** A peer learns another's candidates
//!    only when that other has also asked for it. A host waiting to be found
//!    names every device it is paired with; a controller names the one host it
//!    wants. Nothing is revealed to a party that only asked.
//!
//! The signature also covers **which broker** it was meant for, so a request
//! captured at one broker cannot be replayed at another, and **when** it was
//! made, so an old one cannot be replayed at all.
//!
//! # No opaque handshake blob
//!
//! The connectivity design describes the peers exchanging "an encrypted
//! handshake blob" through the broker. This implementation does not, and the
//! omission is deliberate: there is nothing to pre-share. The QUIC handshake
//! between the two devices *is* the authenticated exchange, it authenticates by
//! device key, and it happens directly. A field the broker must forward and
//! neither peer puts anything in is a channel for something else to appear in
//! later.

use std::net::SocketAddr;

use esper_crypto::Identity;
use esper_crypto::identity::KEY_LEN;
use serde::{Deserialize, Serialize};

/// The version of this protocol.
///
/// Its own number, not the session protocol's: a broker and an engine are
/// upgraded on their own schedules, and tying them together would mean a
/// broker refusing an engine over a change to a message it never sees.
pub const RENDEZVOUS_VERSION: u16 = 1;

/// Most devices a peer may name in one request.
///
/// A host names everything it is paired with, and a device list that size is
/// already unusual. Bounded because the request arrives from an
/// unauthenticated peer — the signature proves who signed it, not that they
/// mean well.
pub const MAX_WITH: usize = 64;

/// Most addresses a peer may offer.
///
/// A machine with a wired interface, wireless, an overlay and a reflexive
/// address has four. Eight leaves room and refuses a peer trying to make a
/// broker fan out its introductions.
pub const MAX_CANDIDATES: usize = 8;

/// How far from now a request's timestamp may be.
///
/// Clocks disagree, so a window rather than an instant; five minutes is
/// generous for an unsynchronised machine and short enough that a captured
/// request is not useful for long. It is not the security boundary — the
/// signature is — it bounds replay.
pub const CLOCK_SLACK_SECS: u64 = 300;

/// A peer asking to be introduced.
///
/// Symmetric on purpose: a host waiting to be found and a controller looking
/// for one send the same message, and the broker's only rule is that two of
/// them name each other. There is no "register" and no "lookup", so there is
/// no asymmetry for an attacker to stand in the middle of.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meet {
    /// Which version of this protocol the peer speaks.
    pub version: u16,
    /// The asking device's public key. What the signature is checked against.
    #[serde(with = "key_bytes")]
    pub me: [u8; KEY_LEN],
    /// The devices this peer will meet: one, for a controller looking for a
    /// host; the paired list, for a host waiting to be found.
    pub with: Vec<KeyBytes>,
    /// Addresses the peer believes it can be reached at, in the order it would
    /// like them tried.
    pub candidates: Vec<SocketAddr>,
    /// The broker this was meant for, so it cannot be replayed at another.
    #[serde(with = "key_bytes")]
    pub broker: [u8; KEY_LEN],
    /// Seconds since the epoch, as the peer sees it.
    pub at: u64,
    /// Over everything above.
    #[serde(with = "sig_bytes")]
    pub signature: [u8; 64],
}

/// A device key on the wire.
///
/// A newtype because a `Vec<[u8; 32]>` needs an element type serde will encode
/// as bytes rather than as a sixty-four-character array of numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct KeyBytes(#[serde(with = "key_bytes")] pub [u8; KEY_LEN]);

/// What the broker says back.
///
/// A peer may hear more than one: a host stays for as long as it runs and is
/// introduced to each device that comes looking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Answer {
    /// Somebody named you, and you named them.
    Introduced(Introduction),
    /// The broker will not do it, and why.
    Refused(Refusal),
}

/// Who to try, and where.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Introduction {
    /// The peer's device key. The connection to it is authenticated against
    /// this, which is what makes a lying broker harmless.
    #[serde(with = "key_bytes")]
    pub peer: [u8; KEY_LEN],
    /// Where to try, in the order that peer offered them.
    pub candidates: Vec<SocketAddr>,
    /// Where the **broker** saw that peer.
    ///
    /// The one address a peer cannot know about itself: behind a router, its
    /// own socket believes it is on a private address and the world sees a
    /// different one. This is where reflexive candidates come from in Esper —
    /// **from your own broker**, never from a public STUN server, because
    /// asking Google where you are tells Google every time you connect.
    ///
    /// Filled in by the broker rather than signed by the peer, and marked as
    /// such: it is the broker's observation, and a reader of a log should be
    /// able to tell it from what the peer claimed.
    pub seen_at: Option<SocketAddr>,
}

/// Why a broker will not introduce somebody.
///
/// Coarse on purpose. A broker is unauthenticated and internet-facing, so its
/// answers are also an oracle: "your signature is wrong" and "you are asking
/// too often" are worth telling apart because they are actionable, and nothing
/// finer is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Refusal {
    /// The request was outside its bounds.
    Malformed,
    /// The signature does not verify against the key in the request.
    NotSigned,
    /// The request was for a different broker.
    NotThisBroker,
    /// The timestamp is too far from now.
    Stale,
    /// This broker speaks a different version of this protocol.
    Version(u16),
    /// The broker is holding as many peers as it will.
    Full,
    /// Too many from one address.
    TooManyFromYou,
}

impl Refusal {
    /// A sentence for a person, not a code for a machine.
    #[must_use]
    pub fn message(self) -> String {
        match self {
            Self::Malformed => "the broker refused a malformed request".to_owned(),
            Self::NotSigned => "the broker could not verify the request's signature".to_owned(),
            Self::NotThisBroker => "that request was meant for a different broker".to_owned(),
            Self::Stale => {
                "the broker refused the request as stale — check this machine's clock".to_owned()
            }
            Self::Version(theirs) => format!(
                "the broker speaks rendezvous version {theirs} and this build speaks \
                 {RENDEZVOUS_VERSION}"
            ),
            Self::Full => "the broker is holding as many peers as it will".to_owned(),
            Self::TooManyFromYou => {
                "too many connections to the broker from this address".to_owned()
            }
        }
    }
}

impl Meet {
    /// Make a signed request.
    pub fn new(
        identity: &Identity,
        with: &[[u8; KEY_LEN]],
        candidates: Vec<SocketAddr>,
        broker: [u8; KEY_LEN],
        at: u64,
    ) -> Self {
        let mut meet = Self {
            version: RENDEZVOUS_VERSION,
            me: identity.public_key(),
            with: with.iter().copied().map(KeyBytes).collect(),
            candidates,
            broker,
            at,
            signature: [0u8; 64],
        };
        meet.signature = identity.sign(&meet.signed_part());
        meet
    }

    /// Exactly what the signature covers.
    ///
    /// Built rather than serialised: the signature must not depend on how CBOR
    /// happened to encode a map, and a hand-built byte string is one anybody
    /// can reimplement from the source.
    #[must_use]
    pub fn signed_part(&self) -> Vec<u8> {
        let mut out =
            Vec::with_capacity(64 + self.with.len() * KEY_LEN + self.candidates.len() * 24);
        out.extend_from_slice(b"esper-rendezvous-meet-v1");
        out.extend_from_slice(&self.version.to_be_bytes());
        out.extend_from_slice(&self.me);
        out.extend_from_slice(&self.broker);
        out.extend_from_slice(&self.at.to_be_bytes());
        // Lengths before contents, so two different lists cannot produce the
        // same bytes.
        out.extend_from_slice(&(self.with.len() as u64).to_be_bytes());
        for KeyBytes(k) in &self.with {
            out.extend_from_slice(k);
        }
        out.extend_from_slice(&(self.candidates.len() as u64).to_be_bytes());
        for c in &self.candidates {
            let text = c.to_string();
            out.extend_from_slice(&(text.len() as u64).to_be_bytes());
            out.extend_from_slice(text.as_bytes());
        }
        out
    }

    /// Whether this request is within its bounds.
    ///
    /// Checked before anything reaches cryptography or a table: a peer here is
    /// unauthenticated by definition.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        !self.with.is_empty()
            && self.with.len() <= MAX_WITH
            && !self.candidates.is_empty()
            && self.candidates.len() <= MAX_CANDIDATES
            // Naming yourself is not a meeting.
            && !self.with.contains(&KeyBytes(self.me))
            // A key that is not usable cannot be verified against, and a
            // request signed by nobody is not a request.
            && Identity::public_key_is_usable(&self.me)
    }

    /// Whether the signature verifies against the key the request presents.
    #[must_use]
    pub fn is_signed(&self) -> bool {
        Identity::verify(&self.me, &self.signed_part(), &self.signature)
    }

    /// Whether this peer will meet that one.
    #[must_use]
    pub fn will_meet(&self, key: &[u8; KEY_LEN]) -> bool {
        self.with.contains(&KeyBytes(*key))
    }

    /// Whether two requests name each other.
    #[must_use]
    pub fn mutual_with(&self, other: &Self) -> bool {
        self.will_meet(&other.me) && other.will_meet(&self.me)
    }

    /// What to send the other side about this peer.
    ///
    /// `seen_at` is the broker's own observation of where this peer connected
    /// from, which is the only part of an introduction the peer did not say
    /// itself.
    #[must_use]
    pub fn introduction(&self, seen_at: Option<SocketAddr>) -> Introduction {
        Introduction {
            peer: self.me,
            candidates: self.candidates.clone(),
            seen_at,
        }
    }
}

/// A fixed-size key as bytes rather than as a sequence of numbers.
mod key_bytes {
    use esper_crypto::identity::KEY_LEN;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8; KEY_LEN], s: S) -> Result<S::Ok, S::Error> {
        serde_bytes::Bytes::new(v).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; KEY_LEN], D::Error> {
        let bytes = serde_bytes::ByteBuf::deserialize(d)?;
        bytes
            .into_vec()
            .try_into()
            .map_err(|_| serde::de::Error::custom("a device key is 32 bytes"))
    }
}

/// The same, for a signature.
mod sig_bytes {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8; 64], s: S) -> Result<S::Ok, S::Error> {
        serde_bytes::Bytes::new(v).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 64], D::Error> {
        let bytes = serde_bytes::ByteBuf::deserialize(d)?;
        bytes
            .into_vec()
            .try_into()
            .map_err(|_| serde::de::Error::custom("a signature is 64 bytes"))
    }
}
