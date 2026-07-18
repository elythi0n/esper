//! Pairing: establishing mutual trust from a short code read off a screen.
//!
//! # What pairing has to achieve
//!
//! Two devices that have never met must end up holding each other's public
//! keys, with confidence that no third party substituted its own. The only
//! thing carried out of band is a short code a person reads and types.
//!
//! # Why a PAKE, and not something simpler
//!
//! It is tempting to skip the PAKE: both sides already share a TLS channel with
//! a per-connection exporter secret, so each could simply MAC the exporter under
//! the code. That is broken. A man in the middle is a *party* to its own TLS
//! connection and therefore knows that exporter, so it can brute-force a 40-bit
//! code offline from the captured MAC — measured at about 110 seconds on a
//! commodity GPU.
//!
//! A PAKE is the construction whose transcript reveals nothing that permits an
//! offline search. The code then only has to survive the online guesses the
//! protocol allows: three, within sixty seconds.
//!
//! # The step SPAKE2 does not give you
//!
//! `spake2::finish()` returns a key **whether or not the codes matched**. If
//! they differed, the two sides simply hold different keys and nothing reports
//! it. Left there, that means:
//!
//! - a wrong code silently "succeeds" and stores keys that can never talk;
//! - a wrong code is indistinguishable from a man in the middle;
//! - **the three-attempt limit cannot exist**, because no side ever learns that
//!   an attempt failed.
//!
//! So this module adds explicit key confirmation. Each side proves it derived
//! the same key by MACing a transcript that covers:
//!
//! | Covered | Prevents |
//! |---|---|
//! | The channel binding (TLS exporter) | Relaying a completed pairing into a different connection |
//! | Both public keys | Substituting a key after the exchange |
//! | Both SPAKE2 messages | Tampering with the exchange itself |
//! | A fixed domain string | Confusion with any other use of the same key |
//!
//! **The controller confirms first.** The code belongs to the host, so the host
//! must be the side that learns an attempt failed and counts it. If the host
//! confirmed first it would reveal whether the code was right before the
//! controller had committed, handing an attacker a free oracle.
//!
//! # The type sequence is the protocol
//!
//! [`Offer`] → [`Exchange`] → [`Confirming`] → [`Paired`]. Each step consumes
//! the previous one, so the steps cannot be skipped, reordered, or repeated: a
//! `Confirming` can only exist if a SPAKE2 exchange completed, and a [`Paired`]
//! only if the peer's MAC verified.

use core::fmt;
use std::time::{Duration, Instant};

use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use spake2::{Ed25519Group, Identity, Password, Spake2};
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::code::PairingCode;
use crate::identity::KEY_LEN;

/// How long a pairing code is valid.
///
/// Short because the whole security argument rests on it. A 40-bit code behind
/// three guesses in sixty seconds is safe; the same code left live for an hour
/// is a different proposition.
pub const PAIRING_TTL: Duration = Duration::from_secs(60);

/// How many failed attempts a code survives.
///
/// Three, then the code is dead and the host must show a new one. Combined with
/// the PAKE's one-guess-per-attempt property, an attacker's chance of guessing
/// a code is at most 3 in 24^8 — about one in ten billion.
pub const MAX_ATTEMPTS: u8 = 3;

/// Length of a confirmation MAC.
pub const MAC_LEN: usize = 32;

/// Domain separator. Every MAC and derived key in this module is bound to it,
/// so none can be confused with any other use of the same key material.
const DOMAIN: &[u8] = b"esper-pairing-v1";

/// The SPAKE2 identity string. Symmetric mode takes one identity, shared by
/// both sides — it is a protocol label, not a device name.
const SPAKE_IDENTITY: &[u8] = b"esper-pairing-v1-symmetric";

/// Which side of a pairing exchange this is.
///
/// Determines two things and nothing else: who opens the stream, and the
/// canonical order of the transcript. Both sides must build the transcript
/// identically, so it is ordered by role rather than by who happened to send
/// first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The machine showing the code.
    Host,
    /// The machine whose user typed the code.
    Controller,
}

impl Side {
    /// The other side.
    #[must_use]
    pub const fn peer(self) -> Self {
        match self {
            Self::Host => Self::Controller,
            Self::Controller => Self::Host,
        }
    }
}

/// Why an offer would not start an exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum OfferError {
    /// The code's sixty seconds elapsed.
    #[error("this pairing code has expired; the host must show a new one")]
    Expired,

    /// Three attempts were used.
    #[error(
        "this pairing code has been used {MAX_ATTEMPTS} times without success and is now \
         dead; the host must show a new one"
    )]
    Exhausted,
}

/// Why a pairing exchange failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PairingError {
    /// The peer's SPAKE2 message was malformed.
    #[error("the peer's pairing message was malformed")]
    BadMessage,

    /// The peer could not prove it derived the same key.
    ///
    /// Overwhelmingly means the typed code was wrong. It could also mean a man
    /// in the middle, and the two are **deliberately not distinguished**: there
    /// is no way to tell them apart, and a message that guessed would either
    /// alarm people over typos or reassure them during an attack.
    #[error(
        "the peer could not prove it knows the pairing code — either the code was typed \
         wrong, or something is interfering with the connection"
    )]
    ConfirmationFailed,

    /// The peer offered a key that cannot serve as an identity.
    #[error("the peer offered a key that cannot be used as an identity")]
    UnusableKey,

    /// A key derivation step failed.
    ///
    /// Should be unreachable — HKDF and HMAC here are given fixed, valid sizes.
    /// It exists so the confirmation code has no reason to invent a fallback
    /// MAC, which would be far more dangerous than an error.
    #[error("internal key derivation failure")]
    Internal,
}

/// A live pairing code on the host, with its expiry and attempt budget.
///
/// Held only in memory. **Pairing state is never written to disk before it
/// completes**: a code persisted across a restart is a code whose sixty seconds
/// no longer mean anything.
pub struct Offer {
    code: PairingCode,
    created_at: Instant,
    attempts_used: u8,
    /// Latched once expiry has been observed.
    ///
    /// **Deadness must be monotonic.** Without this latch, `remaining` is
    /// recomputed from `created_at` on every call, so a `now` earlier than the
    /// last one makes an expired offer live again — and `now` is supplied by
    /// the caller, not read from a clock this type controls.
    ///
    /// `Instant` is monotonic by platform guarantee, so this is not reachable
    /// through ordinary use. It is reachable through a caller that keeps a
    /// stale `Instant`, or a test, or a future caller that derives `now` from
    /// something less well behaved. Found by the state-machine property test on
    /// seed 77, which is the kind of ordering nobody writes by hand.
    expired: bool,
}

impl fmt::Debug for Offer {
    /// Never prints the code. It belongs on the host's screen and nowhere else.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Offer")
            .field("code", &"<redacted>")
            .field("attempts_used", &self.attempts_used)
            .finish()
    }
}

impl Offer {
    /// Generate a fresh code and start its clock.
    #[must_use]
    pub fn new() -> Self {
        Self {
            code: PairingCode::generate(),
            created_at: Instant::now(),
            attempts_used: 0,
            expired: false,
        }
    }

    /// Build an offer with an explicit start time, for tests.
    #[must_use]
    pub fn with_created_at(code: PairingCode, created_at: Instant) -> Self {
        Self {
            code,
            created_at,
            attempts_used: 0,
            expired: false,
        }
    }

    /// The code, to display on the host's screen.
    ///
    /// The only accessor. There is deliberately no way to log it, format it
    /// into an error, or serialize it.
    #[must_use]
    pub const fn code(&self) -> &PairingCode {
        &self.code
    }

    /// How long remains, or `None` if it has expired.
    ///
    /// Returns `None` permanently once expiry has been observed, regardless of
    /// what `now` says afterwards. See the note on [`Offer::expired`].
    #[must_use]
    pub fn remaining(&self, now: Instant) -> Option<Duration> {
        if self.expired {
            return None;
        }
        PAIRING_TTL.checked_sub(now.saturating_duration_since(self.created_at))
    }

    /// Attempts left before the code dies.
    #[must_use]
    pub const fn attempts_remaining(&self) -> u8 {
        MAX_ATTEMPTS.saturating_sub(self.attempts_used)
    }

    /// Whether this offer can still be used.
    #[must_use]
    pub fn is_live(&self, now: Instant) -> bool {
        self.remaining(now).is_some() && self.attempts_remaining() > 0
    }

    /// Begin an exchange, **spending one attempt**.
    ///
    /// The attempt is spent here rather than on failure, deliberately: an
    /// attacker who disconnects mid-exchange must not get a free retry.
    pub fn begin(
        &mut self,
        now: Instant,
        our_public_key: [u8; KEY_LEN],
    ) -> Result<(Exchange, Vec<u8>), OfferError> {
        if self.remaining(now).is_none() {
            // Latch it, so a later call with an earlier `now` cannot revive it.
            self.expired = true;
            return Err(OfferError::Expired);
        }
        if self.attempts_remaining() == 0 {
            return Err(OfferError::Exhausted);
        }
        self.attempts_used = self.attempts_used.saturating_add(1);
        Ok(Exchange::start(&self.code, Side::Host, our_public_key))
    }
}

impl Default for Offer {
    fn default() -> Self {
        Self::new()
    }
}

/// A SPAKE2 exchange in flight.
///
/// Created by [`Offer::begin`] on the host, or [`Exchange::start`] on the
/// controller once a code has been typed.
pub struct Exchange {
    spake: Spake2<Ed25519Group>,
    side: Side,
    our_message: Vec<u8>,
    our_key: [u8; KEY_LEN],
}

impl fmt::Debug for Exchange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Exchange")
            .field("side", &self.side)
            .finish()
    }
}

impl Exchange {
    /// Start an exchange from a code.
    ///
    /// Returns the exchange and the SPAKE2 message to send. Symmetric mode is
    /// used because pairing has no client and no server: two devices are
    /// establishing mutual knowledge, and the A/B modes would require deciding
    /// roles in advance for no benefit.
    #[must_use]
    pub fn start(code: &PairingCode, side: Side, our_public_key: [u8; KEY_LEN]) -> (Self, Vec<u8>) {
        let (spake, message) = Spake2::<Ed25519Group>::start_symmetric(
            &Password::new(code.as_bytes()),
            &Identity::new(SPAKE_IDENTITY),
        );
        (
            Self {
                spake,
                side,
                our_message: message.clone(),
                our_key: our_public_key,
            },
            message,
        )
    }

    /// Which side this is.
    #[must_use]
    pub const fn side(&self) -> Side {
        self.side
    }

    /// Complete SPAKE2 and produce this side's confirmation MAC.
    ///
    /// `channel_binding` must be the TLS exporter secret for the connection the
    /// exchange is happening on. It is what stops a completed pairing being
    /// relayed into a different connection: a man in the middle holds a
    /// different exporter on each of its two connections, so a MAC computed
    /// under one cannot be forwarded to the other.
    pub fn confirm(
        self,
        peer_message: &[u8],
        peer_public_key: [u8; KEY_LEN],
        channel_binding: &[u8],
    ) -> Result<Confirming, PairingError> {
        if !crate::Identity::public_key_is_usable(&peer_public_key) {
            return Err(PairingError::UnusableKey);
        }

        // SPAKE2 yields a key even when the codes differed. That is precisely
        // why the confirmation below exists.
        let mut shared = self
            .spake
            .finish(peer_message)
            .map_err(|_| PairingError::BadMessage)?;

        let transcript = transcript(
            self.side,
            &self.our_message,
            peer_message,
            &self.our_key,
            &peer_public_key,
            channel_binding,
        );

        let ours = mac_for(&shared, self.side, &transcript);
        let theirs = mac_for(&shared, self.side.peer(), &transcript);
        shared.zeroize();
        let (ours, theirs) = (ours?, theirs?);

        Ok(Confirming {
            side: self.side,
            our_mac: ours,
            expected_peer_mac: theirs,
            peer_public_key,
        })
    }
}

/// SPAKE2 has completed; each side must now prove it derived the same key.
pub struct Confirming {
    side: Side,
    our_mac: [u8; MAC_LEN],
    expected_peer_mac: [u8; MAC_LEN],
    peer_public_key: [u8; KEY_LEN],
}

impl fmt::Debug for Confirming {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Confirming")
            .field("side", &self.side)
            .finish()
    }
}

impl Confirming {
    /// This side's confirmation MAC, to send to the peer.
    ///
    /// **Ordering matters.** The controller sends first and the host verifies
    /// first, so the host — which owns the code — is the side that learns an
    /// attempt failed and can count it. Reversing it would tell the controller
    /// whether the code was right before it had committed to anything, which is
    /// a free oracle for an attacker.
    #[must_use]
    pub const fn our_mac(&self) -> &[u8; MAC_LEN] {
        &self.our_mac
    }

    /// Whether this side sends its MAC before verifying the peer's.
    #[must_use]
    pub const fn sends_first(&self) -> bool {
        matches!(self.side, Side::Controller)
    }

    /// Verify the peer's MAC and complete pairing.
    ///
    /// Constant-time comparison: a MAC comparison that returns early leaks how
    /// many bytes matched, which turns one guess into a byte-at-a-time search.
    pub fn verify_peer(self, peer_mac: &[u8]) -> Result<Paired, PairingError> {
        if peer_mac.len() != MAC_LEN {
            return Err(PairingError::ConfirmationFailed);
        }
        let ok: bool = self.expected_peer_mac.ct_eq(peer_mac).into();
        if !ok {
            return Err(PairingError::ConfirmationFailed);
        }
        Ok(Paired {
            peer_public_key: self.peer_public_key,
        })
    }
}

/// Pairing succeeded. The peer's key may now be stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Paired {
    peer_public_key: [u8; KEY_LEN],
}

impl Paired {
    /// The peer's public key, proven to belong to whoever knew the code.
    #[must_use]
    pub const fn peer_public_key(&self) -> &[u8; KEY_LEN] {
        &self.peer_public_key
    }
}

/// Build the transcript both sides MAC over.
///
/// Ordered by **role**, not by who sent first, so both sides produce identical
/// bytes. Every field is length-prefixed: without that, a message ending in the
/// bytes that begin the next field could be shifted between them and produce
/// the same transcript from different inputs.
fn transcript(
    side: Side,
    our_message: &[u8],
    peer_message: &[u8],
    our_key: &[u8; KEY_LEN],
    peer_key: &[u8; KEY_LEN],
    channel_binding: &[u8],
) -> Vec<u8> {
    let (controller_msg, host_msg, controller_key, host_key) = match side {
        Side::Controller => (our_message, peer_message, our_key, peer_key),
        Side::Host => (peer_message, our_message, peer_key, our_key),
    };

    let mut t = Vec::with_capacity(
        DOMAIN.len()
            + controller_msg.len()
            + host_msg.len()
            + 2 * KEY_LEN
            + channel_binding.len()
            + 40,
    );
    for field in [
        DOMAIN,
        controller_msg,
        host_msg,
        controller_key.as_slice(),
        host_key.as_slice(),
        channel_binding,
    ] {
        // 64-bit length prefix, so a field cannot be confused with its neighbour.
        t.extend_from_slice(&u64::try_from(field.len()).unwrap_or(u64::MAX).to_be_bytes());
        t.extend_from_slice(field);
    }
    t
}

/// Derive one side's confirmation MAC from the shared SPAKE2 key.
///
/// Each side gets a *different* MAC key, derived under a different label, so
/// one side's confirmation cannot be replayed back at it as though it were the
/// other's.
///
/// Fallible, and deliberately so. Neither step here can fail in practice —
/// HKDF-Expand refuses only absurd output lengths, and HMAC-SHA-256 accepts any
/// key length — but the alternative to propagating is a fallback, and a
/// fallback in a confirmation step means computing a MAC under some *other*
/// key and reporting success. A confirmation that silently uses the wrong key
/// either fails mysteriously or, far worse, matches something an attacker can
/// also produce. There is no safe default value for this function to return.
fn mac_for(shared: &[u8], side: Side, transcript: &[u8]) -> Result<[u8; MAC_LEN], PairingError> {
    let label: &[u8] = match side {
        Side::Host => b"confirm-host",
        Side::Controller => b"confirm-controller",
    };

    let hk = Hkdf::<Sha256>::new(Some(DOMAIN), shared);
    let mut key = Key([0u8; 32]);
    hk.expand(label, &mut key.0)
        .map_err(|_| PairingError::Internal)?;

    let mut mac =
        <Hmac<Sha256> as Mac>::new_from_slice(&key.0).map_err(|_| PairingError::Internal)?;
    mac.update(transcript);

    let out = mac.finalize().into_bytes();
    let mut result = [0u8; MAC_LEN];
    for (slot, byte) in result.iter_mut().zip(out.iter()) {
        *slot = *byte;
    }
    Ok(result)
}

/// A derived key that wipes itself. The MAC key lives for one HMAC and should
/// not outlive it in memory.
#[derive(ZeroizeOnDrop)]
struct Key([u8; 32]);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Identity;

    fn key(n: u8) -> [u8; KEY_LEN] {
        ed25519_dalek::SigningKey::from_bytes(&[n; KEY_LEN])
            .verifying_key()
            .to_bytes()
    }

    const BINDING: &[u8] = b"tls-exporter-secret-for-this-connection";

    /// Run a full exchange between two sides and report both outcomes.
    fn run(
        host_code: &PairingCode,
        controller_code: &PairingCode,
        host_binding: &[u8],
        controller_binding: &[u8],
    ) -> (Result<Paired, PairingError>, Result<Paired, PairingError>) {
        let hk = key(1);
        let ck = key(2);

        let (h, h_msg) = Exchange::start(host_code, Side::Host, hk);
        let (c, c_msg) = Exchange::start(controller_code, Side::Controller, ck);

        let hc = h.confirm(&c_msg, ck, host_binding);
        let cc = c.confirm(&h_msg, hk, controller_binding);

        match (hc, cc) {
            (Ok(hc), Ok(cc)) => {
                // The controller sends first; the host verifies first.
                assert!(cc.sends_first(), "the controller must send its MAC first");
                assert!(!hc.sends_first(), "the host must verify first");
                let c_mac = *cc.our_mac();
                let h_mac = *hc.our_mac();
                (hc.verify_peer(&c_mac), cc.verify_peer(&h_mac))
            }
            (Err(e), _) | (_, Err(e)) => (Err(e.clone()), Err(e)),
        }
    }

    #[test]
    fn matching_codes_pair_and_learn_each_others_keys() {
        let code = PairingCode::generate();
        let (h, c) = run(&code, &code, BINDING, BINDING);
        let h = h.expect("host should pair");
        let c = c.expect("controller should pair");
        assert_eq!(
            h.peer_public_key(),
            &key(2),
            "host learned the controller's key"
        );
        assert_eq!(
            c.peer_public_key(),
            &key(1),
            "controller learned the host's key"
        );
    }

    /// The property this module exists for. `spake2::finish()` returns a key
    /// either way; only the confirmation step notices.
    #[test]
    fn a_wrong_code_is_detected_rather_than_silently_succeeding() {
        let right = PairingCode::generate();
        let wrong = loop {
            let c = PairingCode::generate();
            if c != right {
                break c;
            }
        };
        let (h, c) = run(&right, &wrong, BINDING, BINDING);
        assert_eq!(h, Err(PairingError::ConfirmationFailed));
        assert_eq!(c, Err(PairingError::ConfirmationFailed));
    }

    /// A near miss must fail exactly like a wild guess: no partial credit, and
    /// no hint in the message about how close it was.
    #[test]
    fn a_single_wrong_character_fails_completely() {
        let right = PairingCode::generate();
        let mut chars: Vec<char> = right.display().chars().filter(|c| *c != '-').collect();
        // Change one symbol to a different alphabet member.
        let at = chars.get(3).copied().unwrap_or('A');
        if let Some(slot) = chars.get_mut(3) {
            *slot = if at == 'A' { 'C' } else { 'A' };
        }
        let nearly: String = chars.into_iter().collect();
        let wrong = PairingCode::parse(&nearly).expect("still a valid code shape");
        assert_ne!(wrong, right);

        let (h, _) = run(&right, &wrong, BINDING, BINDING);
        assert_eq!(h, Err(PairingError::ConfirmationFailed));
    }

    /// Channel binding. A man in the middle holds a different TLS exporter on
    /// each of its two connections, so a MAC computed under one cannot be
    /// forwarded to the other — even with the correct code.
    #[test]
    fn a_completed_pairing_cannot_be_relayed_into_another_connection() {
        let code = PairingCode::generate();
        let (h, c) = run(
            &code,
            &code,
            b"exporter-of-connection-A",
            b"exporter-of-connection-B",
        );
        assert_eq!(
            h,
            Err(PairingError::ConfirmationFailed),
            "differing channel bindings must not confirm, even with the right code"
        );
        assert_eq!(c, Err(PairingError::ConfirmationFailed));
    }

    /// Even a one-bit difference in the binding must break confirmation.
    #[test]
    fn the_channel_binding_is_covered_bit_for_bit() {
        let code = PairingCode::generate();
        let mut altered = BINDING.to_vec();
        if let Some(b) = altered.get_mut(0) {
            *b ^= 0x01;
        }
        let (h, _) = run(&code, &code, BINDING, &altered);
        assert_eq!(h, Err(PairingError::ConfirmationFailed));
    }

    /// A peer that swaps in a different public key after the exchange must not
    /// confirm: the transcript covers both keys.
    #[test]
    fn substituting_a_public_key_breaks_confirmation() {
        let code = PairingCode::generate();
        let hk = key(1);
        let ck = key(2);
        let attacker = key(9);

        let (h, h_msg) = Exchange::start(&code, Side::Host, hk);
        let (c, c_msg) = Exchange::start(&code, Side::Controller, ck);

        // The host is told the controller's key is the attacker's.
        let hc = h
            .confirm(&c_msg, attacker, BINDING)
            .expect("spake completes");
        let cc = c.confirm(&h_msg, hk, BINDING).expect("spake completes");

        let c_mac = *cc.our_mac();
        assert_eq!(
            hc.verify_peer(&c_mac),
            Err(PairingError::ConfirmationFailed),
            "a substituted key must break the transcript"
        );
    }

    /// Each side's MAC is derived under a different label, so one side's
    /// confirmation cannot be reflected back at it as though it were the other's.
    #[test]
    fn a_confirmation_cannot_be_reflected_back_at_its_sender() {
        let code = PairingCode::generate();
        let hk = key(1);
        let ck = key(2);

        let (h, h_msg) = Exchange::start(&code, Side::Host, hk);
        let (c, c_msg) = Exchange::start(&code, Side::Controller, ck);
        let hc = h.confirm(&c_msg, ck, BINDING).expect("ok");
        let cc = c.confirm(&h_msg, hk, BINDING).expect("ok");

        let h_mac = *hc.our_mac();
        let c_mac = *cc.our_mac();
        assert_ne!(h_mac, c_mac, "the two sides must not produce the same MAC");

        // Reflect the host's own MAC back at it.
        assert_eq!(
            hc.verify_peer(&h_mac),
            Err(PairingError::ConfirmationFailed),
            "the host must not accept its own MAC as the controller's"
        );
        assert_eq!(
            cc.verify_peer(&c_mac),
            Err(PairingError::ConfirmationFailed),
            "the controller must not accept its own MAC as the host's"
        );
    }

    #[test]
    fn a_malformed_spake_message_is_refused_without_panicking() {
        let code = PairingCode::generate();
        let (h, _) = Exchange::start(&code, Side::Host, key(1));
        assert_eq!(
            h.confirm(b"not a spake2 message", key(2), BINDING).err(),
            Some(PairingError::BadMessage)
        );

        for len in [0usize, 1, 32, 33, 64, 200] {
            let (h, _) = Exchange::start(&code, Side::Host, key(1));
            let _ = h.confirm(&vec![0xAB; len], key(2), BINDING);
        }
    }

    /// A weak peer key must be refused during pairing, before it can be stored.
    #[test]
    fn a_weak_peer_key_is_refused_during_pairing() {
        let code = PairingCode::generate();
        let (_, c_msg) = Exchange::start(&code, Side::Controller, key(2));
        let (h, _) = Exchange::start(&code, Side::Host, key(1));
        assert_eq!(
            h.confirm(&c_msg, [0u8; KEY_LEN], BINDING).err(),
            Some(PairingError::UnusableKey),
            "an all-zero key is small-order and must not be pairable"
        );
    }

    #[test]
    fn a_truncated_or_overlong_mac_is_refused() {
        let code = PairingCode::generate();
        let hk = key(1);
        let ck = key(2);
        let (h, h_msg) = Exchange::start(&code, Side::Host, hk);
        let (c, _) = Exchange::start(&code, Side::Controller, ck);
        let cc = c.confirm(&h_msg, hk, BINDING).expect("ok");
        let good = *cc.our_mac();

        for bad in [
            good[..MAC_LEN - 1].to_vec(),
            good.into_iter().chain([0u8]).collect(),
            Vec::new(),
        ] {
            let (h2, _) = Exchange::start(&code, Side::Host, hk);
            let hc = h2.confirm(&_spake_msg_for(&code, ck), ck, BINDING);
            if let Ok(hc) = hc {
                assert_eq!(
                    hc.verify_peer(&bad),
                    Err(PairingError::ConfirmationFailed),
                    "a MAC of length {} must be refused",
                    bad.len()
                );
            }
        }
        drop(h);
    }

    fn _spake_msg_for(code: &PairingCode, k: [u8; KEY_LEN]) -> Vec<u8> {
        Exchange::start(code, Side::Controller, k).1
    }

    // ── the offer's lifetime and attempt budget ──────────────────────────────

    #[test]
    fn a_fresh_offer_is_live_with_the_full_budget() {
        let o = Offer::new();
        let now = Instant::now();
        assert!(o.is_live(now));
        assert_eq!(o.attempts_remaining(), MAX_ATTEMPTS);
        assert!(o.remaining(now).is_some());
    }

    #[test]
    fn an_expired_offer_refuses_to_begin() {
        let created = Instant::now();
        let mut o = Offer::with_created_at(PairingCode::generate(), created);
        let later = created + PAIRING_TTL + Duration::from_secs(1);
        assert!(!o.is_live(later));
        assert_eq!(o.remaining(later), None);
        assert_eq!(o.begin(later, key(1)).err(), Some(OfferError::Expired));
    }

    /// Deadness is monotonic. A clock that moves backwards must not revive an
    /// expired offer.
    ///
    /// Found by the state-machine property test, not written by hand: the
    /// sequence is "expire, then go back in time, then try again", which is not
    /// an ordering anyone thinks to test. `remaining` recomputed from
    /// `created_at` every call, so an earlier `now` made the offer live again —
    /// and `now` is supplied by the caller.
    #[test]
    fn an_expired_offer_is_not_revived_by_the_clock_going_backwards() {
        let created = Instant::now();
        let mut o = Offer::with_created_at(PairingCode::generate(), created);

        let after_expiry = created + PAIRING_TTL + Duration::from_secs(1);
        assert_eq!(
            o.begin(after_expiry, key(1)).err(),
            Some(OfferError::Expired)
        );

        // Now go back to when it was live. It must stay dead.
        assert!(!o.is_live(created), "an expired offer must not come back");
        assert_eq!(o.remaining(created), None, "remaining must stay None");
        assert_eq!(
            o.begin(created, key(1)).err(),
            Some(OfferError::Expired),
            "an expired offer must refuse an attempt even at an earlier instant"
        );
    }

    /// Exhaustion is monotonic too, and for the same reason: once dead, dead.
    #[test]
    fn an_exhausted_offer_is_not_revived_by_the_clock_going_backwards() {
        let created = Instant::now();
        let mut o = Offer::with_created_at(PairingCode::generate(), created);
        let later = created + Duration::from_secs(1);
        for _ in 0..MAX_ATTEMPTS {
            assert!(o.begin(later, key(1)).is_ok());
        }
        assert_eq!(o.begin(created, key(1)).err(), Some(OfferError::Exhausted));
        assert!(!o.is_live(created));
    }

    /// The boundary: valid at the last moment, dead just after.
    #[test]
    fn the_expiry_boundary_is_exact() {
        let created = Instant::now();
        let mut o = Offer::with_created_at(PairingCode::generate(), created);
        let last = created + PAIRING_TTL - Duration::from_millis(1);
        assert!(o.is_live(last), "must still be live just before expiry");
        assert!(o.begin(last, key(1)).is_ok());

        let mut o2 = Offer::with_created_at(PairingCode::generate(), created);
        let past = created + PAIRING_TTL + Duration::from_millis(1);
        assert_eq!(o2.begin(past, key(1)).err(), Some(OfferError::Expired));
    }

    #[test]
    fn three_attempts_then_the_code_is_dead() {
        let now = Instant::now();
        let mut o = Offer::with_created_at(PairingCode::generate(), now);
        for i in 0..MAX_ATTEMPTS {
            assert!(
                o.begin(now, key(1)).is_ok(),
                "attempt {i} should be allowed"
            );
            assert_eq!(o.attempts_remaining(), MAX_ATTEMPTS - i - 1);
        }
        assert!(!o.is_live(now));
        assert_eq!(o.begin(now, key(1)).err(), Some(OfferError::Exhausted));
        // And it stays dead.
        assert_eq!(o.begin(now, key(1)).err(), Some(OfferError::Exhausted));
    }

    /// The attempt is spent when the exchange *starts*, not when it fails, so
    /// an attacker who disconnects mid-exchange gets no free retry.
    #[test]
    fn an_abandoned_attempt_is_still_spent() {
        let now = Instant::now();
        let mut o = Offer::with_created_at(PairingCode::generate(), now);
        let (exchange, _msg) = o.begin(now, key(1)).expect("first attempt");
        drop(exchange); // the peer vanished
        assert_eq!(
            o.attempts_remaining(),
            MAX_ATTEMPTS - 1,
            "disconnecting must not refund the attempt"
        );
    }

    /// The code must never reach a log, an error, or a panic message.
    #[test]
    fn debug_never_reveals_the_code() {
        let o = Offer::new();
        let rendered = format!("{o:?}");
        assert!(rendered.contains("<redacted>"));
        assert!(!rendered.contains(&o.code().display()));

        let (e, _) = Exchange::start(o.code(), Side::Host, key(1));
        assert!(!format!("{e:?}").contains(&o.code().display()));
    }

    /// No error message may hint at the code, or how close a guess was.
    #[test]
    fn no_error_message_leaks_anything_about_the_code() {
        for e in [
            PairingError::BadMessage,
            PairingError::ConfirmationFailed,
            PairingError::UnusableKey,
            PairingError::Internal,
        ] {
            let msg = e.to_string();
            assert!(!msg.contains("byte"), "{msg}");
            assert!(!msg.contains("position"), "{msg}");
        }
        // And the failure message must not distinguish a typo from an attack,
        // because nothing can.
        let msg = PairingError::ConfirmationFailed.to_string();
        assert!(msg.contains("typed wrong") && msg.contains("interfering"));
    }

    #[test]
    fn side_peer_is_an_involution() {
        assert_eq!(Side::Host.peer(), Side::Controller);
        assert_eq!(Side::Controller.peer(), Side::Host);
        assert_eq!(Side::Host.peer().peer(), Side::Host);
    }

    /// The transcript must be identical on both sides, or nothing ever confirms.
    #[test]
    fn both_sides_build_the_same_transcript() {
        let hm = b"host-message".to_vec();
        let cm = b"controller-message".to_vec();
        let hk = key(1);
        let ck = key(2);

        let from_host = transcript(Side::Host, &hm, &cm, &hk, &ck, BINDING);
        let from_controller = transcript(Side::Controller, &cm, &hm, &ck, &hk, BINDING);
        assert_eq!(from_host, from_controller);
    }

    /// Length prefixes prevent a field's bytes being shifted into its
    /// neighbour to produce the same transcript from different inputs.
    #[test]
    fn the_transcript_is_unambiguous_across_field_boundaries() {
        let k1 = key(1);
        let k2 = key(2);
        let a = transcript(Side::Host, b"ab", b"c", &k1, &k2, BINDING);
        let b = transcript(Side::Host, b"a", b"bc", &k1, &k2, BINDING);
        assert_ne!(
            a, b,
            "shifting a byte between adjacent fields must change the transcript"
        );
    }

    #[test]
    fn the_offer_defaults_to_a_fresh_generated_code() {
        assert_ne!(Offer::default().code(), Offer::default().code());
    }

    /// A full exchange with an identity generated the real way, rather than
    /// from a test seed.
    #[test]
    fn pairs_real_generated_identities() {
        let host_id = Identity::generate().expect("host");
        let ctrl_id = Identity::generate().expect("controller");
        let code = PairingCode::generate();

        let (h, h_msg) = Exchange::start(&code, Side::Host, host_id.public_key());
        let (c, c_msg) = Exchange::start(&code, Side::Controller, ctrl_id.public_key());

        let hc = h
            .confirm(&c_msg, ctrl_id.public_key(), BINDING)
            .expect("host spake");
        let cc = c
            .confirm(&h_msg, host_id.public_key(), BINDING)
            .expect("controller spake");

        let c_mac = *cc.our_mac();
        let h_mac = *hc.our_mac();
        let host_paired = hc.verify_peer(&c_mac).expect("host confirms");
        let ctrl_paired = cc.verify_peer(&h_mac).expect("controller confirms");

        assert_eq!(host_paired.peer_public_key(), &ctrl_id.public_key());
        assert_eq!(ctrl_paired.peer_public_key(), &host_id.public_key());
    }
}
