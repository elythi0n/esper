// Integration tests. Test code throughout.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Two properties that can only be observed on a real connection: that a
//! session survives its local address changing, and that session keys are not
//! derived from the long-lived device keys.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::{Arc, Once};
use std::time::Duration;

use esper_crypto::Identity;
use esper_proto::{Hello, Role};
use esper_transport::{AnyOf, Endpoint, NoPeers, OnlyPeer, Session};

const TEST_TIMEOUT: Duration = Duration::from_secs(25);

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

/// A live session, and the endpoints behind it.
struct Wired {
    /// Held, not read: dropping an endpoint takes its connections with it.
    _host_ep: Endpoint,
    ctrl_ep: Endpoint,
    host: Session,
    controller: Session,
}

async fn wire(host_id: &Identity, ctrl_id: &Identity) -> Wired {
    init();
    let host_ep = Endpoint::bind(
        host_id,
        loopback(),
        Arc::new(AnyOf::new(vec![ctrl_id.public_key()])),
        Arc::new(NoPeers),
    )
    .expect("host endpoint");
    let addr = host_ep.local_addr().expect("addr");
    let ctrl_ep = Endpoint::bind(
        ctrl_id,
        loopback(),
        Arc::new(NoPeers),
        Arc::new(OnlyPeer::new(host_id.public_key())),
    )
    .expect("controller endpoint");

    let hk = host_id.public_key();
    let ck = ctrl_id.public_key();
    let host_side = async {
        let conn = host_ep.accept().await.unwrap().unwrap();
        Session::open(conn, Hello::new(Role::Host, hk, "host"))
            .await
            .map_err(|(e, _)| e)
            .expect("host session")
    };
    let ctrl_side = async {
        let conn = ctrl_ep.connect(addr).await.expect("connect");
        Session::open(conn, Hello::new(Role::Controller, ck, "controller"))
            .await
            .map_err(|(e, _)| e)
            .expect("controller session")
    };
    let (host, controller) = tokio::join!(host_side, ctrl_side);
    Wired {
        _host_ep: host_ep,
        ctrl_ep,
        host,
        controller,
    }
}

/// Send a datagram one way and require it to arrive.
async fn round_trip(from: &Session, to: &Session, payload: &'static [u8]) {
    from.send_media(bytes::Bytes::from_static(payload))
        .expect("send");
    let got = tokio::time::timeout(Duration::from_secs(5), to.recv_media())
        .await
        .unwrap_or_else(|_| panic!("datagram {payload:?} never arrived"))
        .expect("datagram");
    assert_eq!(&got[..], payload);
}

/// A laptop moving from wifi to a tether must keep its session.
///
/// QUIC identifies a connection by its connection ID rather than the address
/// four-tuple, so the peer follows. This is one of the four properties QUIC was
/// chosen for, and the only way to know it works is to change the address and
/// keep sending.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn a_session_survives_the_controller_changing_its_local_address() {
    bounded("migration", async {
        let host_id = Identity::generate().expect("host");
        let ctrl_id = Identity::generate().expect("controller");
        let w = wire(&host_id, &ctrl_id).await;

        let before = w.host.remote_address();
        round_trip(&w.controller, &w.host, b"before-migration").await;

        // Move the controller to a different local socket, as a NAT rebinding
        // or a network change would.
        let fresh = UdpSocket::bind(loopback()).expect("new socket");
        let new_local = fresh.local_addr().expect("addr");
        assert_ne!(new_local, w.ctrl_ep.local_addr().expect("addr"));
        w.ctrl_ep.rebind(fresh).expect("rebind");

        // Traffic must keep flowing, both ways, with no reconnection.
        round_trip(&w.controller, &w.host, b"after-migration").await;
        round_trip(&w.host, &w.controller, b"host-to-controller").await;

        // And the host must now see the new address — the session followed
        // rather than being re-established.
        let after = w.host.remote_address();
        assert_ne!(
            before, after,
            "the host should observe the controller's new address; \
             before={before} after={after}"
        );
        assert_eq!(
            after.port(),
            new_local.port(),
            "the host should see the port the controller moved to"
        );

        // The peer identity is unchanged: migration moves the address, not the
        // authentication. A migration that re-opened the question of who the
        // peer is would be a vulnerability, not a feature.
        assert_eq!(w.host.peer_key(), &ctrl_id.public_key());
        assert_eq!(w.controller.peer_key(), &host_id.public_key());
    })
    .await;
}

/// Forward secrecy, observed rather than asserted.
///
/// The claim is that compromising a device's long-lived key does not decrypt
/// recorded past sessions. That holds because TLS 1.3 derives traffic keys from
/// an **ephemeral** X25519MLKEM768 exchange; the static Ed25519 key only signs
/// the handshake.
///
/// The observable consequence: two sessions between the *same* two identities
/// must produce *different* keying material. If the session keys were derived
/// from the static keys — the situation in which a leaked static key would
/// decrypt everything — the exported secrets would be identical, because every
/// input would be.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn session_keys_are_not_derived_from_the_device_keys() {
    bounded("forward secrecy", async {
        let host_id = Identity::generate().expect("host");
        let ctrl_id = Identity::generate().expect("controller");

        let mut exported: Vec<[u8; 32]> = Vec::new();

        // Three sessions, same two identities, same code path.
        for _ in 0..3 {
            init();
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
                let _ = Session::open(conn.clone(), Hello::new(Role::Host, hk, "host"))
                    .await
                    .map_err(|(e, _)| e)
                    .expect("host session");
                conn
            };
            let ctrl_side = async {
                let conn = ctrl_ep.connect(addr).await.expect("connect");
                let _ = Session::open(conn.clone(), Hello::new(Role::Controller, ck, "controller"))
                    .await
                    .map_err(|(e, _)| e)
                    .expect("controller session");
                conn
            };
            let (host_conn, ctrl_conn) = tokio::join!(host_side, ctrl_side);

            let mut a = [0u8; 32];
            let mut b = [0u8; 32];
            host_conn
                .export_keying_material(&mut a, b"esper forward secrecy test", &[])
                .expect("export on the host");
            ctrl_conn
                .export_keying_material(&mut b, b"esper forward secrecy test", &[])
                .expect("export on the controller");

            // Sanity: both ends of one session agree, or the test is measuring
            // noise rather than the exchange.
            assert_eq!(a, b, "both ends of a session must export the same secret");
            assert_ne!(a, [0u8; 32], "the exported secret must not be all zeros");

            exported.push(a);
            host_conn.close(0u32.into(), b"done");
            ctrl_conn.close(0u32.into(), b"done");
            tokio::join!(host_ep.wait_idle(), ctrl_ep.wait_idle());
        }

        // The property. Identical static keys, different session keys.
        for (i, x) in exported.iter().enumerate() {
            for (j, y) in exported.iter().enumerate() {
                if i < j {
                    assert_ne!(
                        x, y,
                        "sessions {i} and {j} share the same keying material despite being \
                         separate sessions — session keys would then be a function of the \
                         static device keys, and leaking one would decrypt every recorded \
                         session"
                    );
                }
            }
        }
    })
    .await;
}

/// **A dial to nothing gives up rather than waiting out the idle timeout.**
///
/// An idle *session* may legitimately sit for thirty seconds; a handshake that
/// has had no answer is not going to get one. Before this the two shared a
/// deadline, so a mistyped address bought thirty seconds of silence — which is
/// how a working product feels broken.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn a_dial_with_no_answer_gives_up_on_its_own_deadline() {
    init();
    let id = Identity::generate().expect("identity");
    let endpoint = Endpoint::bind(
        &id,
        (Ipv4Addr::LOCALHOST, 0).into(),
        Arc::new(NoPeers),
        Arc::new(AnyOf::new(vec![id.public_key()])),
    )
    .expect("endpoint");

    // A plain UDP socket, held open and never answering: exactly "nothing is
    // listening for Esper here", with no race over whether a port is free.
    let silent = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("a socket");
    let addr = silent.local_addr().expect("its address");

    let wait = Duration::from_millis(250);
    let started = std::time::Instant::now();
    let err = endpoint
        .connect_within(addr, wait)
        .await
        .expect_err("nothing is there to answer");

    match err {
        esper_transport::TransportError::NoAnswer { addr: a, waited } => {
            assert_eq!(a, addr);
            assert_eq!(waited, wait);
        }
        other => panic!("expected to give up on the deadline, got {other:?}"),
    }
    // The deadline, not the idle timeout. Generously bounded so a loaded
    // machine does not fail it, and far below the thirty seconds it was.
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "gave up after {:?}, which is not the deadline it was given",
        started.elapsed()
    );
}
