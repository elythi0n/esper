// Integration tests for the transport. Test code throughout.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Two real endpoints, real sockets, real handshakes.
//!
//! Skipped under MIRI: the TLS provider is `aws-lc-rs`, which is C, and MIRI
//! interprets Rust rather than executing foreign functions. Nothing is lost —
//! MIRI exists to find undefined behavior in Rust, and there is no unsafe Rust
//! in this crate to find it in.
//!
//! These are not unit tests with a mocked transport. The properties being
//! checked — that an unpaired peer cannot open a session, that a changed key
//! cannot, that a refusal presents as silence rather than an error — are
//! properties of the interaction between rustls, quinn and our verifier. A mock
//! would assert that our own code calls itself.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Once};

use esper_crypto::{Identity, identity::KEY_LEN};
use esper_proto::{Hello, Role};
use esper_transport::{AnyOf, Endpoint, NoPeers, OnlyPeer, Session, SessionError};

/// Every test is bounded. A transport test that hangs is a bug in the transport,
/// and it should be reported in seconds rather than by exhausting a CI job's
/// patience — the first run of this file took ten minutes to say nothing,
/// because `accept_bi` had no deadline.
const TEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Run a test body under [`TEST_TIMEOUT`], failing with a clear message.
async fn bounded<F, T>(what: &str, f: F) -> T
where
    F: std::future::Future<Output = T>,
{
    match tokio::time::timeout(TEST_TIMEOUT, f).await {
        Ok(v) => v,
        Err(_) => panic!("{what} did not finish within {TEST_TIMEOUT:?} — something hangs"),
    }
}

/// rustls requires a process-wide crypto provider, installed once.
fn init() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}

fn loopback() -> SocketAddr {
    (Ipv4Addr::LOCALHOST, 0).into()
}

fn hello(role: Role, key: &[u8; KEY_LEN], name: &str) -> Hello {
    Hello::new(role, *key, name)
}

/// A host endpoint that accepts the given keys, and a controller that will only
/// talk to that host.
struct Pair {
    host_id: Identity,
    controller_id: Identity,
    host: Endpoint,
    controller: Endpoint,
    host_addr: SocketAddr,
}

impl Pair {
    fn build(host_accepts: Vec<[u8; KEY_LEN]>) -> Self {
        init();
        let host_id = Identity::generate().expect("host identity");
        let controller_id = Identity::generate().expect("controller identity");

        let host = Endpoint::bind(
            &host_id,
            loopback(),
            Arc::new(AnyOf::new(host_accepts)),
            Arc::new(NoPeers),
        )
        .expect("host endpoint");
        let host_addr = host.local_addr().expect("host addr");

        let controller = Endpoint::bind(
            &controller_id,
            loopback(),
            Arc::new(NoPeers),
            Arc::new(OnlyPeer::new(host_id.public_key())),
        )
        .expect("controller endpoint");

        Self {
            host_id,
            controller_id,
            host,
            controller,
            host_addr,
        }
    }
}

/// Drive both sides to a session, or report why not.
///
/// Runs the host's accept loop concurrently, because the exchange requires both
/// sides to act — which is the point: a peer the host refused never completes it.
async fn try_session(p: &Pair) -> Result<(Session, Session), SessionError> {
    let host_hello = hello(Role::Host, &p.host_id.public_key(), "test-host");
    let ctrl_hello = hello(
        Role::Controller,
        &p.controller_id.public_key(),
        "test-controller",
    );

    let host_side = async {
        let conn = p
            .host
            .accept()
            .await
            .ok_or(SessionError::NoHello)?
            // `from`, not the variant directly: the conversion is where a TLS
            // authentication alert is separated from a transport failure, and
            // constructing the variant by hand skips exactly that.
            .map_err(SessionError::from)?;
        Session::open(conn, host_hello).await.map_err(|(e, _)| e)
    };

    let controller_side = async {
        let conn = p
            .controller
            .connect(p.host_addr)
            .await
            .map_err(SessionError::from)?;
        Session::open(conn, ctrl_hello).await.map_err(|(e, _)| e)
    };

    let (h, c) = tokio::join!(host_side, controller_side);
    Ok((h?, c?))
}

#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn a_paired_peer_opens_a_session() {
    bounded("a_paired_peer_opens_a_session", async {
        let p = Pair::build(vec![]);
        // Rebuild the host now that we know the controller's key.
        let p = Pair {
            host: Endpoint::bind(
                &p.host_id,
                loopback(),
                Arc::new(AnyOf::new(vec![p.controller_id.public_key()])),
                Arc::new(NoPeers),
            )
            .expect("host endpoint"),
            ..p
        };
        let p = Pair {
            host_addr: p.host.local_addr().expect("addr"),
            ..p
        };

        let (host, controller) = try_session(&p).await.expect("session should open");

        assert_eq!(host.peer_key(), &p.controller_id.public_key());
        assert_eq!(controller.peer_key(), &p.host_id.public_key());
        assert!(host.is_host());
        assert!(!controller.is_host());
        assert_eq!(controller.peer_hello().name, "test-host");
        assert_eq!(host.peer_hello().name, "test-controller");

        host.close(esper_proto::Reason::IDLE_TIMEOUT);
        controller.close(esper_proto::Reason::IDLE_TIMEOUT);
    })
    .await;
}

/// The property that matters most. An unpaired key must not open a session.
///
/// Note what this asserts: not that `connect()` fails — it may well succeed,
/// because TLS 1.3 completes the client handshake before the server verifies the
/// client's certificate — but that no `Session` is produced.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn an_unpaired_peer_cannot_open_a_session() {
    bounded("an_unpaired_peer_cannot_open_a_session", async {
        // The host accepts a key belonging to nobody in this test.
        let stranger = Identity::generate().expect("stranger");
        let p = Pair::build(vec![stranger.public_key()]);

        let result = try_session(&p).await;
        assert!(
            result.is_err(),
            "an unpaired controller must not obtain a session"
        );
    })
    .await;
}

#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn a_host_with_no_paired_devices_accepts_nobody() {
    bounded("a_host_with_no_paired_devices_accepts_nobody", async {
        let p = Pair::build(vec![]);
        assert!(
            try_session(&p).await.is_err(),
            "a host with an empty allowlist must refuse everyone"
        );
    })
    .await;
}

/// A refusal presents as silence, not as a transport error, and the error type
/// says so. This is the case that would otherwise be retried in a loop.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn a_refusal_is_reported_as_authentication_not_as_a_network_problem() {
    bounded(
        "a_refusal_is_reported_as_authentication_not_as_a_network_problem",
        async {
            let p = Pair::build(vec![]);
            match try_session(&p).await {
                Err(e) => {
                    assert_eq!(
                        e.reason(),
                        esper_proto::Reason::UNKNOWN_PEER_KEY,
                        "a refusal must map to an authentication reason, got: {e}"
                    );
                    // **And it presents as the TLS alert, which is the
                    // unambiguous variant.**
                    //
                    // Worth pinning, because the reasoning around here used to
                    // say something else: that TLS 1.3 lets our handshake
                    // finish before the peer's verifier runs, so a refusal
                    // arrives as *silence*. Measured, it does not — it arrives
                    // as `Refused`, carrying the peer's own words.
                    //
                    // The distinction is the whole reason the timeout is now a
                    // variant of its own. Silence is ambiguous: a peer that
                    // refused us and a link that could not carry the exchange
                    // look identical, and a person accused of the first when
                    // it was the second re-pairs a device that was paired
                    // perfectly well. So a refusal must keep arriving as a
                    // refusal, and this is what notices if it stops.
                    assert!(
                        matches!(e, SessionError::Refused(_)),
                        "a refusal must arrive as itself, not as silence: {e:?}"
                    );
                    let msg = e.to_string();
                    assert!(
                        msg.contains("refused our device key") || msg.contains("not paired"),
                        "the message should point at authentication, got: {msg}"
                    );
                    assert!(
                        !msg.contains("tier"),
                        "a refusal must not be reported as a connectivity failure: {msg}"
                    );
                }
                Ok(_) => panic!("session should not have opened"),
            }
        },
    )
    .await;
}

/// A device whose key changed is a different device, and is refused. Never a
/// prompt with a "continue" button.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn a_changed_key_is_refused() {
    bounded("a_changed_key_is_refused", async {
        init();
        let host_id = Identity::generate().expect("host");
        let original = Identity::generate().expect("original");
        let replacement = Identity::generate().expect("replacement");

        // Host pairs with `original`.
        let host = Endpoint::bind(
            &host_id,
            loopback(),
            Arc::new(AnyOf::new(vec![original.public_key()])),
            Arc::new(NoPeers),
        )
        .expect("host");
        let addr = host.local_addr().expect("addr");

        // `replacement` dials, claiming to be a paired device.
        let controller = Endpoint::bind(
            &replacement,
            loopback(),
            Arc::new(NoPeers),
            Arc::new(OnlyPeer::new(host_id.public_key())),
        )
        .expect("controller");

        let host_side = async {
            let conn = host.accept().await.expect("incoming").ok();
            conn.map(|c| async move {
                Session::open(c, hello(Role::Host, &host_id.public_key(), "host")).await
            })
        };
        let ctrl_side = async {
            match controller.connect(addr).await {
                Ok(conn) => Session::open(
                    conn,
                    hello(Role::Controller, &replacement.public_key(), "impostor"),
                )
                .await
                .map(|_| ())
                .map_err(|(e, _)| e),
                Err(e) => Err(SessionError::Stream(e.to_string())),
            }
        };

        let (_, ctrl) = tokio::join!(host_side, ctrl_side);
        assert!(ctrl.is_err(), "a device with a changed key must be refused");
    })
    .await;
}

#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn media_datagrams_carry_payloads() {
    bounded("media_datagrams_carry_payloads", async {
        let host_id = Identity::generate().expect("host");
        init();
        let controller_id = Identity::generate().expect("controller");

        let host = Endpoint::bind(
            &host_id,
            loopback(),
            Arc::new(AnyOf::new(vec![controller_id.public_key()])),
            Arc::new(NoPeers),
        )
        .expect("host");
        let addr = host.local_addr().expect("addr");
        let controller = Endpoint::bind(
            &controller_id,
            loopback(),
            Arc::new(NoPeers),
            Arc::new(OnlyPeer::new(host_id.public_key())),
        )
        .expect("controller");

        let hk = host_id.public_key();
        let ck = controller_id.public_key();

        let host_side = async {
            let conn = host.accept().await.unwrap().unwrap();
            Session::open(conn, hello(Role::Host, &hk, "host"))
                .await
                .map_err(|(e, _)| e)
        };
        let ctrl_side = async {
            let conn = controller.connect(addr).await.unwrap();
            Session::open(conn, hello(Role::Controller, &ck, "controller"))
                .await
                .map_err(|(e, _)| e)
        };
        let (h, c) = tokio::join!(host_side, ctrl_side);
        let (h, c) = (h.expect("host session"), c.expect("controller session"));

        assert!(
            h.max_datagram_size().is_some(),
            "datagrams must be available; media depends on them"
        );

        h.send_media(bytes::Bytes::from_static(b"frame-0")).unwrap();
        let got = tokio::time::timeout(std::time::Duration::from_secs(5), c.recv_media())
            .await
            .expect("datagram should arrive")
            .expect("datagram");
        assert_eq!(&got[..], b"frame-0");

        // And in the other direction: input travels controller to host.
        c.send_media(bytes::Bytes::from_static(b"keypress"))
            .unwrap();
        let got = tokio::time::timeout(std::time::Duration::from_secs(5), h.recv_media())
            .await
            .expect("datagram should arrive")
            .expect("datagram");
        assert_eq!(&got[..], b"keypress");
    })
    .await;
}

#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn control_streams_and_bulk_streams_both_work() {
    bounded("control_streams_and_bulk_streams_both_work", async {
        init();
        let host_id = Identity::generate().expect("host");
        let controller_id = Identity::generate().expect("controller");

        let host = Endpoint::bind(
            &host_id,
            loopback(),
            Arc::new(AnyOf::new(vec![controller_id.public_key()])),
            Arc::new(NoPeers),
        )
        .expect("host");
        let addr = host.local_addr().expect("addr");
        let controller = Endpoint::bind(
            &controller_id,
            loopback(),
            Arc::new(NoPeers),
            Arc::new(OnlyPeer::new(host_id.public_key())),
        )
        .expect("controller");

        let hk = host_id.public_key();
        let ck = controller_id.public_key();

        let host_side = async {
            let conn = host.accept().await.unwrap().unwrap();
            Session::open(conn, hello(Role::Host, &hk, "host"))
                .await
                .map_err(|(e, _)| e)
        };
        let ctrl_side = async {
            let conn = controller.connect(addr).await.unwrap();
            Session::open(conn, hello(Role::Controller, &ck, "controller"))
                .await
                .map_err(|(e, _)| e)
        };
        let (h, c) = tokio::join!(host_side, ctrl_side);
        let (mut h, mut c) = (h.expect("host session"), c.expect("controller session"));

        // The Hello exchange's own stream is the session's control channel, and it
        // stays usable — that is the point of framing rather than read_to_end.
        let mut h_ctl = h.take_control().expect("host control channel");
        let mut c_ctl = c.take_control().expect("controller control channel");
        assert!(
            h.take_control().is_none(),
            "the control channel has one owner; taking it twice must not hand out a second"
        );

        // Several frames over one long-lived stream, in both directions.
        for i in 0..3u32 {
            c_ctl.send(&format!("ping-{i}")).await.expect("send ping");
            let got: String = h_ctl.recv().await.expect("recv ping");
            assert_eq!(got, format!("ping-{i}"));

            h_ctl.send(&format!("pong-{i}")).await.expect("send pong");
            let got: String = c_ctl.recv().await.expect("recv pong");
            assert_eq!(got, format!("pong-{i}"));
        }

        // An additional control stream, opened after the session is up.
        let extra = tokio::spawn(async move {
            let mut s = h.accept_control().await.expect("accept extra");
            let got: String = s.recv().await.expect("recv on extra");
            (h, got)
        });
        let mut c_extra = c.open_control().await.expect("open extra");
        c_extra
            .send(&"on-a-second-stream".to_owned())
            .await
            .expect("send");
        let (_h, got) = extra.await.expect("join");
        assert_eq!(got, "on-a-second-stream");

        // There was a bulk-stream leg here, for a file transfer that went
        // another way: `cp` opens a control stream of its own. It went with
        // the API it was testing — see the note in `session.rs`.
    })
    .await;
}

#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn stats_are_populated_after_traffic() {
    bounded("stats_are_populated_after_traffic", async {
        init();
        let host_id = Identity::generate().expect("host");
        let controller_id = Identity::generate().expect("controller");

        let host = Endpoint::bind(
            &host_id,
            loopback(),
            Arc::new(AnyOf::new(vec![controller_id.public_key()])),
            Arc::new(NoPeers),
        )
        .expect("host");
        let addr = host.local_addr().expect("addr");
        let controller = Endpoint::bind(
            &controller_id,
            loopback(),
            Arc::new(NoPeers),
            Arc::new(OnlyPeer::new(host_id.public_key())),
        )
        .expect("controller");

        let hk = host_id.public_key();
        let ck = controller_id.public_key();
        let host_side = async {
            let conn = host.accept().await.unwrap().unwrap();
            Session::open(conn, hello(Role::Host, &hk, "host"))
                .await
                .map_err(|(e, _)| e)
        };
        let ctrl_side = async {
            let conn = controller.connect(addr).await.unwrap();
            Session::open(conn, hello(Role::Controller, &ck, "controller"))
                .await
                .map_err(|(e, _)| e)
        };
        let (h, c) = tokio::join!(host_side, ctrl_side);
        let (_h, c) = (h.expect("host"), c.expect("controller"));

        let s = c.stats();
        assert!(s.sent_bytes > 0, "we sent a handshake at minimum");
        assert!(s.received_bytes > 0, "and received one");
        assert!(
            s.congestion_window > 0,
            "the congestion window bounds throughput and must be reported"
        );
    })
    .await;
}

#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn a_peer_that_lies_about_its_key_in_hello_is_refused() {
    bounded(
        "a_peer_that_lies_about_its_key_in_hello_is_refused",
        async {
            init();
            let host_id = Identity::generate().expect("host");
            let controller_id = Identity::generate().expect("controller");
            let someone_else = Identity::generate().expect("other");

            let host = Endpoint::bind(
                &host_id,
                loopback(),
                Arc::new(AnyOf::new(vec![controller_id.public_key()])),
                Arc::new(NoPeers),
            )
            .expect("host");
            let addr = host.local_addr().expect("addr");
            let controller = Endpoint::bind(
                &controller_id,
                loopback(),
                Arc::new(NoPeers),
                Arc::new(OnlyPeer::new(host_id.public_key())),
            )
            .expect("controller");

            let hk = host_id.public_key();
            let host_side = async {
                let conn = host.accept().await.unwrap().unwrap();
                Session::open(conn, hello(Role::Host, &hk, "host"))
                    .await
                    .map_err(|(e, _)| e)
            };
            // Authenticates as itself, announces someone else's key.
            let ctrl_side = async {
                let conn = controller.connect(addr).await.unwrap();
                Session::open(
                    conn,
                    hello(Role::Controller, &someone_else.public_key(), "liar"),
                )
                .await
                .map(|_| ())
                .map_err(|(e, _)| e)
            };

            let (h, _c) = tokio::join!(host_side, ctrl_side);
            match h {
                Err(SessionError::KeyMismatch) => {}
                other => panic!("expected KeyMismatch, got {other:?}"),
            }
        },
    )
    .await;
}

#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn both_sides_claiming_host_is_refused() {
    bounded("both_sides_claiming_host_is_refused", async {
        init();
        let a_id = Identity::generate().expect("a");
        let b_id = Identity::generate().expect("b");

        let a = Endpoint::bind(
            &a_id,
            loopback(),
            Arc::new(AnyOf::new(vec![b_id.public_key()])),
            Arc::new(NoPeers),
        )
        .expect("a");
        let addr = a.local_addr().expect("addr");
        let b = Endpoint::bind(
            &b_id,
            loopback(),
            Arc::new(NoPeers),
            Arc::new(OnlyPeer::new(a_id.public_key())),
        )
        .expect("b");

        let ak = a_id.public_key();
        let bk = b_id.public_key();
        let a_side = async {
            let conn = a.accept().await.unwrap().unwrap();
            Session::open(conn, hello(Role::Host, &ak, "a"))
                .await
                .map(|_| ())
                .map_err(|(e, _)| e)
        };
        // b dials but also claims Host, so it never opens the stream a is waiting
        // for — and a times out waiting for a Hello.
        let b_side = async {
            let conn = b.connect(addr).await.unwrap();
            Session::open(conn, hello(Role::Host, &bk, "b"))
                .await
                .map(|_| ())
                .map_err(|(e, _)| e)
        };

        let (ra, rb) = tokio::join!(a_side, b_side);
        assert!(
            ra.is_err() && rb.is_err(),
            "two hosts must not open a session: a={ra:?} b={rb:?}"
        );
    })
    .await;
}
