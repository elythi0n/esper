//! Running pairing over a live connection.
//!
//! The cryptography lives in `esper-crypto`; this drives it over a control
//! stream and supplies the one thing only the transport knows — the **channel
//! binding**.
//!
//! # Channel binding
//!
//! The confirmation MAC covers the TLS exporter secret for this specific
//! connection, obtained from [`quinn::Connection::export_keying_material`]. It
//! is what stops a completed pairing being relayed: a man in the middle holds a
//! different exporter on each of its two connections, so a MAC computed under
//! one cannot be forwarded to the other, even with the correct code.
//!
//! Without it, an attacker could sit between two legitimate peers, let them
//! pair through it, and end up holding a session with each.
//!
//! # Message order
//!
//! ```text
//!   controller                       host
//!       │──────── PairSpake ──────────▶│
//!       │◀─────── PairSpake ───────────│
//!       │──────── PairConfirm ────────▶│   controller commits first
//!       │◀─────── PairResult ──────────│   host is the authority on attempts
//! ```
//!
//! The controller confirms before the host. The code belongs to the host, so
//! the host must be the side that learns an attempt failed and counts it — and
//! confirming in the other order would tell the controller whether the code was
//! right before it had committed, which is a free oracle.

use std::sync::Arc;
use std::time::Instant;

use esper_crypto::identity::KEY_LEN;
use esper_crypto::pairing::{Exchange, Offer, OfferError, PairingError, Side};
use esper_crypto::{Identity, PairingCode};
use esper_proto::{PairConfirm, PairResponse, PairResult, PairSpake};

use crate::{Control, ControlError};

/// Label for the TLS exporter, so this binding can never be confused with any
/// other use of exported keying material on the same connection.
const EXPORTER_LABEL: &[u8] = b"esper pairing channel binding v1";

/// Bytes of channel binding to export.
const BINDING_LEN: usize = 32;

/// What can go wrong pairing over a connection.
#[derive(Debug, thiserror::Error)]
pub enum PairError {
    /// The exchange itself failed. Includes a wrong code.
    #[error(transparent)]
    Pairing(#[from] PairingError),

    /// The offer could not be used.
    #[error(transparent)]
    Offer(#[from] OfferError),

    /// The peer's message was outside its bounds.
    ///
    /// The peer is unauthenticated at this point, so bounds are checked before
    /// anything reaches cryptography.
    #[error("the peer sent a malformed pairing message")]
    Malformed,

    /// The channel binding could not be obtained.
    ///
    /// Pairing is refused rather than continued without it: an unbound pairing
    /// is relayable, which is the attack the binding exists to prevent.
    #[error("could not bind pairing to this connection, so pairing was refused")]
    NoChannelBinding,

    /// The control stream failed.
    #[error("control stream: {0}")]
    Control(String),

    /// The host reported a failure.
    #[error("the host refused the pairing: {0}")]
    Refused(String),
}

impl From<ControlError> for PairError {
    fn from(e: ControlError) -> Self {
        Self::Control(e.to_string())
    }
}

/// Export this connection's channel binding.
///
/// Fails closed. There is no "pair without binding" path, because that path is
/// exactly the relay attack.
fn channel_binding(conn: &quinn::Connection) -> Result<[u8; BINDING_LEN], PairError> {
    let mut out = [0u8; BINDING_LEN];
    conn.export_keying_material(&mut out, EXPORTER_LABEL, &[])
        .map_err(|_| PairError::NoChannelBinding)?;
    Ok(out)
}

/// Check a peer's SPAKE2 message before it reaches any cryptography.
fn vet(msg: &PairSpake) -> Result<[u8; KEY_LEN], PairError> {
    if !msg.is_well_formed() {
        return Err(PairError::Malformed);
    }
    // The key is a claim; `Exchange::confirm` rejects one that cannot serve as
    // an identity. Checked here too so a bad key never reaches the PAKE.
    if !Identity::public_key_is_usable(&msg.device) {
        return Err(PairError::Pairing(PairingError::UnusableKey));
    }
    Ok(msg.device)
}

/// Run pairing as the **host**: the side showing the code.
///
/// Spends one attempt from `offer`, and reports the outcome to the controller so
/// it can distinguish a typo from a dead code without guessing.
///
/// On success the peer's key is returned. Nothing is stored — the caller owns
/// the device store, and a returned key is the proof that storing is safe.
///
/// # The caller must not drop the connection immediately
///
/// This function returns as soon as *this* side has verified the peer's proof.
/// The final message to the peer has been written and the stream finished, but
/// written is not delivered: dropping a `quinn::Connection` sends
/// CONNECTION_CLOSE, which can overtake stream data still in flight and leave
/// the peer reporting a lost connection on a pairing that succeeded.
///
/// So the caller must let the connection wind down — in normal use it does,
/// because the session continues; in a test, close it and await
/// `Endpoint::wait_idle`. Waiting here instead is not an option: both sides
/// would be waiting for each other.
pub async fn host(
    conn: &quinn::Connection,
    control: &mut Control,
    identity: &Identity,
    offer: &mut Offer,
    now: Instant,
) -> Result<[u8; KEY_LEN], PairError> {
    let binding = channel_binding(conn)?;

    // The controller speaks first. Reading before answering means the host's
    // reply is always in the position the controller is waiting on, whether it
    // continues or refuses.
    let peer: PairSpake = control.recv().await?;
    let peer_key = vet(&peer)?;

    // Spend the attempt here, not on failure: a peer that disconnects
    // mid-exchange must not get a free retry.
    let (exchange, our_spake) = match offer.begin(now, identity.public_key()) {
        Ok(v) => v,
        Err(e) => {
            // Refusing before the exchange starts. Sent as a tagged response so
            // the controller can distinguish an expired code from a wrong one
            // instead of reporting a decode failure.
            let _ = control
                .send(&PairResponse::Refused(match e {
                    OfferError::Expired => PairResult::Expired,
                    OfferError::Exhausted => PairResult::Exhausted,
                }))
                .await;
            let _ = control.finish();
            return Err(e.into());
        }
    };

    control
        .send(&PairResponse::Spake(PairSpake {
            spake: our_spake,
            device: identity.public_key(),
        }))
        .await?;

    let confirming = match exchange.confirm(&peer.spake, peer_key, &binding) {
        Ok(c) => c,
        Err(e) => {
            let _ = control
                .send(&PairResult::Failed {
                    attempts_remaining: offer.attempts_remaining(),
                })
                .await;
            return Err(e.into());
        }
    };

    // The controller committed first; the host verifies first.
    // Capture our own MAC before `verify_peer` consumes the state.
    let host_mac = *confirming.our_mac();

    let their: PairConfirm = control.recv().await?;
    let paired = match confirming.verify_peer(&their.mac) {
        Ok(p) => p,
        Err(e) => {
            let _ = control
                .send(&PairResult::Failed {
                    attempts_remaining: offer.attempts_remaining(),
                })
                .await;
            return Err(e.into());
        }
    };

    // Only now does the host reveal the code was right — and it reveals it by
    // proving it, not by asserting it. `our_mac` is over a transcript covering
    // this connection's channel binding, so a man in the middle cannot produce
    // it even after watching the whole exchange.
    control.send(&PairResult::Ok { mac: host_mac }).await?;

    // Finish the send side so the peer sees a clean end of stream.
    //
    // Deliberately **not** waiting for an acknowledgement. The first attempt at
    // this did wait, and it created a who-hangs-up-last problem: whichever side
    // returns first drops the connection, and the other reports
    // "connection lost" on a pairing that in fact succeeded. Making both sides
    // wait for each other has no termination.
    //
    // No acknowledgement is needed, because neither side is waiting to learn
    // anything: the host has verified the controller's MAC and the controller
    // has verified the host's. Both already hold proof. What remains is a
    // transport concern, and it belongs to the caller — see the note on this
    // function.
    control.finish()?;
    Ok(*paired.peer_public_key())
}

/// Run pairing as the **controller**: the side whose user typed the code.
///
/// Carries the same caller obligation as [`host`]: do not drop the connection
/// the instant this returns.
pub async fn controller(
    conn: &quinn::Connection,
    control: &mut Control,
    identity: &Identity,
    code: &PairingCode,
) -> Result<[u8; KEY_LEN], PairError> {
    let binding = channel_binding(conn)?;

    let (exchange, our_spake) = Exchange::start(code, Side::Controller, identity.public_key());
    control
        .send(&PairSpake {
            spake: our_spake,
            device: identity.public_key(),
        })
        .await?;

    let peer = match control.recv::<PairResponse>().await? {
        PairResponse::Spake(s) => s,
        PairResponse::Refused(r) => return Err(PairError::Refused(describe(&r))),
    };
    let peer_key = vet(&peer)?;

    let confirming = exchange.confirm(&peer.spake, peer_key, &binding)?;

    // Commit first, then learn the outcome. This ordering is the point: the
    // host owns the code and must be the side that counts a failed attempt.
    control
        .send(&PairConfirm {
            mac: *confirming.our_mac(),
        })
        .await?;

    let host_mac = match control.recv::<PairResult>().await? {
        PairResult::Ok { mac } => mac,
        other => return Err(PairError::Refused(describe(&other))),
    };

    // **The host saying `Ok` proves nothing.** A man in the middle can send
    // `Ok` too. Only the host's MAC — over a transcript covering this
    // connection's channel binding — proves it knew the code, and until it
    // verifies, nothing may be stored.
    let paired = confirming.verify_peer(&host_mac)?;

    // Finish our side so the host sees a clean end of stream. Same caller
    // obligation as on the host: see the note on `host`.
    control.finish()?;

    Ok(*paired.peer_public_key())
}

/// Turn a host's verdict into something a person can act on.
///
/// One function so the wording exists once. A failure and a dead code call for
/// different actions — retype, versus ask for a new code — and a user told
/// "pairing failed" for both will retype into a code that can no longer work.
fn describe(r: &PairResult) -> String {
    match r {
        // Only reachable if a peer sends `Ok` where a refusal belongs.
        PairResult::Ok { .. } => "the host reported success out of sequence".to_owned(),
        PairResult::Failed { attempts_remaining } => format!(
            "the code did not match, or something is interfering with the connection \
             ({attempts_remaining} attempt(s) left before the host must show a new code)"
        ),
        PairResult::Expired => "that code has expired — ask for a new one".to_owned(),
        PairResult::Exhausted => {
            "that code has been tried too many times and is now dead — ask for a new one".to_owned()
        }
    }
}

/// An authority that accepts any well-formed key, for pairing only.
///
/// Pairing is first contact: neither side has the other's key yet, so the TLS
/// layer cannot authenticate anyone. The code does that job, inside the PAKE.
///
/// **This authority must never be used for a session.** The name is
/// deliberately alarming, and there are exactly two places it belongs:
///
/// 1. **A host in pairing mode**, with a live offer, for the sixty seconds
///    that offer lasts — `esper-engine`'s `pairing` and `invite_cmd`.
/// 2. **A broker**, in `esper-rendezvous`, where accepting a key nobody has
///    seen before is the whole job: it introduces two strangers and stores
///    nothing, and it never opens a session with either of them.
///
/// The second was left out of this list until somebody went looking, which is
/// the failure this kind of sentence invites — an auditor reading "confined to
/// a host in pairing mode" would not think to check a crate it does not name,
/// and somebody adding a third use could point at the broker as precedent.
/// `scripts/check-one-place.py` holds the list now.
#[derive(Debug, Clone, Copy, Default)]
pub struct AnyKeyForPairingOnly;

impl crate::PeerAuthority for AnyKeyForPairingOnly {
    fn is_authorized(&self, public_key: &[u8; KEY_LEN]) -> bool {
        // Still refuse a key that could never authenticate anything: a
        // small-order point would pair and then be useless.
        Identity::public_key_is_usable(public_key)
    }

    fn describe(&self) -> &'static str {
        "any key — PAIRING ONLY, never a session"
    }
}

/// Convenience: an `Arc`'d pairing authority.
#[must_use]
pub fn pairing_authority() -> Arc<dyn crate::PeerAuthority> {
    Arc::new(AnyKeyForPairingOnly)
}
