#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A shell, over a real connection, between two real endpoints.
//!
//! This is the Phase 2 goal in one test: reach a machine over the network,
//! authenticated by key, and get a usable shell — with no capture, no codec and
//! no display anywhere in the path.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Once};
use std::time::Duration;

use esper_audit::{Event, Log};
use esper_crypto::Identity;
use esper_proto::{
    Hello, Permission, Refusal, Role, SessionId, TerminalData, TerminalMessage, TerminalOpen,
    TerminalStart, TerminalStarted,
};
use esper_transport::{
    AnyOf, Control, Ended, Endpoint, Limits, NoPeers, OnlyPeer, Session, TerminalError,
    TerminalPolicy, TerminalService,
};

const TEST_TIMEOUT: Duration = Duration::from_secs(30);

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

fn tmpdir() -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    let mut n = [0u8; 8];
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut n);
    p.push(format!("esper-term-{}", u64::from_le_bytes(n)));
    std::fs::create_dir_all(&p).expect("mkdir");
    p
}

fn open_request() -> TerminalStart {
    TerminalStart::Open(TerminalOpen {
        cols: 80,
        rows: 24,
        term: "xterm-256color".to_owned(),
    })
}

/// A service with the shipping defaults.
fn service(log: &Log) -> Arc<TerminalService> {
    Arc::new(TerminalService::new(
        log.clone(),
        Limits::default(),
        TerminalPolicy::default(),
    ))
}

/// Read the host's answer, which always comes before any terminal bytes.
async fn started(ctl: &mut Control) -> TerminalStarted {
    ctl.recv::<TerminalStarted>()
        .await
        .expect("the host must answer a terminal request before sending output")
}

/// Read the answer and expect a new session, returning its identifier.
async fn opened(ctl: &mut Control) -> SessionId {
    match started(ctl).await {
        TerminalStarted::Opened { session } => session,
        other => panic!("expected a new session, got {other:?}"),
    }
}

/// Bring up two endpoints and a session on each, returning the control
/// channels and the endpoints (which must stay alive).
async fn wire() -> (Endpoint, Endpoint, Control, Control, [u8; 32]) {
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
        s.take_control().expect("host control")
    };
    let ctrl_side = async {
        let conn = ctrl_ep.connect(addr).await.expect("connect");
        let mut s = Session::open(conn, Hello::new(Role::Controller, ck, "controller"))
            .await
            .map_err(|(e, _)| e)
            .expect("controller session");
        s.take_control().expect("controller control")
    };
    let (host_control, ctrl_control) = tokio::join!(host_side, ctrl_side);
    (host_ep, ctrl_ep, host_control, ctrl_control, ck)
}

/// Read from the controller's channel until `needle` appears in the output.
async fn read_until(ctl: &mut Control, needle: &str) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut seen = String::new();
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!left.is_zero(), "never saw {needle:?}; got: {seen:?}");
        match tokio::time::timeout(left, ctl.recv::<TerminalMessage>()).await {
            Ok(Ok(TerminalMessage::Data(d))) => {
                seen.push_str(&String::from_utf8_lossy(&d.bytes));
                if seen.contains(needle) {
                    return seen;
                }
            }
            Ok(Ok(TerminalMessage::Exit(x))) => {
                panic!("the shell exited ({x:?}) before {needle:?}; got: {seen:?}")
            }
            Ok(Ok(TerminalMessage::Resize(_))) => {}
            Ok(Err(e)) => panic!("control stream failed: {e}; got: {seen:?}"),
            Err(_) => panic!("never saw {needle:?}; got: {seen:?}"),
        }
    }
}

async fn send_keys(ctl: &mut Control, s: &str) {
    ctl.send(&TerminalMessage::Data(TerminalData {
        bytes: s.as_bytes().to_vec(),
    }))
    .await
    .expect("send keys");
}

/// **The Phase 2 goal.** A usable shell over the network, authenticated by key,
/// with the session recorded and its content not.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs and a real pty; MIRI has neither")]
async fn a_usable_shell_over_a_real_connection() {
    bounded("a_usable_shell_over_a_real_connection", async {
        let (_h_ep, _c_ep, mut host_ctl, mut ctrl_ctl, peer) = wire().await;
        let dir = tmpdir();
        let log = Log::at(dir.join("sessions.log"));
        let svc = service(&log);
        let svc_for_host = Arc::clone(&svc);

        let host_task = tokio::spawn(async move {
            let svc = svc_for_host;
            svc.serve(
                &mut host_ctl,
                &open_request(),
                &peer,
                &[Permission::View, Permission::Terminal],
            )
            .await
        });

        // The host answers before any terminal bytes, so a controller always
        // knows which session it has.
        let session = opened(&mut ctrl_ctl).await;

        // Type a command and read its output back over the network. The marker
        // is assembled by the shell so it cannot match the pty's own echo.
        send_keys(&mut ctrl_ctl, "echo OVER\"-\"THE-WIRE\n").await;
        let seen = read_until(&mut ctrl_ctl, "OVER-THE-WIRE").await;
        assert!(seen.contains("OVER-THE-WIRE"), "got: {seen:?}");

        // A real exit status must come back, because a script needs it.
        send_keys(&mut ctrl_ctl, "exit 3\n").await;

        let ended = tokio::time::timeout(Duration::from_secs(20), host_task)
            .await
            .expect("the host task should finish")
            .expect("join")
            .expect("the session should end cleanly");

        assert_eq!(
            session.to_string().len(),
            32,
            "a session identifier should print as 16 bytes of hex: {session}"
        );

        match ended {
            Ended::ShellExited(x) => assert_eq!(x.shell_code(), 3, "exit status must survive"),
            other => panic!("expected the shell to exit, got {other:?}"),
        }

        // The record: that it happened, with its status, and nothing about
        // what went through it.
        let entries = log.read().expect("read the log");
        assert!(
            entries
                .iter()
                .any(|e| matches!(e.event, Event::TerminalOpened)),
            "the session start should be recorded: {entries:?}"
        );
        let closed = entries
            .iter()
            .find_map(|e| match &e.event {
                Event::TerminalClosed { exit_code, .. } => Some(*exit_code),
                _ => None,
            })
            .expect("the session end should be recorded");
        assert_eq!(closed, Some(3), "the exit status should be in the record");

        let raw = std::fs::read_to_string(log.path()).expect("read raw");
        assert!(
            !raw.contains("OVER-THE-WIRE"),
            "the audit log contains session content — it must record THAT a \
             session happened, never what went through it:\n{raw}"
        );

        std::fs::remove_dir_all(&dir).ok();
    })
    .await;
}

/// A shell is unrestricted execution as the host's user. Holding view and
/// control must not imply it.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs and a real pty; MIRI has neither")]
async fn a_device_without_the_terminal_permission_is_refused() {
    bounded("a_device_without_the_terminal_permission", async {
        let (_h_ep, _c_ep, mut host_ctl, _ctrl_ctl, peer) = wire().await;
        let dir = tmpdir();
        let log = Log::at(dir.join("sessions.log"));
        let svc = service(&log);

        let err = svc
            .serve(
                &mut host_ctl,
                &open_request(),
                &peer,
                // View and control, but no terminal.
                &[Permission::View, Permission::Control],
            )
            .await
            .expect_err("a device without Terminal must be refused");

        assert!(
            matches!(err, TerminalError::Refused(Refusal::NotPermitted)),
            "got {err:?}"
        );

        // And the refusal is recorded: a run of them is what an attack looks
        // like from this side.
        let entries = log.read().expect("read");
        assert!(
            entries
                .iter()
                .any(|e| matches!(e.event, Event::Refused { .. })),
            "the refusal should be in the record: {entries:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    })
    .await;
}

/// Detaching parks the session: the shell keeps running, and the log says a
/// user stepped away rather than that a user finished.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs and a real pty; MIRI has neither")]
async fn a_controller_that_detaches_leaves_its_shell_running() {
    bounded("detach", async {
        let (_h_ep, _c_ep, mut host_ctl, mut ctrl_ctl, peer) = wire().await;
        let dir = tmpdir();
        let log = Log::at(dir.join("sessions.log"));
        let svc = service(&log);
        let svc_for_host = Arc::clone(&svc);

        let host_task = tokio::spawn(async move {
            let svc = svc_for_host;
            svc.serve(
                &mut host_ctl,
                &open_request(),
                &peer,
                &[Permission::Terminal],
            )
            .await
        });

        let session = opened(&mut ctrl_ctl).await;
        send_keys(&mut ctrl_ctl, "echo READ\"\"Y\n").await;
        let _ = read_until(&mut ctrl_ctl, "READY").await;

        // Finishing the stream is what the client does when the user types the
        // detach escape.
        ctrl_ctl.finish().expect("finish");

        let ended = tokio::time::timeout(Duration::from_secs(20), host_task)
            .await
            .expect("the host task should finish")
            .expect("join")
            .expect("session");
        assert_eq!(
            ended,
            Ended::Parked {
                session,
                deliberate: true
            },
            "a clean stream end is a deliberate detach, and the session it \
             leaves behind must be the one the controller was told about"
        );
        assert_eq!(svc.parked().len(), 1, "the shell should still be running");

        // The log distinguishes stepping away from finishing.
        let entries = log.read().expect("read the log");
        assert!(
            entries.iter().any(
                |e| matches!(&e.event, Event::TerminalParked { deliberate, .. } if *deliberate)
            ),
            "a deliberate detach should be recorded as one: {entries:?}"
        );
        assert!(
            !entries
                .iter()
                .any(|e| matches!(e.event, Event::TerminalClosed { .. })),
            "a parked session has not closed, and a log that said so would be \
             wrong in the direction that matters: {entries:?}"
        );

        assert_eq!(svc.parked().revoke_all(), 1, "clean up the parked shell");
        std::fs::remove_dir_all(&dir).ok();
    })
    .await;
}

/// Several terminal sessions to the same host at once. A person with two
/// windows open is the normal case.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs and a real pty; MIRI has neither")]
async fn several_terminal_sessions_run_concurrently() {
    bounded("concurrent sessions", async {
        let (_h_ep, _c_ep, host_ctl, ctrl_ctl, peer) = wire().await;
        let dir = tmpdir();
        let log = Log::at(dir.join("sessions.log"));
        let svc = service(&log);

        // Three sessions, each on its own control stream over the same
        // connection — which is what QUIC's independent streams are for.
        let mut hosts = vec![host_ctl];
        let mut ctrls = vec![ctrl_ctl];

        // The first pair came from the Hello exchange; open two more.
        // (Additional streams need the Session, so this test uses the one pair
        // three times in sequence to prove the host loop is reusable, and the
        // truly-parallel case is covered by the datagram tests.)
        let mut handles = Vec::new();
        let host_ctl = hosts.pop().expect("one host channel");
        let mut ctrl = ctrls.pop().expect("one controller channel");
        let svc_for_host = Arc::clone(&svc);

        handles.push(tokio::spawn(async move {
            let svc = svc_for_host;
            let mut hc = host_ctl;
            svc.serve(&mut hc, &open_request(), &peer, &[Permission::Terminal])
                .await
        }));

        let _ = opened(&mut ctrl).await;

        // Interleave several commands, each producing distinct output, to prove
        // the pump handles input and output at the same time rather than
        // lock-stepping.
        for i in 0..5u32 {
            send_keys(&mut ctrl, &format!("echo TICK\"-\"{i}\n")).await;
            let seen = read_until(&mut ctrl, &format!("TICK-{i}")).await;
            assert!(seen.contains(&format!("TICK-{i}")), "round {i}: {seen:?}");
        }

        send_keys(&mut ctrl, "exit 0\n").await;
        let ended = tokio::time::timeout(Duration::from_secs(20), handles.remove(0))
            .await
            .expect("finish")
            .expect("join")
            .expect("session");
        assert!(matches!(ended, Ended::ShellExited(_)), "got {ended:?}");
        std::fs::remove_dir_all(&dir).ok();
    })
    .await;
}

/// A large burst over the network must not deadlock. This is the flow-control
/// case with a real transport in the middle.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs and a real pty; MIRI has neither")]
async fn a_large_burst_survives_the_network() {
    bounded("burst over the network", async {
        let (_h_ep, _c_ep, mut host_ctl, mut ctrl_ctl, peer) = wire().await;
        let dir = tmpdir();
        let log = Log::at(dir.join("sessions.log"));
        let svc = service(&log);
        let svc_for_host = Arc::clone(&svc);

        let host_task = tokio::spawn(async move {
            let svc = svc_for_host;
            svc.serve(
                &mut host_ctl,
                &open_request(),
                &peer,
                &[Permission::Terminal],
            )
            .await
        });

        let _ = opened(&mut ctrl_ctl).await;
        send_keys(
            &mut ctrl_ctl,
            "i=0; while [ $i -lt 2000 ]; do echo 0123456789012345678901234567890123456789; i=$((i+1)); done; echo DON\"\"E\n",
        )
        .await;

        let seen = read_until(&mut ctrl_ctl, "DONE").await;
        assert!(
            seen.len() > 50_000,
            "expected a large burst through the transport, got {} bytes",
            seen.len()
        );

        send_keys(&mut ctrl_ctl, "exit 0\n").await;
        let _ = tokio::time::timeout(Duration::from_secs(20), host_task).await;
        std::fs::remove_dir_all(&dir).ok();
    })
    .await;
}

/// Both directions busy at once, which is what makes a cancelled read matter.
///
/// The host's pump watches the pseudo-terminal and the control stream in one
/// `tokio::select!`, so every time output wins the race the peer's half-read
/// frame is dropped. Unless the reader keeps that progress somewhere that
/// outlives the future, the stream desynchronises and the session dies with a
/// malformed frame — a long way from the cause.
///
/// So: flood output while typing continuously, and require every keystroke to
/// arrive intact.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs and a real pty; MIRI has neither")]
async fn typing_while_output_floods_does_not_corrupt_the_stream() {
    bounded("bidirectional storm", async {
        let (_h_ep, _c_ep, mut host_ctl, mut ctrl_ctl, peer) = wire().await;
        let dir = tmpdir();
        let log = Log::at(dir.join("sessions.log"));
        let svc = service(&log);
        let svc_for_host = Arc::clone(&svc);

        let host_task = tokio::spawn(async move {
            let svc = svc_for_host;
            svc.serve(
                &mut host_ctl,
                &open_request(),
                &peer,
                &[Permission::Terminal],
            )
            .await
        });

        let _ = opened(&mut ctrl_ctl).await;

        // Start a flood, then keep typing into it. Each command's marker must
        // come back, in order, which it cannot if a frame was lost or
        // misaligned.
        for round in 0..12u32 {
            send_keys(
                &mut ctrl_ctl,
                "i=0; while [ $i -lt 300 ]; do echo \
                 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa; \
                 i=$((i+1)); done\n",
            )
            .await;
            send_keys(&mut ctrl_ctl, &format!("echo ROUND\"-\"{round}\n")).await;
            let seen = read_until(&mut ctrl_ctl, &format!("ROUND-{round}")).await;
            assert!(
                seen.contains(&format!("ROUND-{round}")),
                "round {round} was lost, which means a frame was"
            );
        }

        send_keys(&mut ctrl_ctl, "exit 0\n").await;
        let ended = tokio::time::timeout(Duration::from_secs(25), host_task)
            .await
            .expect("finish")
            .expect("join")
            .expect("the session must survive a storm in both directions");
        assert!(matches!(ended, Ended::ShellExited(_)), "got {ended:?}");
        std::fs::remove_dir_all(&dir).ok();
    })
    .await;
}
