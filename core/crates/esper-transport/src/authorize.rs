//! Deciding what a session may do, and taking it away again.
//!
//! Pairing establishes **identity**. It grants nothing. What a paired device may
//! actually do is a policy this machine holds and can change at any moment.
//!
//! # The request is a maximum, not a demand
//!
//! A controller says what it would like; the host grants a subset. The grant is
//! authoritative and the controller renders what it holds — a client that
//! assumes its request was honoured will offer a control cursor on a view-only
//! session and confuse the person using it.
//!
//! # Revocation has to reach live sessions
//!
//! Deleting a key from the store stops the *next* connection. It does nothing
//! about the one already open, and a revocation that leaves an attacker
//! connected is not a revocation. So live sessions are registered by peer key,
//! and revoking closes them in the same operation that forgets the key —
//! [`SessionRegistry::revoke`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use std::time::Duration;

use esper_crypto::identity::KEY_LEN;
use esper_crypto::{Device, Fingerprint};
use esper_proto::{
    Permission, Reason, SessionDeny, SessionGrant, SessionKind, SessionRequest, SessionResponse,
};

use crate::control::{Control, ControlError};

/// The verdict on a session request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Open the session with exactly these permissions.
    Grant(Vec<Permission>),
    /// Refuse, with a reason the controller can branch on and a sentence a
    /// person can act on.
    Deny(Reason, String),
}

/// Decides whether a session may open, and with what.
///
/// A trait because the answer depends on things the transport must not know
/// about: whether a human is present to approve, what a schedule says, whether
/// the screen is locked. The transport asks; the control plane answers.
pub trait Policy: Send + Sync + 'static {
    /// Decide on a request from a paired device.
    ///
    /// `device` is the stored record, so its permissions are the ceiling. The
    /// policy may grant less — it must never grant more, and
    /// [`decide_and_clamp`] enforces that regardless of what a policy returns.
    fn decide(&self, device: &Device, request: &SessionRequest) -> Verdict;
}

/// Grants the intersection of what was requested and what the device holds.
///
/// The honest default for an unattended host: no prompt, no widening, nothing
/// granted that was not already stored against that device.
#[derive(Debug, Clone, Copy, Default)]
pub struct GrantStoredPermissions;

impl Policy for GrantStoredPermissions {
    fn decide(&self, device: &Device, request: &SessionRequest) -> Verdict {
        let granted: Vec<Permission> = request
            .want
            .iter()
            .copied()
            .filter(|p| device.may(*p))
            .collect();

        if granted.is_empty() {
            return Verdict::Deny(
                Reason::PERMISSION_DENIED,
                "none of the requested permissions are granted to this device".to_owned(),
            );
        }
        Verdict::Grant(granted)
    }
}

/// Refuses everything. The correct policy for a host with sessions disabled,
/// and the safe default if a caller forgets to supply one.
#[derive(Debug, Clone, Copy, Default)]
pub struct RefuseAll;

impl Policy for RefuseAll {
    fn decide(&self, _device: &Device, _request: &SessionRequest) -> Verdict {
        Verdict::Deny(
            Reason::HOST_DISABLED,
            "this machine is not accepting sessions".to_owned(),
        )
    }
}

/// Apply a policy, then **clamp its answer to the device's stored permissions**.
///
/// The clamp is not redundant with the policy. A policy is application code —
/// it may be written by someone extending Esper, it may have a bug, it may
/// return a permission it never meant to. The stored record is the ceiling, and
/// enforcing that here means no policy can raise it even by mistake.
///
/// A widening is silently clamped rather than refused, and deliberately: the
/// session should still open with what the device legitimately holds. But it is
/// reported, because a policy that tried to widen is a bug someone should see.
pub fn decide_and_clamp(
    policy: &dyn Policy,
    device: &Device,
    request: &SessionRequest,
) -> (Verdict, Vec<Permission>) {
    // **The kind first, because it is the answer somebody can act on.** A
    // device asking for a shell it may not have is told which permission it
    // needs; without this it is told "none of what you asked for is granted",
    // which is true and useless.
    // Any one of them is enough: moving files is two permissions granted
    // separately, and either is a reason to open a session.
    if !request.kind.requires().iter().any(|p| device.may(*p)) {
        return (
            Verdict::Deny(
                Reason::PERMISSION_DENIED,
                format!(
                    "a {} session needs the {} permission, which this device does not hold",
                    request.kind.name(),
                    request.kind.needs_said()
                ),
            ),
            Vec::new(),
        );
    }

    let verdict = policy.decide(device, request);
    let Verdict::Grant(proposed) = verdict else {
        return (verdict, Vec::new());
    };

    let mut over_reach = Vec::new();
    let mut clamped = Vec::new();
    for p in proposed {
        if device.may(p) {
            if !clamped.contains(&p) {
                clamped.push(p);
            }
        } else {
            over_reach.push(p);
        }
    }
    clamped.sort_unstable();

    if clamped.is_empty() {
        return (
            Verdict::Deny(
                Reason::PERMISSION_DENIED,
                "nothing the policy granted is permitted for this device".to_owned(),
            ),
            over_reach,
        );
    }

    // **And again after the policy has spoken.** The check above is about the
    // device's own record; this one is about what the policy actually granted,
    // which can be less — a policy that hands back a subset without the
    // permission the kind needs would otherwise open a session that cannot do
    // the one thing it was opened for.
    if !request.kind.requires().iter().any(|p| clamped.contains(p)) {
        return (
            Verdict::Deny(
                Reason::PERMISSION_DENIED,
                format!(
                    "a {} session needs the {} permission, which this device does not hold",
                    request.kind.name(),
                    request.kind.needs_said()
                ),
            ),
            over_reach,
        );
    }
    (Verdict::Grant(clamped), over_reach)
}

/// Turn a verdict into the message that goes on the wire.
#[must_use]
pub fn response(verdict: &Verdict, session_id: [u8; 16]) -> SessionResponse {
    match verdict {
        Verdict::Grant(granted) => SessionResponse::Grant(SessionGrant {
            session_id,
            granted: granted.clone(),
        }),
        Verdict::Deny(reason, detail) => SessionResponse::Deny(SessionDeny {
            reason: *reason,
            detail: detail.clone(),
        }),
    }
}

/// How long a host waits for a controller to say what it wants.
///
/// A connection that authenticated and then said nothing is holding a capture
/// backend and a slot in the registry. Ten seconds is far longer than a round
/// trip and far shorter than the transport's idle timeout, which is the only
/// thing that would have ended it otherwise.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// What answering a session request settled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answered {
    /// The session may open, with exactly these permissions, for what the
    /// controller said it was for.
    Granted {
        grant: SessionGrant,
        /// What the controller asked to do. Already checked against the
        /// grant: a kind whose permission was not granted is a
        /// [`Answered::Denied`], never a grant the caller must re-check.
        kind: SessionKind,
    },
    /// Refused. **The peer has already been told the same reason** — that is
    /// the difference between this and a [`Verdict`], which is only a decision.
    Denied(SessionDeny),
}

/// What can go wrong agreeing on a session.
#[derive(Debug, thiserror::Error)]
pub enum AuthorizeError {
    /// The peer authenticated and then said nothing.
    #[error("the controller did not say what it wanted within {0:?}")]
    Silent(Duration),

    /// The control stream failed.
    #[error("control stream: {0}")]
    Control(#[from] ControlError),

    /// The host refused, and said why.
    ///
    /// Controller-side. The host's own refusals are an [`Answered::Denied`],
    /// because from that side refusing is a normal outcome and not an error.
    #[error("{}", .0.detail)]
    Refused(SessionDeny),
}

/// The controller's half: say what you want, and read the answer.
///
/// The grant is authoritative and **may be smaller than the request**. A
/// controller that renders what it asked for rather than what it holds will
/// offer a control cursor on a view-only session.
pub async fn request_session(
    control: &mut Control,
    request: &SessionRequest,
) -> Result<SessionGrant, AuthorizeError> {
    control.send(request).await?;
    match control.recv::<SessionResponse>().await? {
        SessionResponse::Grant(g) => Ok(g),
        SessionResponse::Deny(d) => Err(AuthorizeError::Refused(d)),
    }
}

/// The host's half: read the request, decide, and answer before doing anything.
///
/// **The answer goes out before the session starts.** A host that decided
/// silently and then began — or did not begin — sending leaves the controller
/// inferring its permissions from what shows up, which is how a client ends up
/// guessing that a session is view-only because no input took effect.
///
/// A malformed request is denied rather than dropped, for the same reason.
pub async fn answer_session(
    control: &mut Control,
    policy: &dyn Policy,
    device: &Device,
    session_id: [u8; 16],
) -> Result<Answered, AuthorizeError> {
    answer_session_within(control, policy, device, session_id, REQUEST_TIMEOUT).await
}

/// [`answer_session`], with the wait as an argument.
///
/// Separate so the wait can be *tested*: a test that proved a ten-second
/// timeout by waiting ten seconds is a test nobody runs.
pub async fn answer_session_within(
    control: &mut Control,
    policy: &dyn Policy,
    device: &Device,
    session_id: [u8; 16],
    wait: Duration,
) -> Result<Answered, AuthorizeError> {
    let request = match tokio::time::timeout(wait, control.recv::<SessionRequest>()).await {
        Ok(r) => r?,
        Err(_) => return Err(AuthorizeError::Silent(wait)),
    };

    if !request.is_well_formed() {
        let deny = SessionDeny {
            reason: Reason::MALFORMED_FRAME,
            detail: "the session request was outside its bounds".to_owned(),
        };
        control.send(&SessionResponse::Deny(deny.clone())).await?;
        return Ok(Answered::Denied(deny));
    }

    let (verdict, over_reach) = decide_and_clamp(policy, device, &request);
    if !over_reach.is_empty() {
        // A policy that tried to widen is a bug someone should see. The
        // session still opens with what the device holds.
        tracing::warn!(
            ?over_reach,
            device = %device.fingerprint().hex(),
            "the session policy granted permissions the device does not hold; clamped"
        );
    }

    let answer = response(&verdict, session_id);
    control.send(&answer).await?;
    Ok(match answer {
        SessionResponse::Grant(grant) => Answered::Granted {
            grant,
            kind: request.kind,
        },
        SessionResponse::Deny(d) => Answered::Denied(d),
    })
}

/// A live session, as the registry sees it.
struct Live {
    conn: quinn::Connection,
    session_id: [u8; 16],
}

/// Every session currently open, keyed by peer.
///
/// Exists so revocation can reach connections already established. Without it,
/// deleting a key stops the next connection and leaves the current one running
/// — which is not revocation, it is a delay.
#[derive(Clone, Default)]
pub struct SessionRegistry {
    inner: Arc<Mutex<HashMap<[u8; KEY_LEN], Vec<Live>>>>,
}

impl std::fmt::Debug for SessionRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = self.inner.lock().map(|g| g.len()).unwrap_or(0);
        f.debug_struct("SessionRegistry")
            .field("peers", &n)
            .finish()
    }
}

impl SessionRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a live session.
    ///
    /// A peer may hold several at once — several viewers of one host, or a
    /// controller with two monitors open — so sessions are stored per peer as a
    /// list. Revocation must close all of them, not the most recent.
    pub fn register(&self, peer: [u8; KEY_LEN], session_id: [u8; 16], conn: quinn::Connection) {
        if let Ok(mut g) = self.inner.lock() {
            g.entry(peer).or_default().push(Live { conn, session_id });
        }
    }

    /// Forget one session, by id. Called when it ends normally.
    pub fn finished(&self, peer: &[u8; KEY_LEN], session_id: &[u8; 16]) {
        if let Ok(mut g) = self.inner.lock()
            && let Some(list) = g.get_mut(peer)
        {
            list.retain(|l| &l.session_id != session_id);
            if list.is_empty() {
                g.remove(peer);
            }
        }
    }

    /// How many sessions a peer currently holds.
    #[must_use]
    pub fn live_for(&self, peer: &[u8; KEY_LEN]) -> usize {
        self.inner
            .lock()
            .map(|g| g.get(peer).map_or(0, Vec::len))
            .unwrap_or(0)
    }

    /// Total live sessions.
    #[must_use]
    pub fn total(&self) -> usize {
        self.inner
            .lock()
            .map(|g| g.values().map(Vec::len).sum())
            .unwrap_or(0)
    }

    /// **Close every session a peer holds, immediately.**
    ///
    /// Called in the same operation that removes the key from the store, so
    /// there is no window in which a revoked device is still connected.
    /// Returns how many sessions were closed, so the caller can tell the user
    /// what actually happened rather than assuming.
    ///
    /// The close carries [`Reason::REVOKED`], so the far side reports "this
    /// device has been revoked" rather than a bare disconnection the user will
    /// try to reconnect through.
    pub fn revoke(&self, peer: &[u8; KEY_LEN]) -> usize {
        let Ok(mut g) = self.inner.lock() else {
            return 0;
        };
        let Some(list) = g.remove(peer) else {
            return 0;
        };
        let n = list.len();
        for live in list {
            live.conn.close(
                u32::from(Reason::REVOKED.0).into(),
                Reason::REVOKED.to_string().as_bytes(),
            );
        }
        n
    }

    /// Close every session, for shutdown.
    pub fn close_all(&self, reason: Reason) -> usize {
        let Ok(mut g) = self.inner.lock() else {
            return 0;
        };
        let mut n = 0usize;
        for (_, list) in g.drain() {
            for live in list {
                live.conn
                    .close(u32::from(reason.0).into(), reason.to_string().as_bytes());
                n = n.saturating_add(1);
            }
        }
        n
    }

    /// The fingerprints of every peer with a live session, for display.
    #[must_use]
    pub fn peers(&self) -> Vec<Fingerprint> {
        self.inner
            .lock()
            .map(|g| g.keys().map(|k| Fingerprint::of(k)).collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u8) -> [u8; KEY_LEN] {
        ed25519_dalek::SigningKey::from_bytes(&[n; KEY_LEN])
            .verifying_key()
            .to_bytes()
    }

    fn device_with(perms: &[Permission]) -> Device {
        let mut d = Device::new(key(1), "laptop".to_owned(), 0);
        d.permissions = perms.to_vec();
        d
    }

    fn request(want: &[Permission]) -> SessionRequest {
        SessionRequest {
            kind: esper_proto::SessionKind::Screen,
            want: want.to_vec(),
            codecs: vec!["h264".to_owned()],
            reason: "helping with email".to_owned(),
        }
    }

    #[test]
    fn the_default_policy_grants_the_intersection() {
        let d = device_with(&[Permission::View, Permission::Control]);
        let r = request(&[Permission::View, Permission::Control, Permission::Audio]);
        match GrantStoredPermissions.decide(&d, &r) {
            Verdict::Grant(g) => {
                assert!(g.contains(&Permission::View));
                assert!(g.contains(&Permission::Control));
                assert!(
                    !g.contains(&Permission::Audio),
                    "audio was requested but not stored, so must not be granted"
                );
            }
            other => panic!("expected a grant, got {other:?}"),
        }
    }

    #[test]
    fn requesting_only_things_you_do_not_hold_is_denied() {
        let d = device_with(&[Permission::View]);
        let r = request(&[Permission::Unattended, Permission::FilesIn]);
        assert!(matches!(
            GrantStoredPermissions.decide(&d, &r),
            Verdict::Deny(Reason::PERMISSION_DENIED, _)
        ));
    }

    fn asking_for(kind: SessionKind, want: &[Permission]) -> SessionRequest {
        SessionRequest {
            kind,
            want: want.to_vec(),
            codecs: Vec::new(),
            reason: "a session".to_owned(),
        }
    }

    /// A session of a kind the device may not open is refused **even when
    /// something else it asked for would have been granted**.
    ///
    /// This is the case the policy alone does not catch. Asking for a forward
    /// while holding only `view` and requesting both would otherwise produce a
    /// grant — the intersection is not empty — and the session would open as a
    /// forward for a device that holds no such permission.
    #[test]
    fn a_kind_the_device_may_not_open_is_refused_even_alongside_one_it_may() {
        let d = device_with(&[Permission::View]);
        let r = asking_for(
            SessionKind::Forward,
            &[Permission::View, Permission::Forward],
        );
        let (verdict, _) = decide_and_clamp(&GrantStoredPermissions, &d, &r);
        match verdict {
            Verdict::Deny(reason, detail) => {
                assert_eq!(reason, Reason::PERMISSION_DENIED);
                // Which permission, by name: "none of what you asked for" is
                // true and leaves somebody guessing.
                assert!(detail.contains("forward"), "{detail}");
            }
            other => panic!("a forward session opened for a device without it: {other:?}"),
        }

        // The same for a shell.
        let r = asking_for(
            SessionKind::Terminal,
            &[Permission::View, Permission::Terminal],
        );
        assert!(matches!(
            decide_and_clamp(&GrantStoredPermissions, &d, &r).0,
            Verdict::Deny(Reason::PERMISSION_DENIED, _)
        ));
    }

    /// And the kind it *may* open still opens, with what it holds.
    #[test]
    fn a_kind_the_device_holds_opens_with_the_permissions_it_holds() {
        let d = device_with(&[Permission::View, Permission::Forward]);
        let r = asking_for(
            SessionKind::Forward,
            &[Permission::View, Permission::Forward],
        );
        match decide_and_clamp(&GrantStoredPermissions, &d, &r).0 {
            Verdict::Grant(g) => assert!(g.contains(&Permission::Forward)),
            other => panic!("expected a grant, got {other:?}"),
        }
    }

    /// The kind is settled **before the policy is consulted**.
    ///
    /// Both checks refuse the same request with the same sentence, so this is
    /// what makes the first one worth having: deciding a session is
    /// impossible is not a question to put to a policy, and the policy this
    /// product is heading for is a prompt on somebody's screen. Asking a
    /// person to approve a session that was going to be refused whatever they
    /// said is how people learn to dismiss prompts.
    #[test]
    fn a_kind_the_device_may_not_open_is_refused_without_asking_the_policy() {
        /// Grants everything, and remembers being asked.
        struct Noticed(std::sync::atomic::AtomicBool);
        impl Policy for Noticed {
            fn decide(&self, device: &Device, _request: &SessionRequest) -> Verdict {
                self.0.store(true, std::sync::atomic::Ordering::Relaxed);
                Verdict::Grant(device.permissions.clone())
            }
        }

        let asked = Noticed(std::sync::atomic::AtomicBool::new(false));
        let d = device_with(&[Permission::View]);
        let r = asking_for(
            SessionKind::Forward,
            &[Permission::View, Permission::Forward],
        );
        let (verdict, _) = decide_and_clamp(&asked, &d, &r);
        assert!(matches!(verdict, Verdict::Deny(..)));
        assert!(
            !asked.0.load(std::sync::atomic::Ordering::Relaxed),
            "the policy was asked about a session that could not open"
        );

        // And it *is* asked about one that can, so this is not simply a check
        // that nothing gets through.
        let asked = Noticed(std::sync::atomic::AtomicBool::new(false));
        let d = device_with(&[Permission::View, Permission::Forward]);
        let (verdict, _) = decide_and_clamp(&asked, &d, &r);
        assert!(matches!(verdict, Verdict::Grant(_)));
        assert!(asked.0.load(std::sync::atomic::Ordering::Relaxed));
    }

    /// A policy that grants a subset without what the kind needs is refused
    /// too — the second check, after the policy has spoken.
    #[test]
    fn a_policy_that_grants_the_wrong_thing_does_not_open_the_session() {
        /// Grants view and nothing else, whatever was asked for.
        struct ViewOnly;
        impl Policy for ViewOnly {
            fn decide(&self, _device: &Device, _request: &SessionRequest) -> Verdict {
                Verdict::Grant(vec![Permission::View])
            }
        }

        // The device does hold `forward`, so the first check passes; the
        // policy is what does not grant it.
        let d = device_with(&[Permission::View, Permission::Forward]);
        let r = asking_for(SessionKind::Forward, &[Permission::Forward]);
        assert!(matches!(
            decide_and_clamp(&ViewOnly, &d, &r).0,
            Verdict::Deny(Reason::PERMISSION_DENIED, _)
        ));
    }

    #[test]
    fn refuse_all_refuses_even_a_fully_permitted_device() {
        let d = device_with(&[
            Permission::View,
            Permission::Control,
            Permission::Unattended,
        ]);
        assert!(matches!(
            RefuseAll.decide(&d, &request(&[Permission::View])),
            Verdict::Deny(Reason::HOST_DISABLED, _)
        ));
    }

    /// A policy is application code. It can have a bug, or be written by
    /// someone extending Esper. The stored record is the ceiling, and the
    /// clamp means no policy can raise it even by mistake.
    #[test]
    fn a_policy_cannot_grant_more_than_the_device_holds() {
        struct OverReaching;
        impl Policy for OverReaching {
            fn decide(&self, _d: &Device, _r: &SessionRequest) -> Verdict {
                Verdict::Grant(vec![
                    Permission::View,
                    Permission::Unattended,
                    Permission::FilesIn,
                ])
            }
        }

        let d = device_with(&[Permission::View]);
        let (verdict, over_reach) = decide_and_clamp(&OverReaching, &d, &request(&[]));

        match verdict {
            Verdict::Grant(g) => {
                assert_eq!(
                    g,
                    vec![Permission::View],
                    "only the stored permission survives"
                );
                assert!(!g.contains(&Permission::Unattended));
            }
            other => panic!("expected a clamped grant, got {other:?}"),
        }
        assert_eq!(
            over_reach.len(),
            2,
            "the attempted widening must be reported, not silently dropped"
        );
        assert!(over_reach.contains(&Permission::Unattended));
    }

    /// The most dangerous single case: a policy that tries to hand out
    /// unattended access to a device that was never granted it.
    #[test]
    fn a_policy_cannot_invent_unattended_access() {
        struct GrantsUnattended;
        impl Policy for GrantsUnattended {
            fn decide(&self, _d: &Device, _r: &SessionRequest) -> Verdict {
                Verdict::Grant(vec![Permission::Unattended])
            }
        }
        let d = device_with(&[Permission::View, Permission::Control]);
        let (verdict, over_reach) = decide_and_clamp(&GrantsUnattended, &d, &request(&[]));
        assert!(
            matches!(verdict, Verdict::Deny(Reason::PERMISSION_DENIED, _)),
            "a grant of nothing permitted must become a denial, not an empty grant"
        );
        assert_eq!(over_reach, vec![Permission::Unattended]);
    }

    #[test]
    fn a_denial_passes_through_the_clamp_unchanged() {
        let d = device_with(&[Permission::View]);
        let (verdict, over) = decide_and_clamp(&RefuseAll, &d, &request(&[Permission::View]));
        assert!(matches!(verdict, Verdict::Deny(Reason::HOST_DISABLED, _)));
        assert!(over.is_empty());
    }

    #[test]
    fn the_clamp_deduplicates_and_sorts() {
        struct Repeats;
        impl Policy for Repeats {
            fn decide(&self, _d: &Device, _r: &SessionRequest) -> Verdict {
                Verdict::Grant(vec![
                    Permission::Control,
                    Permission::View,
                    Permission::Control,
                ])
            }
        }
        let d = device_with(&[Permission::View, Permission::Control]);
        let (verdict, _) = decide_and_clamp(&Repeats, &d, &request(&[]));
        match verdict {
            Verdict::Grant(g) => {
                assert_eq!(g.len(), 2, "duplicates must not survive: {g:?}");
                let mut sorted = g.clone();
                sorted.sort_unstable();
                assert_eq!(g, sorted, "the grant should be in a stable order");
            }
            other => panic!("expected a grant, got {other:?}"),
        }
    }

    #[test]
    fn a_grant_becomes_a_grant_message_carrying_the_session_id() {
        let id = [7u8; 16];
        match response(&Verdict::Grant(vec![Permission::View]), id) {
            SessionResponse::Grant(g) => {
                assert_eq!(g.session_id, id);
                assert!(g.allows(Permission::View));
                assert!(!g.allows(Permission::Unattended));
            }
            other => panic!("expected a grant message, got {other:?}"),
        }
    }

    #[test]
    fn a_denial_becomes_a_deny_message_carrying_both_the_code_and_the_sentence() {
        let v = Verdict::Deny(Reason::OUT_OF_SCHEDULE, "outside 08:00-18:00".to_owned());
        match response(&v, [0u8; 16]) {
            SessionResponse::Deny(d) => {
                assert_eq!(d.reason, Reason::OUT_OF_SCHEDULE);
                assert!(d.detail.contains("08:00"));
            }
            other => panic!("expected a deny message, got {other:?}"),
        }
    }

    // ── the registry ────────────────────────────────────────────────────────

    #[test]
    fn a_new_registry_is_empty() {
        let r = SessionRegistry::new();
        assert_eq!(r.total(), 0);
        assert_eq!(r.live_for(&key(1)), 0);
        assert!(r.peers().is_empty());
    }

    #[test]
    fn revoking_a_peer_with_no_sessions_is_not_an_error() {
        assert_eq!(SessionRegistry::new().revoke(&key(1)), 0);
    }

    #[test]
    fn finishing_an_unknown_session_is_not_an_error() {
        let r = SessionRegistry::new();
        r.finished(&key(1), &[0u8; 16]);
        assert_eq!(r.total(), 0);
    }

    /// Bounds checks on a request from an authenticated peer — because an
    /// authenticated peer is not a trusted one.
    #[test]
    fn an_over_long_reason_is_rejected() {
        let mut r = request(&[Permission::View]);
        assert!(r.is_well_formed());
        r.reason = "x".repeat(esper_proto::MAX_REASON_LEN + 1);
        assert!(!r.is_well_formed());
    }

    #[test]
    fn a_reason_with_control_characters_is_rejected() {
        let mut r = request(&[Permission::View]);
        r.reason = "helping\u{7}\nwith email".to_owned();
        assert!(
            !r.is_well_formed(),
            "a reason is shown on the host's prompt; control characters must not reach it"
        );
    }

    #[test]
    fn an_absurd_number_of_requested_permissions_is_rejected() {
        let mut r = request(&[]);
        r.want = vec![Permission::View; esper_proto::MAX_REQUESTED_PERMISSIONS + 1];
        assert!(!r.is_well_formed());
    }
}
