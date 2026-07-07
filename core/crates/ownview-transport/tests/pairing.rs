// Integration tests. Test code throughout.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(miri, allow(unused))]

//! Pairing over a real connection.
//!
//! Two endpoints, real sockets, a real TLS handshake, a real SPAKE2 exchange.
//! The properties under test — that a wrong code fails, that a completed
//! pairing cannot be relayed, that the attempt budget is enforced — are
//! properties of the interaction, not of any one function.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use ownview_crypto::pairing::Offer;
use ownview_crypto::{Identity, PairingCode};
use ownview_proto::{Hello, Role};
use ownview_transport::{Control, Endpoint, NoPeers, OnlyPeer, Session, pair, pairing_authority};

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

/// Placeholder for the case where a connection was never established.
#[allow(non_snake_case)]
fn None_conn() -> Option<quinn::Connection> {
    None
}

/// The outcome of a pairing attempt from both sides.
struct Outcome {
    host: Result<[u8; 32], String>,
    controller: Result<[u8; 32], String>,
}

/// Drive a full pairing: connect, open a session, then pair on its control
/// channel.
///
/// The host uses the pairing authority (accepts any well-formed key), because
/// at first contact neither side has the other's key — the code does that job,
/// inside the PAKE.
async fn pair_once(
    host_id: &Identity,
    controller_id: &Identity,
    offer: &mut Offer,
    typed: &PairingCode,
    now: Instant,
) -> Outcome {
    let host_ep = Endpoint::bind(host_id, loopback(), pairing_authority(), Arc::new(NoPeers))
        .expect("host endpoint");
    let addr = host_ep.local_addr().expect("addr");

    // The controller does know which host it is dialling — the invite or the
    // discovery record named it. Only the *host* accepts an unknown key.
    let ctrl_ep = Endpoint::bind(
        controller_id,
        loopback(),
        Arc::new(NoPeers),
        Arc::new(OnlyPeer::new(host_id.public_key())),
    )
    .expect("controller endpoint");

    let hk = host_id.public_key();
    let ck = controller_id.public_key();

    // Each side returns its outcome *and* its connection, so teardown can be
    // sequenced after both have finished. An inner Result keeps `?` usable.
    let host_side = async {
        let step = async {
            let conn = host_ep
                .accept()
                .await
                .ok_or_else(|| "no incoming".to_owned())?
                .map_err(|e| e.to_string())?;
            let mut sess = Session::open(conn.clone(), Hello::new(Role::Host, hk, "host"))
                .await
                .map_err(|(e, _)| e.to_string())?;
            let mut ctl: Control = sess
                .take_control()
                .ok_or_else(|| "no control channel".to_owned())?;
            let out = pair::host(&conn, &mut ctl, host_id, offer, now)
                .await
                .map_err(|e| e.to_string());
            Ok::<_, String>((out, Some(conn)))
        };
        match step.await {
            Ok(v) => v,
            Err(e) => (Err(e), None_conn()),
        }
    };

    let ctrl_side = async {
        let step = async {
            let conn = ctrl_ep.connect(addr).await.map_err(|e| e.to_string())?;
            let mut sess =
                Session::open(conn.clone(), Hello::new(Role::Controller, ck, "controller"))
                    .await
                    .map_err(|(e, _)| e.to_string())?;
            let mut ctl: Control = sess
                .take_control()
                .ok_or_else(|| "no control channel".to_owned())?;
            let out = pair::controller(&conn, &mut ctl, controller_id, typed)
                .await
                .map_err(|e| e.to_string());
            Ok::<_, String>((out, Some(conn)))
        };
        match step.await {
            Ok(v) => v,
            Err(e) => (Err(e), None_conn()),
        }
    };

    let ((host, host_conn), (controller, ctrl_conn)) = tokio::join!(host_side, ctrl_side);

    // Only now, with both sides finished, wind the connections down.
    for c in [host_conn, ctrl_conn].into_iter().flatten() {
        c.close(0u32.into(), b"done");
    }
    tokio::join!(host_ep.wait_idle(), ctrl_ep.wait_idle());
    Outcome {
        host: host.map_err(|e: String| e),
        controller: controller.map_err(|e: String| e),
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn two_processes_pair_with_a_typed_code() {
    bounded("two_processes_pair_with_a_typed_code", async {
        init();
        let host_id = Identity::generate().expect("host");
        let ctrl_id = Identity::generate().expect("controller");
        let mut offer = Offer::new();

        // What the user actually does: reads the code off the screen and types
        // it, lowercase, with the separator.
        let typed = PairingCode::parse(&offer.code().display().to_lowercase())
            .expect("a displayed code must parse");

        let out = pair_once(&host_id, &ctrl_id, &mut offer, &typed, Instant::now()).await;

        let host_learned = out.host.expect("host should pair");
        let ctrl_learned = out.controller.expect("controller should pair");

        assert_eq!(
            host_learned,
            ctrl_id.public_key(),
            "host learned the controller's key"
        );
        assert_eq!(
            ctrl_learned,
            host_id.public_key(),
            "controller learned the host's key"
        );
    })
    .await;
}

#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn a_wrong_code_fails_on_both_sides_and_spends_an_attempt() {
    bounded("a_wrong_code_fails", async {
        init();
        let host_id = Identity::generate().expect("host");
        let ctrl_id = Identity::generate().expect("controller");
        let mut offer = Offer::new();

        let wrong = loop {
            let c = PairingCode::generate();
            if &c != offer.code() {
                break c;
            }
        };

        let out = pair_once(&host_id, &ctrl_id, &mut offer, &wrong, Instant::now()).await;

        assert!(out.host.is_err(), "host must not pair on a wrong code");
        assert!(out.controller.is_err(), "controller must not pair either");
        assert_eq!(
            offer.attempts_remaining(),
            ownview_crypto::MAX_ATTEMPTS - 1,
            "a failed attempt must be spent"
        );

        // The controller must be told what happened, without being told how
        // close it was.
        let msg = out.controller.unwrap_err();
        assert!(
            msg.contains("did not match") || msg.contains("interfering"),
            "the controller should learn the code was refused: {msg}"
        );
    })
    .await;
}

#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn an_expired_offer_is_refused_and_says_so() {
    bounded("an_expired_offer", async {
        init();
        let host_id = Identity::generate().expect("host");
        let ctrl_id = Identity::generate().expect("controller");

        let created = Instant::now();
        let code = PairingCode::generate();
        let mut offer = Offer::with_created_at(code.clone(), created);
        let too_late = created + ownview_crypto::PAIRING_TTL + Duration::from_secs(1);

        let out = pair_once(&host_id, &ctrl_id, &mut offer, &code, too_late).await;

        assert!(out.host.is_err(), "an expired offer must not pair");
        let msg = out.controller.unwrap_err();
        assert!(
            msg.contains("expired"),
            "the controller should be told the code expired rather than that it was wrong: {msg}"
        );
    })
    .await;
}

#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn the_attempt_budget_is_enforced_across_connections() {
    bounded("the_attempt_budget", async {
        init();
        let host_id = Identity::generate().expect("host");
        let mut offer = Offer::new();
        let wrong = loop {
            let c = PairingCode::generate();
            if &c != offer.code() {
                break c;
            }
        };
        let now = Instant::now();

        // Three wrong attempts, each on its own connection.
        for i in 0..ownview_crypto::MAX_ATTEMPTS {
            let ctrl_id = Identity::generate().expect("controller");
            let out = pair_once(&host_id, &ctrl_id, &mut offer, &wrong, now).await;
            assert!(out.host.is_err(), "attempt {i} should fail");
        }
        assert_eq!(offer.attempts_remaining(), 0);

        // The fourth is refused before any cryptography happens, and even the
        // *right* code cannot revive a dead offer.
        let ctrl_id = Identity::generate().expect("controller");
        let right = offer.code().clone();
        let out = pair_once(&host_id, &ctrl_id, &mut offer, &right, now).await;
        assert!(
            out.host.is_err(),
            "a dead offer must refuse the right code too"
        );
        let msg = out.controller.unwrap_err();
        assert!(
            msg.contains("too many times") || msg.contains("dead"),
            "the controller should learn the code is dead: {msg}"
        );
    })
    .await;
}

/// The pairing authority must never admit a key that could not authenticate.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn the_pairing_authority_still_refuses_unusable_keys() {
    use ownview_transport::PeerAuthority;
    let a = ownview_transport::AnyKeyForPairingOnly;

    let good = Identity::generate().expect("id").public_key();
    assert!(a.is_authorized(&good), "a real key must be accepted");

    let mut identity_point = [0u8; 32];
    identity_point[0] = 1;
    for weak in [[0u8; 32], identity_point] {
        assert!(
            !a.is_authorized(&weak),
            "a small-order key must not be pairable even in pairing mode"
        );
    }
    assert!(
        a.describe().contains("PAIRING ONLY"),
        "the description must warn what this authority is for"
    );
}
