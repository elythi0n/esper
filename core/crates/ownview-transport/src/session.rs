//! A connection that has completed its opening exchange.
//!
//! # Why this type exists
//!
//! A `quinn::Connection` whose handshake succeeded is **not** an authenticated
//! peer. TLS 1.3 finishes the client's handshake before the server has verified
//! the client's certificate, so a client the server intends to refuse still
//! sees `connect()` succeed; the alert arrives afterwards.
//!
//! Authentication is established when the peer has done something that required
//! accepting us. So there is no way to obtain a [`Session`] except by
//! completing a `Hello` exchange, and every operation — control, media, bulk —
//! hangs off `Session` rather than off the connection.
//!
//! The rule is therefore structural rather than remembered: **you cannot send
//! anything before the peer has proven it accepted you, because you do not have
//! the type that lets you.**

use std::net::SocketAddr;
use std::time::Duration;

use ownview_crypto::{Fingerprint, der, identity::KEY_LEN};
use ownview_proto::{Hello, Reason, Role};

/// How long the **entire** opening exchange may take.
///
/// This bounds every step, not just the read: opening or accepting the stream,
/// writing our `Hello`, and reading the peer's. A peer that connects and then
/// does nothing must cost us a timer, not a task pinned forever — the exchange
/// happens *before* authentication is established, so an unauthenticated peer
/// is what this limit exists to contain.
///
/// A timeout here is a **rejection**, not a network problem to retry through: a
/// peer that accepted us acts immediately, and one that refused us goes silent.
/// Retrying would turn a refusal into a slow loop.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

/// What can go wrong opening a session.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// The peer did not send its `Hello` in time.
    ///
    /// Overwhelmingly means the peer refused our key: TLS 1.3 lets our
    /// handshake complete before their verifier runs, so a refusal presents as
    /// silence. Treated as authentication failure rather than a transport hiccup.
    #[error(
        "peer sent no hello within {:?} — it most likely refused our device key",
        HELLO_TIMEOUT
    )]
    NoHello,

    /// The peer speaks a different protocol version.
    #[error("protocol version mismatch: we speak {ours}, peer speaks {theirs}")]
    VersionMismatch { ours: u16, theirs: u16 },

    /// The peer's `Hello` was malformed or dishonest.
    #[error("peer sent an unusable hello: {0}")]
    BadHello(String),

    /// Both sides claimed the same role.
    #[error("both peers claimed the {0:?} role; exactly one must be the host")]
    RoleConflict(Role),

    /// The peer's key in TLS and the key in its `Hello` disagree.
    ///
    /// A peer that authenticates with one key and announces another is either
    /// broken or attempting something; either way the session does not open.
    #[error("peer authenticated with one key and announced another")]
    KeyMismatch,

    /// The peer refused our device key, reported as a TLS alert.
    ///
    /// Distinguished from a transport failure deliberately. A user told "every
    /// connection tier failed" when the truth is "that device is not paired"
    /// will spend the evening checking their router.
    #[error("the peer refused our device key: {0}")]
    Refused(String),

    /// The transport failed.
    #[error("transport: {0}")]
    Connection(quinn::ConnectionError),

    /// A stream failed.
    #[error("stream: {0}")]
    Stream(String),

    /// Framing failed.
    #[error("frame: {0}")]
    Frame(#[from] ownview_proto::FrameError),

    /// The control stream failed.
    #[error("control stream: {0}")]
    Control(String),
}

/// TLS alerts arrive over QUIC as a `CRYPTO_ERROR` transport error whose code
/// is `0x100` plus the alert number (RFC 9001 §4.8).
const TLS_ALERT_BASE: u64 = 0x100;

/// Alerts that mean "I do not accept your identity", as opposed to a genuine
/// transport or protocol failure.
///
/// Kept to authentication-shaped alerts specifically: mapping every alert to a
/// key problem would mislead in the other direction, telling someone their
/// device is unpaired when the real fault is a protocol mismatch.
const AUTH_ALERTS: &[u64] = &[
    40,  // handshake_failure — what our verifier's rejection produces
    42,  // bad_certificate
    43,  // unsupported_certificate
    44,  // certificate_revoked
    45,  // certificate_expired
    46,  // certificate_unknown
    48,  // unknown_ca
    116, // certificate_required — the peer wanted a key we did not present
];

impl From<quinn::ConnectionError> for SessionError {
    /// Classify a connection error, separating "you were refused" from
    /// "the network failed".
    fn from(e: quinn::ConnectionError) -> Self {
        if let quinn::ConnectionError::TransportError(ref t) = e {
            let alert = u64::from(t.code).checked_sub(TLS_ALERT_BASE);
            if alert.is_some_and(|a| AUTH_ALERTS.contains(&a)) {
                {
                    // The peer's verifier put its reason in the alert's reason
                    // string, which is the most useful thing we have.
                    return Self::Refused(if t.reason.is_empty() {
                        format!("TLS alert {}", alert.unwrap_or_default())
                    } else {
                        t.reason.clone()
                    });
                }
            }
        }
        Self::Connection(e)
    }
}

impl From<crate::TransportError> for SessionError {
    /// A failed dial is classified the same way as a failed connection: a
    /// refusal must not present as a connectivity problem regardless of which
    /// layer noticed it.
    fn from(e: crate::TransportError) -> Self {
        match e {
            crate::TransportError::Connect { source, .. } => Self::from(source),
            other => Self::Stream(other.to_string()),
        }
    }
}

impl SessionError {
    /// The wire reason code to close with.
    #[must_use]
    pub const fn reason(&self) -> Reason {
        match self {
            Self::NoHello | Self::KeyMismatch => Reason::UNKNOWN_PEER_KEY,
            Self::VersionMismatch { .. } => Reason::PROTOCOL_VERSION_MISMATCH,
            Self::BadHello(_) | Self::RoleConflict(_) => Reason::MALFORMED_FRAME,
            Self::Refused(_) => Reason::UNKNOWN_PEER_KEY,
            Self::Connection(_) | Self::Stream(_) => Reason::ALL_TIERS_EXHAUSTED,
            Self::Frame(_) | Self::Control(_) => Reason::MALFORMED_FRAME,
        }
    }
}

/// Live statistics, for the session HUD and the local log.
///
/// Never leaves the machine. Displayed so a user can see *why* a session feels
/// slow rather than guessing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stats {
    /// Smoothed round-trip time.
    pub rtt: Duration,
    /// Packets the congestion controller believes were lost.
    pub lost_packets: u64,
    /// Bytes sent and received at the UDP layer.
    pub sent_bytes: u64,
    pub received_bytes: u64,
    /// Current congestion window, which is what actually bounds throughput.
    pub congestion_window: u64,
}

/// An authenticated, opened session with a peer.
pub struct Session {
    conn: quinn::Connection,
    peer_key: [u8; KEY_LEN],
    peer_hello: Hello,
    local_role: Role,
    /// The stream the `Hello` exchange happened on, kept as the session's
    /// primary control channel rather than discarded and reopened.
    control: Option<crate::Control>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("peer", &self.peer_fingerprint().inline())
            .field("peer_name", &self.peer_hello.name)
            .field("peer_role", &self.peer_hello.role)
            .field("remote", &self.remote_address())
            .finish()
    }
}

impl Session {
    /// Complete the opening exchange on an accepted or dialled connection.
    ///
    /// Sends our `Hello` and waits for the peer's. The peer's key is taken from
    /// the **TLS handshake**, not from its `Hello`, and the two are required to
    /// agree: the handshake is what was cryptographically proven, and the
    /// `Hello` is only what the peer says about itself.
    pub async fn open(
        conn: quinn::Connection,
        local: Hello,
    ) -> Result<Self, (SessionError, quinn::Connection)> {
        let local_role = local.role;

        // The **whole** exchange is bounded, not merely the read. Every step
        // here happens before authentication is established, so a peer that
        // stalls at any of them — connects and opens no stream, opens one and
        // sends nothing — must cost a timer rather than a task held open.
        let outcome =
            tokio::time::timeout(HELLO_TIMEOUT, Self::negotiate(&conn, local, local_role))
                .await
                .unwrap_or(Err(SessionError::NoHello));

        match outcome {
            Ok((peer_key, peer_hello, control)) => Ok(Self {
                conn,
                peer_key,
                peer_hello,
                local_role,
                control: Some(control),
            }),
            Err(e) => {
                // Tell the peer why, so a legitimate one can report it rather
                // than guessing at a silent drop.
                conn.close(
                    u32::from(e.reason().0).into(),
                    e.reason().to_string().as_bytes(),
                );
                Err((e, conn))
            }
        }
    }

    async fn negotiate(
        conn: &quinn::Connection,
        local: Hello,
        local_role: Role,
    ) -> Result<([u8; KEY_LEN], Hello, crate::Control), SessionError> {
        // The key TLS actually authenticated. This is the identity; anything in
        // the Hello is a claim.
        let peer_key = peer_key_from_tls(conn)?;

        // The host listens, the controller speaks first: whoever dialled opens
        // the stream. That removes any ambiguity about who initiates without
        // needing a negotiation for it.
        let (send, recv) = match local_role {
            Role::Controller => conn
                .open_bi()
                .await
                .map_err(|e| SessionError::Stream(e.to_string()))?,
            Role::Host => conn
                .accept_bi()
                .await
                .map_err(|e| SessionError::Stream(e.to_string()))?,
        };

        // A framed stream, not `read_to_end`: this stream stays open for the
        // rest of the session, and `read_to_end` waits for the sender to
        // *finish*, which a stream still in use never does.
        let mut control = crate::Control::new(send, recv);
        control
            .send(&local)
            .await
            .map_err(|e| SessionError::Control(e.to_string()))?;

        // The time bound is applied by `open` around the whole exchange, so
        // there is exactly one deadline rather than several that could each be
        // generous enough to matter.
        let peer: Hello = match control.recv().await {
            Ok(h) => h,
            // A peer that refused us closes without sending anything.
            Err(crate::ControlError::Finished) => return Err(SessionError::NoHello),
            Err(e) => return Err(SessionError::Control(e.to_string())),
        };

        if !peer.version_matches() {
            return Err(SessionError::VersionMismatch {
                ours: ownview_proto::PROTOCOL_VERSION,
                theirs: peer.proto,
            });
        }
        if !peer.name_is_acceptable() {
            return Err(SessionError::BadHello(
                "name is over-long or contains control characters".to_owned(),
            ));
        }
        if peer.role == local_role {
            return Err(SessionError::RoleConflict(local_role));
        }
        // The claim must match the proof.
        if peer.device != peer_key {
            return Err(SessionError::KeyMismatch);
        }

        Ok((peer_key, peer, control))
    }

    /// The peer's authenticated public key.
    #[must_use]
    pub const fn peer_key(&self) -> &[u8; KEY_LEN] {
        &self.peer_key
    }

    /// The peer's fingerprint, for display.
    #[must_use]
    pub fn peer_fingerprint(&self) -> Fingerprint {
        Fingerprint::of(&self.peer_key)
    }

    /// The peer's `Hello`. Its `name` and `caps` are peer-supplied claims.
    #[must_use]
    pub const fn peer_hello(&self) -> &Hello {
        &self.peer_hello
    }

    /// Which role this side holds. Exactly one side of a session is the host.
    #[must_use]
    pub const fn local_role(&self) -> Role {
        self.local_role
    }

    /// Whether this side is the host — the machine being viewed and controlled.
    ///
    /// Worth asking rather than inferring: several later decisions (who may
    /// prompt, who owns the framebuffer, whose input outranks whose) depend on
    /// it, and a comparison written out at each site is a comparison written
    /// wrongly at one of them.
    #[must_use]
    pub const fn is_host(&self) -> bool {
        matches!(self.local_role, Role::Host)
    }

    /// Capabilities both sides advertise. The only sanctioned way to decide
    /// what a session supports.
    #[must_use]
    pub fn common_caps(&self, local: &Hello) -> Vec<String> {
        local.common_caps(&self.peer_hello)
    }

    /// The peer's current address. Changes when the connection migrates.
    #[must_use]
    pub fn remote_address(&self) -> SocketAddr {
        self.conn.remote_address()
    }

    /// The session's primary control channel — the stream the `Hello` exchange
    /// happened on.
    ///
    /// Taken rather than borrowed, once: the control channel has a single owner
    /// for the life of the session, and two tasks reading frames off one stream
    /// would interleave halves of different messages.
    pub fn take_control(&mut self) -> Option<crate::Control> {
        self.control.take()
    }

    /// Open an additional bidirectional control stream.
    pub async fn open_control(&self) -> Result<crate::Control, SessionError> {
        let (s, r) = self
            .conn
            .open_bi()
            .await
            .map_err(|e| SessionError::Stream(e.to_string()))?;
        Ok(crate::Control::new(s, r))
    }

    /// Accept an additional control stream the peer opened.
    pub async fn accept_control(&self) -> Result<crate::Control, SessionError> {
        let (s, r) = self
            .conn
            .accept_bi()
            .await
            .map_err(|e| SessionError::Stream(e.to_string()))?;
        Ok(crate::Control::new(s, r))
    }

    /// Open a reliable unidirectional bulk stream, for file transfer.
    pub async fn open_bulk(&self) -> Result<quinn::SendStream, SessionError> {
        self.conn
            .open_uni()
            .await
            .map_err(|e| SessionError::Stream(e.to_string()))
    }

    /// Accept a bulk stream the peer opened.
    pub async fn accept_bulk(&self) -> Result<quinn::RecvStream, SessionError> {
        self.conn
            .accept_uni()
            .await
            .map_err(|e| SessionError::Stream(e.to_string()))
    }

    /// Send a media datagram.
    ///
    /// Unreliable by design: a lost video packet must never delay the next one,
    /// and retransmitting a frame already too late to display costs latency for
    /// nothing.
    pub fn send_media(&self, payload: bytes::Bytes) -> Result<(), SessionError> {
        self.conn
            .send_datagram(payload)
            .map_err(|e| SessionError::Stream(e.to_string()))
    }

    /// Receive a media datagram.
    pub async fn recv_media(&self) -> Result<bytes::Bytes, SessionError> {
        self.conn.read_datagram().await.map_err(SessionError::from)
    }

    /// The largest datagram that will fit without fragmenting, if datagrams are
    /// available at all. `None` means the peer did not enable them.
    #[must_use]
    pub fn max_datagram_size(&self) -> Option<usize> {
        self.conn.max_datagram_size()
    }

    /// Current transport statistics.
    #[must_use]
    pub fn stats(&self) -> Stats {
        let s = self.conn.stats();
        Stats {
            rtt: s.path.rtt,
            lost_packets: s.path.lost_packets,
            sent_bytes: s.udp_tx.bytes,
            received_bytes: s.udp_rx.bytes,
            congestion_window: s.path.cwnd,
        }
    }

    /// Close with a reason the peer can act on.
    ///
    /// Both the numeric code and its text are sent: the code so a client can
    /// branch, the text so a person reading the UI understands.
    pub fn close(&self, reason: Reason) {
        self.conn
            .close(u32::from(reason.0).into(), reason.to_string().as_bytes());
    }

    /// Wait until the session ends, and report why.
    pub async fn closed(&self) -> quinn::ConnectionError {
        self.conn.closed().await
    }
}

/// Extract the peer's public key from the completed TLS handshake.
///
/// This is the authenticated identity. It comes from the handshake rather than
/// from anything the peer sent afterwards, which is the distinction that makes
/// it trustworthy.
fn peer_key_from_tls(conn: &quinn::Connection) -> Result<[u8; KEY_LEN], SessionError> {
    let identity = conn
        .peer_identity()
        .ok_or_else(|| SessionError::BadHello("peer presented no key".to_owned()))?;

    let certs = identity
        .downcast::<Vec<rustls_pki_types::CertificateDer<'static>>>()
        .map_err(|_| SessionError::BadHello("unexpected peer identity type".to_owned()))?;

    let first = certs
        .first()
        .ok_or_else(|| SessionError::BadHello("peer presented an empty key".to_owned()))?;

    der::public_key_from_spki(first.as_ref())
        .ok_or_else(|| SessionError::BadHello("peer key is not an Ed25519 SPKI".to_owned()))
}
