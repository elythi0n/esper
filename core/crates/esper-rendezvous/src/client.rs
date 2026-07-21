//! The engine's half: wait to be found, or go looking.
//!
//! Symmetric, like the protocol: a host that wants to be reachable through a
//! broker and a controller looking for one send the same request and wait for
//! the same answer. The only difference is how many devices they name.
//!
//! # The broker's key is pinned
//!
//! A peer dials one specific machine, so it accepts one specific key. There is
//! no discovery, no certificate authority and no fallback: a broker whose key
//! does not match is not the broker, and the connection fails at the
//! handshake.
//!
//! # What comes back is not trusted
//!
//! An introduction is a *hint about where to try*. The addresses in it came
//! from a peer, through a broker, and neither is trusted with the session: the
//! QUIC handshake to the peer authenticates its device key, so a broker that
//! substituted an address of its own would be refused by the thing that
//! matters.

use std::net::SocketAddr;
use std::time::Duration;

use esper_crypto::Identity;
use esper_crypto::identity::KEY_LEN;
use esper_proto::Hello;
use esper_transport::{Control, Endpoint, Session, TransportError};

use crate::wire::{Answer, Introduction, Meet, Refusal};

/// What can go wrong meeting somebody.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// The broker could not be reached, or is not the broker.
    #[error("{0}")]
    Transport(#[from] TransportError),

    /// The stream to the broker failed.
    #[error("the broker's stream: {0}")]
    Stream(String),

    /// The broker refused.
    #[error("{}", .0.message())]
    Refused(Refusal),

    /// The broker went away without saying anything.
    #[error("the broker closed the connection without answering")]
    Silent,

    /// The broker introduced somebody else.
    ///
    /// Not an attack it could get anywhere with — the session would refuse the
    /// wrong key — but a broker doing this is broken or lying, and either way
    /// the answer is not to dial whatever it said.
    #[error("the broker introduced a different device than the one asked for")]
    WrongPeer,

    /// No candidate answered. Each failure is named.
    #[error("no candidate answered: {}", describe(.0))]
    Nowhere(Vec<(SocketAddr, String)>),

    /// The peer answered but the session did not open.
    #[error("the peer answered but the session did not open: {0}")]
    Session(String),
}

/// Every address tried, and why each failed.
///
/// "Could not connect" without saying what was tried is the error message this
/// project refuses to print, and a ladder of tiers makes it worse: by the time
/// it fails, several things have been attempted.
fn describe(failures: &[(SocketAddr, String)]) -> String {
    failures
        .iter()
        .map(|(addr, why)| format!("{addr}: {why}"))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Which broker to go through: where it is, and which key it must present.
///
/// The key rather than the fingerprint. A fingerprint is a truncated hash for
/// a person to read aloud; pinning against sixty-four bits of one would be a
/// security decision made on a birthday bound.
///
/// Named for how it reads at a call site — `reach(…, via, …)` — and to keep
/// it distinct from [`crate::Broker`], which is the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Via {
    /// Where the broker is.
    pub addr: SocketAddr,
    /// Its key, pinned.
    pub key: [u8; KEY_LEN],
}

/// Somebody to try, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Met {
    /// The peer's device key. **What the connection to it is authenticated
    /// against**, which is what makes a lying broker harmless.
    pub peer: [u8; KEY_LEN],
    /// Where to try, in the order that peer offered them.
    pub candidates: Vec<SocketAddr>,
    /// Where the broker saw that peer, if it said.
    pub seen_at: Option<SocketAddr>,
}

impl From<Introduction> for Met {
    fn from(i: Introduction) -> Self {
        Self {
            peer: i.peer,
            candidates: i.candidates,
            seen_at: i.seen_at,
        }
    }
}

impl Met {
    /// Everywhere to try, in the order to try it, without repeats.
    ///
    /// The peer's own addresses first and the broker's observation last: a
    /// peer's own list is usually right on a LAN or an overlay and costs
    /// nothing to try, while the reflexive address is the one that works
    /// through a router — and trying a local address that happens to be in
    /// use by something else on *this* machine first would be a connection to
    /// the wrong thing, which is exactly what the key check catches.
    #[must_use]
    pub fn everywhere(&self) -> Vec<SocketAddr> {
        let mut out = self.candidates.clone();
        if let Some(seen) = self.seen_at
            && !out.contains(&seen)
        {
            out.push(seen);
        }
        out
    }
}

/// A live conversation with a broker.
///
/// Held open for as long as the peer wants to be findable: a host keeps one
/// while it runs and hears about each device that comes looking; a controller
/// keeps one until it has what it came for.
#[derive(Debug)]
pub struct Client {
    control: Control,
    /// Kept because dropping it closes the connection, and the whole point of
    /// a host's client is that it stays.
    _conn: quinn::Connection,
}

impl Client {
    /// Ask a broker to introduce this device to the ones it names.
    ///
    /// **The endpoint is the caller's**, and that is load-bearing rather than
    /// tidy: the address a router will hand out is a property of the *socket*,
    /// so the reflexive candidate the broker observes is only useful if it is
    /// observed on the same socket the session will use. A rendezvous on its
    /// own socket would punch a hole for a port nothing was going to listen
    /// on.
    ///
    /// The endpoint's outbound authority must therefore accept the broker's
    /// key as well as the peers it will dial.
    ///
    /// `with` is one device for a controller looking for a host, or the whole
    /// paired list for a host waiting to be found. `candidates` are the
    /// addresses this machine believes it can be reached at; the broker adds
    /// what it sees, which is the one address this machine cannot know.
    pub async fn arrive(
        endpoint: &Endpoint,
        identity: &Identity,
        broker_addr: SocketAddr,
        broker_key: [u8; KEY_LEN],
        with: &[[u8; KEY_LEN]],
        candidates: Vec<SocketAddr>,
    ) -> Result<Self, ClientError> {
        let conn = endpoint.connect(broker_addr).await?;
        let (send, recv) = conn
            .open_bi()
            .await
            .map_err(|e| ClientError::Stream(e.to_string()))?;
        let mut control = Control::new(send, recv);

        let meet = Meet::new(identity, with, candidates, broker_key, crate::now_secs());
        control
            .send(&meet)
            .await
            .map_err(|e| ClientError::Stream(e.to_string()))?;

        Ok(Self {
            control,
            _conn: conn,
        })
    }

    /// Wait for the next introduction.
    ///
    /// A host calls this in a loop for as long as it runs. A controller calls
    /// it once.
    pub async fn next(&mut self) -> Result<Met, ClientError> {
        match self.control.recv::<Answer>().await {
            Ok(Answer::Introduced(i)) => Ok(Met::from(i)),
            Ok(Answer::Refused(why)) => Err(ClientError::Refused(why)),
            Err(esper_transport::ControlError::Finished) => Err(ClientError::Silent),
            Err(e) => Err(ClientError::Stream(e.to_string())),
        }
    }
}

/// Meet a peer at a broker and open a session with it.
///
/// The whole of tier 3 in one call: pin the broker, ask to be introduced to
/// one device, take the introduction, dial every address it named at once, and
/// hand back the session on the first that answered.
///
/// **The broker's part ends at the introduction.** What it said is a hint
/// about where to try; the session is authenticated against the device key, so
/// a broker that substituted an address of its own is refused by the thing
/// that matters. This returns a [`Session`] rather than a connection for that
/// reason — the authenticated thing is the useful thing.
pub async fn reach(
    endpoint: &Endpoint,
    identity: &Identity,
    via: Via,
    peer: [u8; KEY_LEN],
    candidates: Vec<SocketAddr>,
    hello: Hello,
    each: Duration,
) -> Result<(Met, Session), ClientError> {
    let mut client =
        Client::arrive(endpoint, identity, via.addr, via.key, &[peer], candidates).await?;

    let met = client.next().await?;
    if met.peer != peer {
        return Err(ClientError::WrongPeer);
    }

    let where_to = met.everywhere();
    let conn = race_to(endpoint, &where_to, each)
        .await
        .map_err(ClientError::Nowhere)?;

    // The broker is no longer needed: the two peers have found each other, and
    // holding the introduction open would keep a place in its table for a
    // meeting that has happened.
    drop(client);

    match Session::open(conn, hello).await {
        Ok(session) => Ok((met, session)),
        Err((e, _)) => Err(ClientError::Session(e.to_string())),
    }
}

/// Send packets towards a peer so this side's router opens a path.
///
/// **A hole punch is nothing more than both ends sending at once.** A router
/// forwards inbound packets for a mapping it made because something inside
/// sent out, so the side that will *accept* the session still has to send
/// something first. It does not want the connection it is dialling — a
/// controller accepts nobody, so the attempt will be refused — it wants the
/// mapping the attempt creates on the way out.
///
/// The attempt is therefore expected to fail, and its failure is not reported
/// as one. **Untested against real routers**: there is none between two
/// sockets on one machine, and what two consumer NATs do with this is an open
/// question until it is tried on two networks.
pub async fn punch_toward(endpoint: &Endpoint, met: &Met, each: Duration) {
    let where_to = met.everywhere();
    if where_to.is_empty() {
        return;
    }
    match race_to(endpoint, &where_to, each).await {
        // A connection is not what was wanted, and it is not a problem.
        Ok(conn) => {
            conn.close(0u32.into(), b"punch");
            tracing::debug!("a punch reached the peer, which is not what it was for");
        }
        Err(failures) => tracing::debug!(?failures, "punched towards a peer"),
    }
}

/// Where this machine believes it can be reached.
///
/// The bound address if it names one, and otherwise the address the kernel
/// would use to reach `toward` — which is asked for by connecting a throwaway
/// UDP socket, a question that sends no packets. A machine bound to every
/// address cannot say which of them a peer should use, and the one that gets
/// to the broker is the best guess available locally.
///
/// The broker adds its own observation, so this list not being complete is not
/// fatal — it is the LAN and overlay half of the answer.
#[must_use]
pub fn local_candidates(bound: SocketAddr, toward: SocketAddr) -> Vec<SocketAddr> {
    if !bound.ip().is_unspecified() {
        return vec![bound];
    }
    let Ok(probe) = std::net::UdpSocket::bind(match bound {
        SocketAddr::V4(_) => SocketAddr::from(([0, 0, 0, 0], 0)),
        SocketAddr::V6(_) => SocketAddr::from(([0u16; 8], 0)),
    }) else {
        return Vec::new();
    };
    // `connect` on a UDP socket sets a default destination and sends nothing.
    if probe.connect(toward).is_err() {
        return Vec::new();
    }
    match probe.local_addr() {
        Ok(local) => vec![SocketAddr::new(local.ip(), bound.port())],
        Err(_) => Vec::new(),
    }
}

/// Dial every candidate at once and keep the first that answers.
///
/// **First to complete wins**, which is the tier ladder's rule applied to one
/// tier's addresses: a LAN address and a reflexive address are both worth
/// trying and there is no way to know which will work. Every failure is
/// returned when none succeeds, because "could not connect" without saying
/// what was tried is the error message this project refuses to print.
pub async fn race_to(
    endpoint: &Endpoint,
    candidates: &[SocketAddr],
    each: Duration,
) -> Result<quinn::Connection, Vec<(SocketAddr, String)>> {
    let mut tries = tokio::task::JoinSet::new();
    for addr in candidates {
        let endpoint = endpoint.clone();
        let addr = *addr;
        tries.spawn(async move {
            (
                addr,
                endpoint
                    .connect_within(addr, each)
                    .await
                    .map_err(|e| e.to_string()),
            )
        });
    }

    let mut failed = Vec::new();
    while let Some(finished) = tries.join_next().await {
        match finished {
            Ok((addr, Ok(conn))) => {
                // The rest are abandoned: a second connection to the same peer
                // is a second session it would have to serve.
                tries.abort_all();
                tracing::debug!(%addr, "a candidate answered");
                return Ok(conn);
            }
            Ok((addr, Err(why))) => failed.push((addr, why)),
            Err(e) => failed.push((
                SocketAddr::from(([0, 0, 0, 0], 0)),
                format!("a dial did not finish: {e}"),
            )),
        }
    }
    Err(failed)
}
