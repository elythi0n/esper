// Integration tests. Test code throughout.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Two peers meeting through a real broker.
//!
//! Over loopback, which is the honest limit of what one machine can test: the
//! *rendezvous* is exercised end to end — the handshake, the pinning, the
//! signed request, the matching, the introduction and the limits — while the
//! thing loopback cannot show is the hole punch itself, because there is no
//! NAT between two sockets on one host. What is proven here is that both peers
//! learn each other's addresses and keys; what needs two networks is whether a
//! punch through two routers succeeds.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Once};
use std::time::Duration;

use esper_crypto::Identity;
use esper_rendezvous::table::MAX_PER_ADDRESS;
use esper_rendezvous::wire::Refusal;
use esper_rendezvous::{Broker, Client, ClientError, Limits};
use esper_transport::{AnyOf, Endpoint, NoPeers};

fn init() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}

fn loopback() -> SocketAddr {
    (Ipv4Addr::LOCALHOST, 0).into()
}

fn candidate(port: u16) -> SocketAddr {
    (Ipv4Addr::new(192, 168, 1, 20), port).into()
}

/// A broker running in the background, and where to find it.
struct Running {
    addr: SocketAddr,
    key: [u8; 32],
    broker: Arc<Broker>,
    stop: tokio::sync::watch::Sender<bool>,
}

impl Running {
    fn start() -> Self {
        init();
        let identity = Identity::generate().expect("a broker key");
        let broker = Arc::new(
            Broker::bind(&identity, loopback(), Limits::default()).expect("bind the broker"),
        );
        let addr = broker.local_addr().expect("its address");
        let key = broker.key();
        let (stop, stopping) = tokio::sync::watch::channel(false);
        let serving = Arc::clone(&broker);
        tokio::spawn(async move { serving.run(stopping).await });
        Self {
            addr,
            key,
            broker,
            stop,
        }
    }

    /// Wait until the broker is holding this many peers.
    ///
    /// `arrive` returns when the request has been *sent*, not when it has been
    /// filed, so a test that asserts on the table immediately is asserting on
    /// a race.
    async fn until_waiting(&self, n: usize) {
        for _ in 0..200 {
            if self.broker.waiting() == n {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the broker holds {} peers, not {n}", self.broker.waiting());
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}

/// A peer's own endpoint: the socket its session would use, which is the one
/// the rendezvous has to go through.
fn endpoint_for(who: &Identity, broker_key: [u8; 32]) -> Endpoint {
    Endpoint::bind(
        who,
        loopback(),
        Arc::new(NoPeers),
        // The broker's key, because that is what it dials. A real peer also
        // names the devices it will dial afterwards.
        Arc::new(AnyOf::new(vec![broker_key])),
    )
    .expect("an endpoint")
}

/// A peer arriving at the broker.
async fn arrive_from(
    at: &Running,
    endpoint: &Endpoint,
    who: &Identity,
    with: &[[u8; 32]],
    port: u16,
) -> Result<Client, ClientError> {
    Client::arrive(endpoint, who, at.addr, at.key, with, vec![candidate(port)]).await
}

/// **The whole point: two peers that name each other learn where to try.**
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn two_peers_meet_and_each_learns_where_to_try() {
    let broker = Running::start();
    let host = Identity::generate().unwrap();
    let ctrl = Identity::generate().unwrap();

    // The host waits to be found, naming every device it is paired with.
    let host_ep = endpoint_for(&host, broker.key);
    let mut waiting = arrive_from(&broker, &host_ep, &host, &[ctrl.public_key()], 7645)
        .await
        .expect("the host should reach the broker");
    broker.until_waiting(1).await;

    // The controller goes looking.
    let ctrl_ep = endpoint_for(&ctrl, broker.key);
    let mut looking = arrive_from(&broker, &ctrl_ep, &ctrl, &[host.public_key()], 51000)
        .await
        .expect("the controller should reach the broker");

    let found = tokio::time::timeout(Duration::from_secs(5), looking.next())
        .await
        .expect("an introduction should not take five seconds")
        .expect("the controller should be introduced");
    assert_eq!(found.peer, host.public_key());
    assert_eq!(found.candidates, vec![candidate(7645)]);

    // And the waiting side is told too — an introduction one peer acts on and
    // the other does not is a hole punch with one fist.
    let told = tokio::time::timeout(Duration::from_secs(5), waiting.next())
        .await
        .expect("the host should be told as well")
        .expect("the host should be introduced");
    assert_eq!(told.peer, ctrl.public_key());
    assert_eq!(told.candidates, vec![candidate(51000)]);
}

/// **A device that was not named learns nothing at all.**
///
/// The property that makes a broker safe to leave running on the internet: an
/// unauthenticated party that knows a device's key must not be able to collect
/// its addresses by asking after it.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn a_stranger_asking_after_a_host_is_told_nothing() {
    let broker = Running::start();
    let host = Identity::generate().unwrap();
    let ctrl = Identity::generate().unwrap();
    let stranger = Identity::generate().unwrap();

    let host_ep = endpoint_for(&host, broker.key);
    let mut waiting = arrive_from(&broker, &host_ep, &host, &[ctrl.public_key()], 7645)
        .await
        .expect("the host arrives");
    broker.until_waiting(1).await;

    let prying_ep = endpoint_for(&stranger, broker.key);
    let mut prying = arrive_from(&broker, &prying_ep, &stranger, &[host.public_key()], 40000)
        .await
        .expect("anybody may connect to a broker — that is what it is for");

    // It connected, it asked, and it waits for ever.
    assert!(
        tokio::time::timeout(Duration::from_millis(400), prying.next())
            .await
            .is_err(),
        "a stranger must be left waiting, not introduced"
    );
    // And the host is not even told that somebody asked.
    assert!(
        tokio::time::timeout(Duration::from_millis(200), waiting.next())
            .await
            .is_err(),
        "the host must not learn that a stranger asked after it"
    );
}

/// The broker's key is pinned: another key in its place is not the broker.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn a_broker_with_the_wrong_key_is_not_the_broker() {
    let broker = Running::start();
    let host = Identity::generate().unwrap();
    let ctrl = Identity::generate().unwrap();
    let impostor = Identity::generate().unwrap();

    // The endpoint pins the impostor's key, so the real broker cannot satisfy
    // it: pinning is refused at the handshake, not afterwards.
    let host_ep = endpoint_for(&host, impostor.public_key());
    let err = Client::arrive(
        &host_ep,
        &host,
        broker.addr,
        impostor.public_key(),
        &[ctrl.public_key()],
        vec![candidate(7645)],
    )
    .await
    .expect_err("a broker whose key does not match is not the broker");

    // It fails at the handshake, which is the point: nothing is sent to a
    // machine that has not proven which one it is.
    assert!(
        matches!(
            err,
            ClientError::Transport(_) | ClientError::Stream(_) | ClientError::Silent
        ),
        "got {err:?}"
    );
    assert_eq!(broker.broker.waiting(), 0);
}

/// One address cannot fill the broker, and is told why rather than dropped.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn one_address_is_held_to_its_share_and_told_so() {
    let broker = Running::start();
    let peer = Identity::generate().unwrap();

    // Every one of these is loopback, so they are all "one address".
    let mut held = Vec::new();
    for n in 0..MAX_PER_ADDRESS {
        let who = Identity::generate().unwrap();
        let ep = endpoint_for(&who, broker.key);
        let client = arrive_from(&broker, &ep, &who, &[peer.public_key()], 20000 + n as u16)
            .await
            .expect("within the share");
        held.push((ep, client));
    }
    broker.until_waiting(MAX_PER_ADDRESS).await;

    let extra = Identity::generate().unwrap();
    let extra_ep = endpoint_for(&extra, broker.key);
    let mut over = arrive_from(&broker, &extra_ep, &extra, &[peer.public_key()], 30000)
        .await
        .expect("the connection is accepted");
    let refused = tokio::time::timeout(Duration::from_secs(5), over.next())
        .await
        .expect("the broker should answer rather than hang")
        .expect_err("over the share");
    assert!(
        matches!(refused, ClientError::Refused(Refusal::TooManyFromYou)),
        "a peer over the limit should be told which limit: got {refused:?}"
    );
}

/// A peer that leaves gives up its place, so nobody is handed a dead address.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn a_peer_that_leaves_gives_up_its_place() {
    let broker = Running::start();
    let host = Identity::generate().unwrap();
    let ctrl = Identity::generate().unwrap();

    let host_ep = endpoint_for(&host, broker.key);
    let waiting = arrive_from(&broker, &host_ep, &host, &[ctrl.public_key()], 7645)
        .await
        .expect("the host arrives");
    broker.until_waiting(1).await;

    // Dropping the client closes the connection, which is what a host exiting
    // does.
    drop(waiting);
    broker.until_waiting(0).await;
}

/// **The payoff: neither side was told where the other was, and they meet.**
///
/// The host is bound to a socket it both accepts on and reaches the broker
/// through — which is the engine's shape, and the reason for it is that a
/// router's mapping belongs to a socket. The controller is given no address at
/// all: it knows a broker and a device key, and everything else comes from the
/// introduction.
///
/// What loopback cannot prove is the hole punch, because there is no router in
/// the way. What it does prove is everything up to it.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn a_controller_given_only_a_broker_reaches_the_host() {
    let broker = Running::start();
    let host = Identity::generate().unwrap();
    let ctrl = Identity::generate().unwrap();

    // The host: accepts its paired controller, dials only the broker, and
    // does both on one socket.
    let host_ep = Endpoint::bind(
        &host,
        loopback(),
        Arc::new(AnyOf::new(vec![ctrl.public_key()])),
        Arc::new(AnyOf::new(vec![broker.key])),
    )
    .expect("the host's endpoint");
    let host_at = host_ep.local_addr().expect("its address");

    let mut waiting = Client::arrive(
        &host_ep,
        &host,
        broker.addr,
        broker.key,
        &[ctrl.public_key()],
        vec![host_at],
    )
    .await
    .expect("the host reaches its broker");
    broker.until_waiting(1).await;

    // The controller: dials the broker and, afterwards, the host. One socket
    // again, and no address for the host anywhere.
    let ctrl_ep = Endpoint::bind(
        &ctrl,
        loopback(),
        Arc::new(NoPeers),
        Arc::new(AnyOf::new(vec![broker.key, host.public_key()])),
    )
    .expect("the controller's endpoint");
    let ctrl_at = ctrl_ep.local_addr().expect("its address");
    let mut looking = Client::arrive(
        &ctrl_ep,
        &ctrl,
        broker.addr,
        broker.key,
        &[host.public_key()],
        vec![ctrl_at],
    )
    .await
    .expect("the controller reaches its broker");

    let met = tokio::time::timeout(Duration::from_secs(5), looking.next())
        .await
        .expect("an introduction")
        .expect("introduced");
    assert_eq!(met.peer, host.public_key());
    assert!(
        met.seen_at.is_some(),
        "the broker should say where it saw the host — that is the one address \
         the host cannot know about itself"
    );

    // The host is told as well, and would punch towards these.
    let told = tokio::time::timeout(Duration::from_secs(5), waiting.next())
        .await
        .expect("the host is told")
        .expect("introduced");
    assert_eq!(told.peer, ctrl.public_key());

    // Accept on the host's own socket, the one the broker saw.
    let accepting = tokio::spawn(async move { host_ep.accept().await });

    let conn = esper_rendezvous::race_to(&ctrl_ep, &met.everywhere(), Duration::from_secs(5))
        .await
        .expect("one of the candidates should answer");
    assert_eq!(
        conn.remote_address(),
        host_at,
        "the connection should be to the host's own socket"
    );

    let arrived = tokio::time::timeout(Duration::from_secs(5), accepting)
        .await
        .expect("the host should see it")
        .expect("join")
        .expect("a connection")
        .expect("a completed handshake");
    assert_eq!(arrived.remote_address(), ctrl_at);
}

/// A connection to nowhere names every address it tried.
///
/// "Could not connect" without saying what was tried is the error message this
/// project refuses to print, and a tier ladder makes that worse: by the time
/// it fails, several things have been attempted.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn a_race_that_loses_reports_every_address_it_tried() {
    init();
    let ctrl = Identity::generate().unwrap();
    let peer = Identity::generate().unwrap();
    let ep = Endpoint::bind(
        &ctrl,
        loopback(),
        Arc::new(NoPeers),
        Arc::new(AnyOf::new(vec![peer.public_key()])),
    )
    .expect("an endpoint");

    // Sockets nobody is listening on for Esper, held open so the ports are
    // certainly not free for something else.
    let silent: Vec<std::net::UdpSocket> = (0..3)
        .map(|_| std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("a socket"))
        .collect();
    let nowhere: Vec<SocketAddr> = silent
        .iter()
        .map(|s| s.local_addr().expect("its address"))
        .collect();

    let failures = esper_rendezvous::race_to(&ep, &nowhere, Duration::from_millis(300))
        .await
        .expect_err("nothing is there");
    assert_eq!(
        failures.len(),
        nowhere.len(),
        "every address tried should be named: {failures:?}"
    );
    for (addr, why) in &failures {
        assert!(
            nowhere.contains(addr),
            "an address nobody asked for: {addr}"
        );
        assert!(!why.is_empty(), "a failure with no reason");
    }
}
