// Integration tests. Test code throughout.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Revocation reaching a live session.
//!
//! Deleting a key from the store stops the *next* connection. It does nothing
//! about the one already open, and a revocation that leaves an attacker
//! connected is a delay, not a revocation. These tests use real connections
//! because that is the only way to observe whether the far side actually went
//! away.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Once};
use std::time::Duration;

use ownview_crypto::Identity;
use ownview_proto::{Hello, Reason, Role};
use ownview_transport::{AnyOf, Endpoint, NoPeers, OnlyPeer, Session, SessionRegistry};

const TEST_TIMEOUT: Duration = Duration::from_secs(20);

fn init() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}

fn loopback() -> SocketAddr {
    (Ipv4Addr::LOCALHOST, 0).into()
}

async fn bounded<F: std::future::Future<Output = T>, T>(what: &str, f: F) -> T {
    match tokio::time::timeout(TEST_TIMEOUT, f).await {
        Ok(v) => v,
        Err(_) => panic!("{what} did not finish within {TEST_TIMEOUT:?} — something hangs"),
    }
}

/// A live session between two endpoints, plus the registry the host uses.
struct Wired {
    _host_ep: Endpoint,
    _ctrl_ep: Endpoint,
    host: Session,
    controller: Session,
    registry: SessionRegistry,
    host_conn: quinn::Connection,
    peer_key: [u8; 32],
}

async fn wire_up() -> Wired {
    init();
    let host_id = Identity::generate().expect("host");
    let ctrl_id = Identity::generate().expect("controller");

    let host_ep = Endpoint::bind(
        &host_id,
        loopback(),
        Arc::new(AnyOf::new(vec![ctrl_id.public_key()])),
        Arc::new(NoPeers),
    )
    .expect("host endpoint");
    let addr = host_ep.local_addr().expect("addr");
    let ctrl_ep = Endpoint::bind(
        &ctrl_id,
        loopback(),
        Arc::new(NoPeers),
        Arc::new(OnlyPeer::new(host_id.public_key())),
    )
    .expect("controller endpoint");

    let hk = host_id.public_key();
    let ck = ctrl_id.public_key();

    let host_side = async {
        let conn = host_ep.accept().await.unwrap().unwrap();
        let s = Session::open(conn.clone(), Hello::new(Role::Host, hk, "host"))
            .await
            .map_err(|(e, _)| e)
            .expect("host session");
        (s, conn)
    };
    let ctrl_side = async {
        let conn = ctrl_ep.connect(addr).await.expect("connect");
        Session::open(conn, Hello::new(Role::Controller, ck, "controller"))
            .await
            .map_err(|(e, _)| e)
            .expect("controller session")
    };
    let ((host, host_conn), controller) = tokio::join!(host_side, ctrl_side);

    let registry = SessionRegistry::new();
    registry.register(ck, [1u8; 16], host_conn.clone());

    Wired {
        _host_ep: host_ep,
        _ctrl_ep: ctrl_ep,
        host,
        controller,
        registry,
        host_conn,
        peer_key: ck,
    }
}

/// The property. Revoking must reach a connection that is already open.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn revoking_closes_a_live_session_immediately() {
    bounded("revoking_closes_a_live_session", async {
        let w = wire_up().await;
        assert_eq!(w.registry.live_for(&w.peer_key), 1);
        assert_eq!(w.registry.total(), 1);

        // The session is genuinely usable before revocation.
        w.host
            .send_media(bytes::Bytes::from_static(b"frame"))
            .expect("send before revoke");
        let got = tokio::time::timeout(Duration::from_secs(5), w.controller.recv_media())
            .await
            .expect("datagram should arrive")
            .expect("datagram");
        assert_eq!(&got[..], b"frame");

        let closed = w.registry.revoke(&w.peer_key);
        assert_eq!(closed, 1, "revoke should report what it closed");
        assert_eq!(w.registry.live_for(&w.peer_key), 0);

        // The controller must observe the session ending, not hang.
        let why = tokio::time::timeout(Duration::from_secs(5), w.controller.closed())
            .await
            .expect("the controller must notice the session ended");

        // And it must learn *why*, so it does not simply reconnect.
        let text = format!("{why:?}");
        assert!(
            text.contains(&Reason::REVOKED.0.to_string()) || why.to_string().contains("revoked"),
            "the close should carry the revoked reason, got: {why} / {text}"
        );
    })
    .await;
}

/// A peer may hold several sessions at once — several viewers of one host.
/// Revocation must close all of them, not the most recent.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn revoking_closes_every_session_a_peer_holds() {
    bounded("revoking_closes_every_session", async {
        let w = wire_up().await;
        // Two more sessions from the same peer.
        w.registry
            .register(w.peer_key, [2u8; 16], w.host_conn.clone());
        w.registry
            .register(w.peer_key, [3u8; 16], w.host_conn.clone());
        assert_eq!(w.registry.live_for(&w.peer_key), 3);

        assert_eq!(
            w.registry.revoke(&w.peer_key),
            3,
            "all three must be closed, not just the newest"
        );
        assert_eq!(w.registry.total(), 0);
        assert!(w.registry.peers().is_empty());
    })
    .await;
}

#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn a_session_that_ends_normally_leaves_the_registry() {
    bounded("a_session_that_ends_normally", async {
        let w = wire_up().await;
        assert_eq!(w.registry.total(), 1);
        w.registry.finished(&w.peer_key, &[1u8; 16]);
        assert_eq!(
            w.registry.total(),
            0,
            "a finished session must be forgotten"
        );
        assert!(
            w.registry.peers().is_empty(),
            "the peer should disappear once it holds nothing"
        );
    })
    .await;
}

#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn finishing_one_session_leaves_the_others_alone() {
    bounded("finishing_one_session", async {
        let w = wire_up().await;
        w.registry
            .register(w.peer_key, [2u8; 16], w.host_conn.clone());
        assert_eq!(w.registry.live_for(&w.peer_key), 2);

        w.registry.finished(&w.peer_key, &[1u8; 16]);
        assert_eq!(
            w.registry.live_for(&w.peer_key),
            1,
            "only the named session should be removed"
        );
    })
    .await;
}

#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn close_all_ends_every_session_for_shutdown() {
    bounded("close_all", async {
        let w = wire_up().await;
        let other = Identity::generate().expect("other").public_key();
        w.registry.register(other, [9u8; 16], w.host_conn.clone());
        assert_eq!(w.registry.total(), 2);

        assert_eq!(w.registry.close_all(Reason::IDLE_TIMEOUT), 2);
        assert_eq!(w.registry.total(), 0);

        let why = tokio::time::timeout(Duration::from_secs(5), w.controller.closed())
            .await
            .expect("the controller must notice shutdown");
        assert!(!why.to_string().is_empty());
    })
    .await;
}

/// Revoking one peer must not disturb another's sessions.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn revoking_one_peer_does_not_touch_another() {
    bounded("revoking_one_peer", async {
        let w = wire_up().await;
        let other = Identity::generate().expect("other").public_key();
        w.registry.register(other, [9u8; 16], w.host_conn.clone());

        assert_eq!(w.registry.revoke(&other), 1);
        assert_eq!(
            w.registry.live_for(&w.peer_key),
            1,
            "the other peer's session must survive"
        );
        assert_eq!(w.registry.total(), 1);
    })
    .await;
}
