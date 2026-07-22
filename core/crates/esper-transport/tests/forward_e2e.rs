// Integration tests. Test code throughout.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A forwarded TCP connection, over a real session.
//!
//! The unit tests prove the allowlist decides correctly. This proves the other
//! half: that bytes actually arrive, in both directions, through a QUIC stream
//! that stopped being framed after its first message — which is where a
//! forward would break silently if the leftover bytes of that first read were
//! dropped, or if half-close were not passed through.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Once};
use std::time::Duration;

use esper_crypto::Identity;
use esper_proto::{ForwardOpen, ForwardRefusal, Hello, Role};
use esper_transport::forward::{self, Allowlist};
use esper_transport::{AnyOf, Endpoint, NoPeers, OnlyPeer, Session};
use std::net::UdpSocket;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

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

/// Two paired sessions, host and controller, with the endpoints they run on.
///
/// The endpoints come back because they have to be held: dropping one closes
/// the socket underneath its session.
async fn sessions() -> (Endpoint, Endpoint, Session, Session) {
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
    (host_ep, ctrl_ep, host, controller)
}

/// A target that answers every connection with the bytes it was sent, upper-cased.
///
/// Not an echo: a reply identical to the request would pass even if the two
/// directions were crossed.
async fn shouting_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut got = Vec::new();
                // Reads until the peer half-closes, which is what makes this a
                // test of half-close as well as of copying.
                let _ = sock.read_to_end(&mut got).await;
                let _ = sock.write_all(&got.to_ascii_uppercase()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    port
}

/// The host's side of one forwarded connection.
///
/// Borrows the session rather than taking it. **Dropping a `Session` closes
/// the connection, and closing a QUIC connection discards whatever is still in
/// flight** — so a host that let its session end the moment its own copying
/// finished would throw away the last bytes the controller had not read yet.
/// The engine does not have this problem, because there the session outlives
/// every forward on it; a test that took ownership would have invented it.
async fn host_forwards(session: &Session, allow: Allowlist) {
    let mut control = session.accept_control().await.expect("a forward stream");
    let open = forward::requested(&mut control).await.expect("the request");
    match allow.dial(&open).await {
        Ok(mut target) => {
            let stream = forward::accepted(control).await.expect("accepting");
            stream.pump(&mut target).await.expect("pumping");
        }
        Err(why) => forward::refuse(control, why).await.expect("refusing"),
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn bytes_go_both_ways_through_a_forward() {
    bounded("a forwarded connection", async {
        let port = shouting_server().await;
        let (_host_ep, _ctrl_ep, host, controller) = sessions().await;

        let host = Arc::new(host);
        let serving = tokio::spawn({
            let host = Arc::clone(&host);
            async move { host_forwards(&host, Allowlist::loopback_only()).await }
        });

        let control = controller.open_control().await.expect("a stream");
        let stream = forward::open(
            control,
            &ForwardOpen {
                host: "127.0.0.1".to_owned(),
                port,
            },
        )
        .await
        .expect("the host to open it");

        // The local end of the forward, driven from this test rather than
        // through a listener: what a listener would hand `pump` is a socket,
        // and this is a socket.
        let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let echo_addr = echo.local_addr().unwrap();
        let client = tokio::spawn(async move {
            let mut c = TcpStream::connect(echo_addr).await.unwrap();
            c.write_all(b"hello through the tunnel").await.unwrap();
            c.shutdown().await.unwrap();
            let mut back = Vec::new();
            c.read_to_end(&mut back).await.unwrap();
            back
        });
        let (mut local, _) = echo.accept().await.unwrap();
        let (sent, received) = stream.pump(&mut local).await.expect("pumping");

        let back = client.await.unwrap();
        assert_eq!(back, b"HELLO THROUGH THE TUNNEL".to_vec());
        assert_eq!(sent, 24, "the request went out once");
        assert_eq!(received, 24, "and the reply came back once");
        serving.await.unwrap();
    })
    .await;
}

#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn a_target_the_host_will_not_reach_comes_back_as_a_refusal() {
    bounded("a refused forward", async {
        let port = shouting_server().await;
        let (_host_ep, _ctrl_ep, host, controller) = sessions().await;

        // Loopback is where the target is, and this list does not name it.
        let allow = Allowlist::parse("db.internal:5432").unwrap();
        let host = Arc::new(host);
        let serving = tokio::spawn({
            let host = Arc::clone(&host);
            async move { host_forwards(&host, allow).await }
        });

        let control = controller.open_control().await.expect("a stream");
        let refused = forward::open(
            control,
            &ForwardOpen {
                host: "127.0.0.1".to_owned(),
                port,
            },
        )
        .await
        .expect_err("the host must refuse this");

        match refused {
            forward::ForwardError::Refused(why) => assert_eq!(why, ForwardRefusal::NotAllowed),
            other => panic!("expected a refusal, got {other}"),
        }
        serving.await.unwrap();
    })
    .await;
}

#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn several_connections_through_one_session_do_not_cross() {
    bounded("three forwarded connections", async {
        let port = shouting_server().await;
        let (_host_ep, _ctrl_ep, host, controller) = sessions().await;
        let allow = Allowlist::loopback_only();

        // The host serves whatever streams turn up, for as long as they do.
        let serving = tokio::spawn(async move {
            loop {
                let Ok(mut control) = host.accept_control().await else {
                    return;
                };
                let allow = allow.clone();
                tokio::spawn(async move {
                    let open = forward::requested(&mut control).await.expect("a request");
                    let mut target = allow.dial(&open).await.expect("dialling");
                    let stream = forward::accepted(control).await.expect("accepting");
                    stream.pump(&mut target).await.expect("pumping");
                });
            }
        });

        let mut running = Vec::new();
        for n in 0..3u8 {
            let control = controller.open_control().await.expect("a stream");
            let stream = forward::open(
                control,
                &ForwardOpen {
                    host: "127.0.0.1".to_owned(),
                    port,
                },
            )
            .await
            .expect("opened");

            // Each connection sends something only it sends, so a reply that
            // came back on the wrong stream is a different string rather than
            // an indistinguishable one.
            let mine = format!("connection-{n}");
            running.push(tokio::spawn(async move {
                let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let addr = echo.local_addr().unwrap();
                let client = tokio::spawn(async move {
                    let mut c = TcpStream::connect(addr).await.unwrap();
                    c.write_all(mine.as_bytes()).await.unwrap();
                    c.shutdown().await.unwrap();
                    let mut back = Vec::new();
                    c.read_to_end(&mut back).await.unwrap();
                    (mine, back)
                });
                let (mut local, _) = echo.accept().await.unwrap();
                stream.pump(&mut local).await.expect("pumping");
                client.await.unwrap()
            }));
        }

        for task in running {
            let (sent, back) = task.await.unwrap();
            assert_eq!(
                String::from_utf8(back).unwrap(),
                sent.to_uppercase(),
                "a reply came back on the wrong connection"
            );
        }
        serving.abort();
    })
    .await;
}

/// A target that answers each line with the same line, upper-cased.
///
/// Line at a time rather than to-EOF, so a test can talk to it across an
/// event — like the controller's address changing under the session.
async fn shouting_lines() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let (read, mut write) = sock.into_split();
                let mut lines = tokio::io::BufReader::new(read);
                let mut line = String::new();
                loop {
                    line.clear();
                    match tokio::io::AsyncBufReadExt::read_line(&mut lines, &mut line).await {
                        Ok(0) | Err(_) => return,
                        Ok(_) => {}
                    }
                    if write
                        .write_all(line.to_uppercase().as_bytes())
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            });
        }
    });
    port
}

/// A forwarded connection survives the controller changing its local address.
///
/// The session migrating is QUIC's property and is tested on its own. This is
/// the part that is not automatic: a forward is a *stream* with payload in
/// flight and a copy loop on each side, and either of those noticing the
/// address change would end somebody's database connection when their laptop
/// moved from wifi to a tether — the exact moment the product is supposed to
/// be invisible.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs, which is C; MIRI cannot call it")]
async fn a_forwarded_connection_survives_the_controller_moving() {
    bounded("a migrating forward", async {
        let port = shouting_lines().await;
        let (_host_ep, ctrl_ep, host, controller) = sessions().await;

        let host = Arc::new(host);
        let serving = tokio::spawn({
            let host = Arc::clone(&host);
            async move { host_forwards(&host, Allowlist::loopback_only()).await }
        });

        let control = controller.open_control().await.expect("a stream");
        let stream = forward::open(
            control,
            &ForwardOpen {
                host: "127.0.0.1".to_owned(),
                port,
            },
        )
        .await
        .expect("opened");

        let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let echo_addr = echo.local_addr().unwrap();
        let client = tokio::spawn(async move {
            let stream = TcpStream::connect(echo_addr).await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = tokio::io::BufReader::new(read);
            let mut back = String::new();

            write.write_all(b"before\n").await.unwrap();
            tokio::io::AsyncBufReadExt::read_line(&mut lines, &mut back)
                .await
                .unwrap();
            assert_eq!(back, "BEFORE\n");

            // Sent, then the address moves under it, then the answer is read.
            write.write_all(b"across\n").await.unwrap();
            (write, lines)
        });
        let (mut local, _) = echo.accept().await.unwrap();
        let pumping = tokio::spawn(async move { stream.pump(&mut local).await });

        let (mut write, mut lines) = client.await.unwrap();

        // The controller moves, as a NAT rebinding or a change of network
        // would move it.
        let was = host.remote_address();
        let fresh = UdpSocket::bind(loopback()).expect("a new socket");
        ctrl_ep.rebind(fresh).expect("rebind");

        let mut back = String::new();
        tokio::io::AsyncBufReadExt::read_line(&mut lines, &mut back)
            .await
            .unwrap();
        assert_eq!(back, "ACROSS\n", "the byte in flight did not arrive");

        // And the connection is still a connection afterwards, not a corpse
        // that happened to deliver one more line.
        write.write_all(b"after\n").await.unwrap();
        back.clear();
        tokio::io::AsyncBufReadExt::read_line(&mut lines, &mut back)
            .await
            .unwrap();
        assert_eq!(back, "AFTER\n");

        // And the move actually happened. Without this the test would pass
        // just as happily against a rebind that never took effect, which is
        // the failure mode of every migration test that only checks that
        // traffic still flows.
        assert_ne!(
            host.remote_address(),
            was,
            "the controller never moved, so nothing was tested"
        );

        drop(write);
        drop(lines);
        pumping.await.unwrap().expect("pumping");
        serving.await.unwrap();
    })
    .await;
}
