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

use esper_crypto::{Fingerprint, der, identity::KEY_LEN};
use esper_proto::{Hello, Reason, Role};

/// How long the **entire** opening exchange may take.
///
/// This bounds every step, not just the read: opening or accepting the stream,
/// writing our `Hello`, and reading the peer's. A peer that connects and then
/// does nothing must cost us a timer, not a task pinned forever — the exchange
/// happens *before* authentication is established, so an unauthenticated peer
/// is what this limit exists to contain.
///
/// A timeout here is *usually* a **rejection** rather than a network problem to
/// retry through: a peer that accepted us acts immediately, and one that
/// refused us goes silent. Retrying would turn a refusal into a slow loop.
///
/// Usually, not always — see [`SessionError::HelloTimedOut`], which is what
/// the ambiguous case is called.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

/// A round trip a person can read, in whole milliseconds.
fn readable(rtt: Duration) -> String {
    format!("{:.0} ms", rtt.as_secs_f64() * 1000.0)
}

/// What can go wrong opening a session.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// The peer closed the exchange without saying anything.
    ///
    /// **Not the ordinary refusal**, which arrives as
    /// [`SessionError::Refused`] carrying the peer's own TLS alert — measured,
    /// and pinned by `a_refusal_is_reported_as_authentication_not_as_a_network_problem`.
    /// The reasoning here used to claim otherwise: that TLS 1.3 lets our
    /// handshake finish before the peer's verifier runs, so a refusal presents
    /// as silence. It can, but it is not what this codebase does.
    ///
    /// What reaches here is a peer that got as far as the control stream and
    /// then finished it without a word — which is still most likely a refusal,
    /// and unlike a *timeout* it is at least a positive act rather than an
    /// absence.
    #[error("peer closed the session without a hello — it refused our device key")]
    NoHello,

    /// The peer said nothing at all within [`HELLO_TIMEOUT`].
    ///
    /// **Two causes with one symptom, and the round trip tells them apart.** A
    /// peer that refused us goes silent, and so does a link too slow to carry
    /// the exchange in five seconds — a hundred-kilobit uplink with a full
    /// buffer takes longer than that to get the bytes across.
    ///
    /// This used to be reported as the refusal, in those words, which sends
    /// somebody on a slow link off to re-pair a device that is paired
    /// perfectly well. Measured: a session over a 100 kbit/s link with a
    /// second of queue was told its device key had most likely been refused —
    /// the handshake's own datagrams had filled the queue and six of them were
    /// dropped, so the exchange never finished.
    ///
    /// **Both causes are named, and neither is ranked, because nothing here
    /// can tell them apart.** Two candidate discriminators were tried against
    /// a real 100 kbit/s link and both failed:
    ///
    /// - The **round trip** reads 16 ms. By the time the deadline passes the
    ///   sender has stopped, so the queue that broke the handshake has
    ///   drained and the path looks healthy.
    /// - **Packets lost** reads zero. The relay dropped six of the
    ///   handshake's own datagrams, but with a handful of packets in flight
    ///   QUIC has not inferred their loss within five seconds — loss
    ///   detection needs acknowledgements it never got.
    ///
    /// So the numbers are reported as evidence rather than as a verdict. A
    /// wrong verdict here is expensive: told its key was refused, somebody
    /// re-pairs a device that was paired perfectly well, and the actual
    /// problem — a link that cannot carry a handshake — goes unexamined.
    #[error(
        "peer sent no hello within {:?} — either it refused our device key, or the link \
         did not finish carrying the exchange (round trip {}, {lost} packet(s) lost)",
        HELLO_TIMEOUT,
        readable(*rtt)
    )]
    HelloTimedOut { rtt: Duration, lost: u64 },

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
    Frame(#[from] esper_proto::FrameError),

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

/// Why the peer closed, when it said.
///
/// Only an *application* close carries one: that is a decision one side made
/// and named. A transport failure is not a decision, and reporting a reason
/// for it would invent an explanation.
fn peer_close_reason(conn: &quinn::Connection) -> Option<(Reason, String)> {
    match conn.close_reason()? {
        quinn::ConnectionError::ApplicationClosed(closed) => {
            let code = u16::try_from(u64::from(closed.error_code)).unwrap_or(0);
            Some((
                Reason(code),
                // The peer's own words, and it may be a peer that has just
                // decided something about us.
                peer_reason(&String::from_utf8_lossy(&closed.reason)),
            ))
        }
        _ => None,
    }
}

/// A peer's own words, made safe to print.
///
/// This string comes from the other end of a connection that has just refused
/// us, and it ends up in a terminal and in a log. So: control characters
/// removed, because a refusal must not be able to move somebody's cursor or
/// clear their screen, and bounded, because "the peer said why" is worth a
/// sentence and not a megabyte.
fn peer_reason(raw: &str) -> String {
    /// One line of explanation. Longer than anything this implementation
    /// sends, short enough to read.
    const MOST: usize = 160;

    let cleaned: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MOST)
        .collect();
    let trimmed = cleaned.trim();
    if raw.chars().count() > MOST {
        format!("{trimmed}…")
    } else {
        trimmed.to_owned()
    }
}

/// Classify a failure during the opening exchange.
///
/// **A stream failing is a symptom; the connection knows the cause.** And the
/// cause is often "your key was refused": TLS 1.3 finishes the *client's*
/// handshake before the server verifies the client's certificate, so a
/// controller whose key is not paired sees `connect()` succeed and then its
/// first stream fail. Reported as a stream error, that is "connection lost" —
/// which sends somebody to check their router about a device that simply is
/// not paired.
///
/// So the connection is asked why it closed, and its answer is used **only**
/// when it is an authentication alert. Anything else keeps the symptom, which
/// is the more specific description of a genuine transport fault.
fn during_open(conn: &quinn::Connection, symptom: SessionError) -> SessionError {
    match conn.close_reason() {
        Some(cause) => match SessionError::from(cause) {
            refused @ SessionError::Refused(_) => refused,
            _ => symptom,
        },
        None => symptom,
    }
}

impl From<quinn::ConnectionError> for SessionError {
    /// Classify a connection error, separating "you were refused" from
    /// "the network failed".
    ///
    /// **Two variants carry a TLS alert, and the difference is which side
    /// objected.** `TransportError` is our own TLS layer refusing the peer;
    /// `ConnectionClosed` is the peer's `CONNECTION_CLOSE` frame telling us it
    /// refused *us*. Only the second reaches a controller whose key is not
    /// paired — and only the first was inspected here, so the one refusal a
    /// user actually meets was reported as a broken network for as long as
    /// this code has existed. Nothing tested it, because the test would have
    /// had to be about the case nobody had.
    fn from(e: quinn::ConnectionError) -> Self {
        let alert = match &e {
            quinn::ConnectionError::TransportError(t) => {
                Some((u64::from(t.code), t.reason.clone()))
            }
            quinn::ConnectionError::ConnectionClosed(f) => Some((
                u64::from(f.error_code),
                String::from_utf8_lossy(&f.reason).into_owned(),
            )),
            _ => None,
        };

        if let Some((code, reason)) = alert
            && let Some(number) = code.checked_sub(TLS_ALERT_BASE)
            && AUTH_ALERTS.contains(&number)
        {
            // The peer's verifier put its reason in the alert's reason string,
            // which is the most useful thing we have — and it is *the peer's*
            // words, on their way to somebody's terminal.
            return Self::Refused(if reason.is_empty() {
                format!("TLS alert {number}")
            } else {
                peer_reason(&reason)
            });
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
            // The timeout goes here too: with no `Hello` there is nothing
            // better to say, and the guess a *reason code* carries matters
            // less than the sentence a person reads.
            Self::NoHello | Self::HelloTimedOut { .. } | Self::KeyMismatch => {
                Reason::UNKNOWN_PEER_KEY
            }
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
            match tokio::time::timeout(HELLO_TIMEOUT, Self::negotiate(&conn, local, local_role))
                .await
            {
                Ok(outcome) => outcome,
                // The round trip as the path last measured it, which is the one
                // piece of evidence available for telling a refusal from a link
                // that is merely slow.
                Err(_) => {
                    let path = conn.stats().path;
                    Err(SessionError::HelloTimedOut {
                        rtt: path.rtt,
                        lost: path.lost_packets,
                    })
                }
            };

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
                .map_err(|e| during_open(conn, SessionError::Stream(e.to_string())))?,
            Role::Host => conn
                .accept_bi()
                .await
                .map_err(|e| during_open(conn, SessionError::Stream(e.to_string())))?,
        };

        // A framed stream, not `read_to_end`: this stream stays open for the
        // rest of the session, and `read_to_end` waits for the sender to
        // *finish*, which a stream still in use never does.
        let mut control = crate::Control::new(send, recv);
        control
            .send(&local)
            .await
            .map_err(|e| during_open(conn, SessionError::Control(e.to_string())))?;

        // The time bound is applied by `open` around the whole exchange, so
        // there is exactly one deadline rather than several that could each be
        // generous enough to matter.
        let peer: Hello = match control.recv().await {
            Ok(h) => h,
            // A peer that refused us closes without sending anything.
            Err(crate::ControlError::Finished) => {
                return Err(during_open(conn, SessionError::NoHello));
            }
            Err(e) => return Err(during_open(conn, SessionError::Control(e.to_string()))),
        };

        if !peer.version_matches() {
            return Err(SessionError::VersionMismatch {
                ours: esper_proto::PROTOCOL_VERSION,
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

    /// Something that can accept a control stream later, without borrowing
    /// the session.
    ///
    /// For a stream the peer opens **lazily** — input is the case: a session
    /// that never sends a keystroke never opens the stream, so accepting it
    /// means waiting for something that may never arrive. That waiting belongs
    /// in a task, and a task cannot hold a borrow of the session.
    ///
    /// Kept here rather than handing out the connection so that nothing
    /// outside this crate has to name a `quinn::Connection`: the transport's
    /// choice of QUIC stays the transport's business.
    #[must_use]
    pub fn control_acceptor(&self) -> ControlAcceptor {
        ControlAcceptor {
            conn: self.conn.clone(),
        }
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

    /*
        **There is deliberately no bulk-stream pair here.**

        There was: `open_bulk` and `accept_bulk`, two lines each over quinn's
        unidirectional streams, documented "for file transfer" — and the file
        transfer went another way. `cp` opens a *control* stream of its own,
        which is a separate QUIC stream and so carries no head-of-line cost,
        and speaks the same framed messages as everything else rather than a
        second wire format for one feature.

        They were removed rather than left: this is the crate that carries the
        security boundary, and a public API nothing uses is an invitation to
        wire the next thing onto a path with no callers, no fixtures and no
        checks behind it.
    */

    /// A handle for sending and receiving media, detached from the session.
    ///
    /// # Why this exists rather than sharing the `Session`
    ///
    /// Capture blocks — `XShmGetImage`, `AcquireNextFrame`, a PipeWire wait —
    /// so the capture and encode loop gets a thread of its own, and that
    /// thread is what has the frames to send. It cannot borrow the session,
    /// which is being used by the async task running the control stream.
    ///
    /// A `MediaChannel` is cheap to clone, `Send` and `Sync`, and can do
    /// exactly two things: send a datagram and say how large one may be. That
    /// is the whole surface the sending thread needs, and keeping it to that
    /// means the thread cannot accidentally reach for the control stream.
    #[must_use]
    pub fn media_channel(&self) -> MediaChannel {
        MediaChannel {
            conn: self.conn.clone(),
        }
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

    /// Wait for the peer to close, for at most this long.
    ///
    /// **The last thing said on a stream is not safe until the peer has read
    /// it.** Closing a QUIC connection discards whatever has not been sent, and
    /// dropping a session is what closes it — so a side that answers and
    /// returns can throw its own answer away. Measured three times in this
    /// project: a shell's exit status, a broker's refusal, and a session
    /// denial, each arriving at the far end as "connection lost".
    ///
    /// A close *reason* needs none of this: it travels in the close frame
    /// itself. This is for the stream data that precedes it.
    ///
    /// Bounded, because a peer that never closes must not hold anything open;
    /// and free when the peer has already gone, which is the common case.
    pub async fn linger(&self, at_most: std::time::Duration) {
        let _ = tokio::time::timeout(at_most, self.conn.closed()).await;
    }

    /// Why the peer closed, when it said.
    ///
    /// The same question [`MediaChannel::peer_close_reason`] answers, asked of
    /// the session — because a session with no media in it, a terminal, has
    /// the same need to report that the host ended it deliberately rather than
    /// that the link broke.
    #[must_use]
    pub fn peer_close_reason(&self) -> Option<(Reason, String)> {
        peer_close_reason(&self.conn)
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

/// The media half of a session: datagrams in and out, nothing else.
///
/// Cloneable and shareable, so the thread that captures can send without
/// holding the session the control stream is using.
#[derive(Debug, Clone)]
pub struct MediaChannel {
    conn: quinn::Connection,
}

impl MediaChannel {
    /// Send one datagram.
    ///
    /// Unreliable by design: a lost video packet must never delay the next
    /// one, and retransmitting a frame already too late to display costs
    /// latency for nothing.
    pub fn send(&self, payload: bytes::Bytes) -> Result<(), SessionError> {
        self.conn
            .send_datagram(payload)
            .map_err(|e| SessionError::Stream(e.to_string()))
    }

    /// How many bytes of datagram the transport will take right now.
    ///
    /// # Why a caller has to ask
    ///
    /// [`Self::send`] does **not** fail when there is no room. quinn's own
    /// documentation is explicit: *"previously queued datagrams which are
    /// still unsent may be discarded to make space for this datagram, in order
    /// of oldest to newest"* — and it returns success while doing it. So a
    /// sender that keeps handing over datagrams gets `Ok` for every one and
    /// loses the earliest of them, which are the *leading* datagrams of
    /// whatever frame is being assembled.
    ///
    /// That is the worst possible loss pattern for video. A frame that is
    /// never sent costs one frame; a frame that arrives with its first
    /// datagram missing costs the frame **and** the reference chain behind it,
    /// which is what turns a slow link into a keyframe storm: measured at 200
    /// kbit/s with 100 ms of buffer, fifty-six keyframe requests in fifteen
    /// seconds and not one picture, while the delta frames — one datagram
    /// each — arrived perfectly well and were discarded for want of a
    /// reference.
    ///
    /// So a sender asks first, and sends a frame only if the whole of it
    /// fits.
    #[must_use]
    pub fn room_for_datagrams(&self) -> usize {
        self.conn.datagram_send_buffer_space()
    }

    /// Receive one datagram.
    pub async fn recv(&self) -> Result<bytes::Bytes, SessionError> {
        self.conn.read_datagram().await.map_err(SessionError::from)
    }

    /// The largest datagram that will fit without fragmenting.
    ///
    /// `None` means the peer did not enable datagrams at all, which for this
    /// protocol means it is not a peer that can carry video.
    #[must_use]
    pub fn max_datagram_size(&self) -> Option<usize> {
        self.conn.max_datagram_size()
    }

    /// Whether the connection is still up.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.conn.close_reason().is_none()
    }

    /// Close with a reason the peer can act on.
    ///
    /// On the media half because the thread that sends is the thread that
    /// notices the session is over — a capture loop that finds its display
    /// gone should be able to say so without reaching for the session.
    pub fn close(&self, reason: Reason) {
        self.conn
            .close(u32::from(reason.0).into(), reason.to_string().as_bytes());
    }

    /// Why the peer closed, once it has.
    ///
    /// `None` while the connection is up, or when it ended without either side
    /// saying anything — a timeout or a broken path rather than a decision.
    ///
    /// The string is peer-supplied and returned alongside the code precisely
    /// because a newer peer may send a code this build has no name for: the
    /// code is what a caller branches on, the string is what a person reads.
    #[must_use]
    pub fn peer_close_reason(&self) -> Option<(Reason, String)> {
        peer_close_reason(&self.conn)
    }

    /// Wait for the peer to close, for at most this long.
    ///
    /// See [`Session::linger`]. On the media half as well because a caller
    /// that has handed its session on still holds one of these, and the wait
    /// belongs at the end of everything rather than at the end of whatever
    /// happens to still own the session.
    pub async fn linger(&self, at_most: std::time::Duration) {
        let _ = tokio::time::timeout(at_most, self.conn.closed()).await;
    }
    /// What the transport knows about the path.
    ///
    /// Available from the media half because both sides of a session want it:
    /// a host reports what it is spending and a controller reports what it is
    /// waiting for, and neither should have to hold the session to ask.
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
}

/// Accepts a control stream the peer opens later.
///
/// See [`Session::control_acceptor`].
#[derive(Debug, Clone)]
pub struct ControlAcceptor {
    conn: quinn::Connection,
}

impl ControlAcceptor {
    /// Wait for the peer to open a control stream.
    pub async fn accept(&self) -> Result<crate::Control, SessionError> {
        let (s, r) = self
            .conn
            .accept_bi()
            .await
            .map_err(|e| SessionError::Stream(e.to_string()))?;
        Ok(crate::Control::new(s, r))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn a_peers_refusal_reason_cannot_move_the_cursor_or_fill_the_screen() {
        // A refusal is printed. It arrives from the far end of a connection
        // that just declined to talk to us, which is not a reason to trust
        // its formatting.
        assert_eq!(
            peer_reason("peer key is not paired\u{1b}[2J with this device"),
            "peer key is not paired [2J with this device"
        );
        assert_eq!(peer_reason("  spaced  "), "spaced");

        let long = "x".repeat(1000);
        let bounded = peer_reason(&long);
        assert!(
            bounded.chars().count() <= 161,
            "a peer must not be able to print a thousand characters: {}",
            bounded.chars().count()
        );
        assert!(bounded.ends_with('…'), "truncation should be visible");
    }

    #[test]
    fn a_refusal_frame_from_the_peer_is_read_as_a_refusal() {
        // 0x100 + 40 (handshake_failure) is what a rejected device key
        // produces, and it arrives in the peer's close frame rather than in a
        // transport error of our own — which is the distinction this used to
        // get wrong.
        let frame = quinn::ConnectionClose {
            error_code: quinn::TransportErrorCode::crypto(40),
            frame_type: None,
            reason: bytes::Bytes::from_static(b"peer key is not paired with this device"),
        };
        match SessionError::from(quinn::ConnectionError::ConnectionClosed(frame)) {
            SessionError::Refused(why) => {
                assert_eq!(why, "peer key is not paired with this device");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_close_that_is_not_an_authentication_alert_stays_a_transport_failure() {
        // Mapping every close to "you were refused" would tell somebody their
        // device is unpaired when the real fault is a protocol mismatch.
        let frame = quinn::ConnectionClose {
            error_code: quinn::TransportErrorCode::PROTOCOL_VIOLATION,
            frame_type: None,
            reason: bytes::Bytes::from_static(b"something else"),
        };
        assert!(matches!(
            SessionError::from(quinn::ConnectionError::ConnectionClosed(frame)),
            SessionError::Connection(_)
        ));
    }
}
