// Integration tests. Test code throughout.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Agreeing what a session may do, over a real connection.
//!
//! Pairing establishes identity and grants nothing. What a paired device may
//! actually do is settled here, before anything starts — and the point of
//! testing it over a real control stream rather than against the policy
//! directly is that the *answer must arrive*. A host that decided silently
//! leaves the controller inferring its permissions from what shows up, which
//! is how a client ends up concluding a session is view-only because no
//! keystroke had any effect.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Once};
use std::time::Duration;

use esper_crypto::{Device, Identity};
use esper_proto::{Hello, Permission, Reason, Role, SessionRequest};
use esper_transport::{
    Answered, AnyOf, AuthorizeError, Control, Endpoint, GrantStoredPermissions, NoPeers, OnlyPeer,
    RefuseAll, Session, answer_session, answer_session_within, refuse_session,
    refuse_session_within, request_session,
};

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

/// One session, and everything holding it up.
///
/// The endpoints and the sessions are fields rather than locals dropped at the
/// end of `wire` because **dropping a [`Session`] closes its connection**. A
/// test that let them go would be racing that close, and would pass only for
/// as long as it did everything inside a single poll — which is exactly the
/// kind of test that starts failing the day somebody adds a `sleep`.
struct Wire {
    _host_ep: Endpoint,
    _ctrl_ep: Endpoint,
    _host_session: Session,
    _ctrl_session: Session,
    host: Control,
    ctrl: Control,
}

/// Two endpoints and the control stream on each side of one session.
async fn wire() -> Wire {
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
        let mut s = Session::open(conn, Hello::new(Role::Host, hk, "host"))
            .await
            .map_err(|(e, _)| e)
            .expect("host session");
        let control = s.take_control().expect("host control");
        (s, control)
    };
    let ctrl_side = async {
        let conn = ctrl_ep.connect(addr).await.expect("connect");
        let mut s = Session::open(conn, Hello::new(Role::Controller, ck, "controller"))
            .await
            .map_err(|(e, _)| e)
            .expect("controller session");
        let control = s.take_control().expect("controller control");
        (s, control)
    };
    let ((host_session, host), (ctrl_session, ctrl)) = tokio::join!(host_side, ctrl_side);
    Wire {
        _host_ep: host_ep,
        _ctrl_ep: ctrl_ep,
        _host_session: host_session,
        _ctrl_session: ctrl_session,
        host,
        ctrl,
    }
}

fn device(perms: &[Permission]) -> Device {
    let mut d = Device::new([7u8; 32], "laptop".to_owned(), 0);
    d.permissions = perms.to_vec();
    d
}

fn asking_for(want: &[Permission]) -> SessionRequest {
    SessionRequest {
        kind: esper_proto::SessionKind::Screen,
        want: want.to_vec(),
        codecs: vec!["h264".to_owned()],
        reason: "helping with email".to_owned(),
    }
}

/// The grant may be smaller than the request, and the controller is told which.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn a_controller_is_told_the_permissions_it_actually_holds() {
    bounded("a smaller grant", async {
        let mut w = wire().await;
        let device = device(&[Permission::View]);

        let host = async {
            answer_session(
                &mut w.host,
                std::sync::Arc::new(GrantStoredPermissions),
                &device,
                [1u8; 16],
                &[esper_proto::Codec::H264],
            )
            .await
            .expect("the host must answer")
        };
        let controller = async {
            request_session(
                &mut w.ctrl,
                &asking_for(&[Permission::View, Permission::Control]),
            )
            .await
            .expect("a device holding view must get a session")
        };
        let (answered, grant) = tokio::join!(host, controller);

        assert_eq!(grant.granted, vec![Permission::View]);
        assert!(
            !grant.allows(Permission::Control),
            "control was asked for and is not held, so it must not come back"
        );
        assert_eq!(grant.session_id, [1u8; 16]);
        match answered {
            Answered::Granted { grant: g, kind } => {
                assert_eq!(g.granted, grant.granted);
                assert_eq!(kind, esper_proto::SessionKind::Screen);
            }
            other => panic!("the host thought it {other:?} while the controller was granted"),
        }
    })
    .await;
}

/// A refusal reaches the controller as a refusal, with the reason on it.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn a_refused_session_says_why() {
    bounded("a refusal", async {
        let mut w = wire().await;
        let device = device(&[Permission::View]);

        let host = async {
            answer_session(
                &mut w.host,
                std::sync::Arc::new(RefuseAll),
                &device,
                [2u8; 16],
                &[esper_proto::Codec::H264],
            )
            .await
            .expect("the host must answer")
        };
        let controller = async {
            request_session(&mut w.ctrl, &asking_for(&[Permission::View]))
                .await
                .expect_err("RefuseAll refuses")
        };
        let (answered, err) = tokio::join!(host, controller);

        match err {
            AuthorizeError::Refused(d) => {
                assert_eq!(d.reason, Reason::HOST_DISABLED);
                assert!(!d.detail.is_empty(), "a refusal without a sentence");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert!(matches!(answered, Answered::Denied(_)), "{answered:?}");
    })
    .await;
}

/// Asking for only what you do not hold is denied — and answered.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn asking_for_only_what_you_do_not_hold_is_denied_out_loud() {
    bounded("nothing held", async {
        let mut w = wire().await;
        let device = device(&[Permission::View]);

        let host = async {
            answer_session(
                &mut w.host,
                std::sync::Arc::new(GrantStoredPermissions),
                &device,
                [3u8; 16],
                &[esper_proto::Codec::H264],
            )
            .await
            .expect("the host must answer")
        };
        let controller = async {
            request_session(&mut w.ctrl, &asking_for(&[Permission::Terminal]))
                .await
                .expect_err("terminal is not held")
        };
        let (_, err) = tokio::join!(host, controller);

        match err {
            AuthorizeError::Refused(d) => assert_eq!(d.reason, Reason::PERMISSION_DENIED),
            other => panic!("expected a refusal, got {other:?}"),
        }
    })
    .await;
}

/// A request outside its bounds is refused, not dropped.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn a_malformed_request_is_refused_rather_than_dropped() {
    bounded("malformed", async {
        let mut w = wire().await;
        let device = device(&[Permission::View]);

        // A control character in the reason, which is shown on a host's
        // prompt: a peer is authenticated but an authenticated peer is not a
        // trusted one.
        let mut bad = asking_for(&[Permission::View]);
        bad.reason = "helping\u{7}with email".to_owned();

        let host = async {
            answer_session(
                &mut w.host,
                std::sync::Arc::new(GrantStoredPermissions),
                &device,
                [4u8; 16],
                &[esper_proto::Codec::H264],
            )
            .await
            .expect("the host must answer")
        };
        let controller = async {
            request_session(&mut w.ctrl, &bad)
                .await
                .expect_err("a malformed request is not a session")
        };
        let (answered, err) = tokio::join!(host, controller);

        match err {
            AuthorizeError::Refused(d) => assert_eq!(d.reason, Reason::MALFORMED_FRAME),
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert!(matches!(answered, Answered::Denied(_)), "{answered:?}");
    })
    .await;
}

/// A peer that authenticates and then says nothing does not hold the host.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn a_silent_controller_is_given_up_on() {
    bounded("silence", async {
        let mut w = wire().await;
        let device = device(&[Permission::View]);

        // The controller is connected and simply never asks. Tested with a
        // short wait rather than the shipping one, which is why the wait is an
        // argument.
        let wait = Duration::from_millis(150);
        let err = answer_session_within(
            &mut w.host,
            std::sync::Arc::new(GrantStoredPermissions),
            &device,
            [5u8; 16],
            wait,
            &[esper_proto::Codec::H264],
        )
        .await
        .expect_err("silence is not a session");

        match err {
            AuthorizeError::Silent(d) => assert_eq!(d, wait),
            other => panic!("expected to give up, got {other:?}"),
        }
    })
    .await;
}

/// **A device the host does not know is told so.**
///
/// TLS 1.3 finishes the client's handshake before the server verifies the
/// client's certificate, so a controller whose key is not paired sees
/// `connect()` succeed and discovers the truth when its first stream fails.
/// Reported as a stream failure that reads "connection lost", which is what
/// sends somebody to check their router about a device that is simply not
/// paired — and, until this test existed, what actually happened: the
/// classification was there and nothing reached it.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn a_key_the_host_does_not_know_is_refused_as_a_refusal() {
    bounded("an unknown key", async {
        init();
        let host_id = Identity::generate().expect("host");
        let ctrl_id = Identity::generate().expect("controller");

        // A host that authorises nobody: the same answer a host gives after
        // the device it knew was forgotten.
        let host_ep = Endpoint::bind(
            &host_id,
            loopback(),
            Arc::new(AnyOf::new(Vec::new())),
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

        // The host accepts and immediately finds the key unauthorised, which
        // is a TLS alert rather than anything this side sends.
        let hk = host_id.public_key();
        let host_side = tokio::spawn(async move {
            let Some(Ok(conn)) = host_ep.accept().await else {
                return;
            };
            let _ = Session::open(conn, Hello::new(Role::Host, hk, "host")).await;
        });

        let ck = ctrl_id.public_key();
        let err = match ctrl_ep.connect(addr).await {
            // The dial itself may carry the alert, which is also a refusal.
            Err(e) => esper_transport::SessionError::from(e),
            Ok(conn) => {
                Session::open(conn, Hello::new(Role::Controller, ck, "controller"))
                    .await
                    .map(|_| ())
                    .expect_err("a host that authorises nobody must not open a session")
                    .0
            }
        };

        assert!(
            matches!(err, esper_transport::SessionError::Refused(_)),
            "an unpaired device must be told it was refused, not that the \
             network broke: got {err:?}"
        );
        host_side.abort();
    })
    .await;
}

/// **A device turned away is told why, and the telling survives the host
/// letting the stream go.**
///
/// This is the shape of a host that is already busy: it refuses without asking
/// anybody anything, and it gets there *before* the controller has sent its
/// request. A refusal written into a stream whose request had never been read
/// reached the far end as a failed write rather than as a reason — the host
/// stops reading the moment it lets the stream go, and what fails is the
/// controller's own send. Which makes a busy host indistinguishable from a
/// broken one, and that confusion is the entire thing the reason exists to
/// end.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn a_refusal_reaches_a_controller_that_had_not_asked_yet() {
    bounded("a refusal that arrives first", async {
        let mut w = wire().await;
        let mut letting_go = w.host;

        let host = async {
            let deny = refuse_session(
                &mut letting_go,
                Reason::HOST_IS_BUSY,
                "this host serves one session at a time and is serving another",
            )
            .await
            .expect("a host must be able to refuse");
            // What the caller does: the stream is let go of, and the
            // connection is held open a moment longer so what was written on
            // it arrives.
            drop(letting_go);
            tokio::time::sleep(Duration::from_millis(500)).await;
            deny
        };
        let controller = async {
            // Long enough that the host is already inside its refusal.
            tokio::time::sleep(Duration::from_millis(200)).await;
            request_session(&mut w.ctrl, &asking_for(&[Permission::View])).await
        };

        let (sent, got) = tokio::join!(host, controller);
        assert_eq!(sent.reason, Reason::HOST_IS_BUSY);
        match got {
            Err(AuthorizeError::Refused(deny)) => {
                assert_eq!(deny.reason, Reason::HOST_IS_BUSY);
                assert_eq!(
                    deny.detail,
                    "this host serves one session at a time and is serving another"
                );
            }
            other => panic!(
                "a controller turned away must be told why, not left with a \
                 broken stream: got {other:?}"
            ),
        }
    })
    .await;
}

/// And a refusal does not wait for ever for a request that is never coming.
///
/// A peer that opens a connection and then says nothing would otherwise hold
/// the task trying to get rid of it — which is the thing a host that is
/// already busy can least afford to spend.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn a_refusal_gives_up_on_a_peer_that_never_asks() {
    bounded("a refusal with nobody asking", async {
        let mut w = wire().await;
        let err = refuse_session_within(
            &mut w.host,
            Reason::HOST_IS_BUSY,
            "serving somebody else",
            Duration::from_millis(200),
        )
        .await
        .expect_err("a peer that asks for nothing cannot be answered");
        assert!(
            matches!(err, AuthorizeError::Silent(_)),
            "the wait running out is what happened: got {err:?}"
        );
    })
    .await;
}
