#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A dropped link must not kill the shell.
//!
//! The claim terminal mode makes is that you can start a long job on a server
//! from a laptop, close the laptop, and come back to the job. These tests are
//! that claim, over a real connection, with a real shell — and the security
//! properties that have to hold for a standing shell to be safe to leave.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Once};
use std::time::Duration;

use ownview_audit::{Event, Log};
use ownview_crypto::Identity;
use ownview_proto::{
    Hello, Permission, Refusal, Role, SessionId, TerminalData, TerminalMessage, TerminalOpen,
    TerminalResume, TerminalStart, TerminalStarted,
};
use ownview_transport::{
    AnyOf, Control, Ended, Endpoint, Limits, NoPeers, OnlyPeer, Session, TerminalError,
    TerminalPolicy, TerminalService,
};

const TEST_TIMEOUT: Duration = Duration::from_secs(40);

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
    p.push(format!("ownview-reconnect-{}", u64::from_le_bytes(n)));
    std::fs::create_dir_all(&p).expect("mkdir");
    p
}

fn open() -> TerminalStart {
    TerminalStart::Open(TerminalOpen {
        cols: 80,
        rows: 24,
        term: "xterm-256color".to_owned(),
    })
}

/// A resume, at a deliberately different window size from [`open`], so a test
/// can tell whether the new size was applied.
fn resume(session: SessionId) -> TerminalStart {
    TerminalStart::Resume(TerminalResume {
        session,
        cols: 100,
        rows: 40,
    })
}

/// The host: one endpoint, bound once, as a server is.
struct HostSide {
    ep: Endpoint,
    addr: SocketAddr,
    key: [u8; 32],
}

/// A controller device. Its identity outlives any one connection, which is the
/// whole point: coming back is coming back *as the same device*.
struct DeviceSide {
    id: Identity,
    key: [u8; 32],
}

impl DeviceSide {
    fn new() -> Self {
        let id = Identity::generate().expect("device identity");
        let key = id.public_key();
        Self { id, key }
    }
}

/// A host that will accept these devices, and the devices themselves.
///
/// The count is a const parameter so callers destructure — `let (host, [me,
/// other]) = fleet();` — and name their devices instead of indexing a vector.
/// Indexing panics, and this crate denies it in tests as well as in the
/// library, because a test that panics in its own scaffolding says nothing
/// about the code it was meant to check.
fn fleet<const N: usize>() -> (HostSide, [DeviceSide; N]) {
    init();
    let host_id = Identity::generate().expect("host identity");
    let devs: [DeviceSide; N] = std::array::from_fn(|_| DeviceSide::new());

    let ep = Endpoint::bind(
        &host_id,
        loopback(),
        Arc::new(AnyOf::new(devs.iter().map(|d| d.key).collect())),
        Arc::new(NoPeers),
    )
    .expect("host endpoint");
    let addr = ep.local_addr().expect("addr");
    let key = host_id.public_key();
    (HostSide { ep, addr, key }, devs)
}

/// One connection. The controller binds a **fresh endpoint** each time, as a
/// laptop reconnecting from a different network does.
///
/// The endpoint is returned rather than kept internally so a test can drop it,
/// which is how a link is lost without warning.
async fn dial(host: &HostSide, dev: &DeviceSide) -> (Control, Control, Endpoint) {
    let ctrl_ep = Endpoint::bind(
        &dev.id,
        loopback(),
        Arc::new(NoPeers),
        Arc::new(OnlyPeer::new(host.key)),
    )
    .expect("controller endpoint");

    let hk = host.key;
    let ck = dev.key;
    let host_side = async {
        let conn = host.ep.accept().await.unwrap().unwrap();
        let mut s = Session::open(conn, Hello::new(Role::Host, hk, "host"))
            .await
            .map_err(|(e, _)| e)
            .expect("host session");
        s.take_control().expect("host control")
    };
    let ctrl_side = async {
        let conn = ctrl_ep.connect(host.addr).await.expect("connect");
        let mut s = Session::open(conn, Hello::new(Role::Controller, ck, "controller"))
            .await
            .map_err(|(e, _)| e)
            .expect("controller session");
        s.take_control().expect("controller control")
    };
    let (h, c) = tokio::join!(host_side, ctrl_side);
    (h, c, ctrl_ep)
}

fn service(log: &Log, limits: Limits, policy: TerminalPolicy) -> Arc<TerminalService> {
    Arc::new(TerminalService::new(log.clone(), limits, policy))
}

/// Run one session on the host side, in its own task.
///
/// The channel is **returned rather than dropped**, and that is load-bearing:
/// it holds the last handle to the QUIC connection, so dropping it closes the
/// connection and discards anything still in flight. A refusal is written and
/// the task then finishes immediately, so a task that dropped its channel
/// would lose the very message the test is checking for. In the product the
/// connection outlives any one terminal stream, which is the same thing by a
/// different route.
type Served = tokio::task::JoinHandle<(Result<Ended, TerminalError>, Control)>;

fn serve(
    svc: &Arc<TerminalService>,
    mut host_ctl: Control,
    start: TerminalStart,
    peer: [u8; 32],
    granted: Vec<Permission>,
) -> Served {
    let svc = Arc::clone(svc);
    tokio::spawn(async move {
        let r = svc.serve(&mut host_ctl, &start, &peer, &granted).await;
        (r, host_ctl)
    })
}

/// Await a served session, expecting it to have run.
async fn ok(h: Served) -> Ended {
    let (r, ctl) = h.await.expect("join");
    drop(ctl);
    r.expect("the session should have run")
}

/// Await a served session, expecting a refusal.
async fn refused(h: Served) -> TerminalError {
    let (r, ctl) = h.await.expect("join");
    drop(ctl);
    r.expect_err("the session should have been refused")
}

async fn started(ctl: &mut Control) -> TerminalStarted {
    ctl.recv::<TerminalStarted>()
        .await
        .expect("the host must answer before sending output")
}

async fn opened(ctl: &mut Control) -> SessionId {
    match started(ctl).await {
        TerminalStarted::Opened { session } => session,
        other => panic!("expected a new session, got {other:?}"),
    }
}

async fn send_keys(ctl: &mut Control, s: &str) {
    ctl.send(&TerminalMessage::Data(TerminalData {
        bytes: s.as_bytes().to_vec(),
    }))
    .await
    .expect("send keys");
}

/// Read until `needle` appears, tolerating the shell exiting afterwards.
async fn read_until(ctl: &mut Control, needle: &str) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(25);
    let mut seen = String::new();
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !left.is_zero(),
            "never saw {needle:?}; got {} bytes",
            seen.len()
        );
        match tokio::time::timeout(left, ctl.recv::<TerminalMessage>()).await {
            Ok(Ok(TerminalMessage::Data(d))) => {
                seen.push_str(&String::from_utf8_lossy(&d.bytes));
                if seen.contains(needle) {
                    return seen;
                }
            }
            Ok(Ok(TerminalMessage::Exit(x))) => panic!(
                "the shell exited ({x:?}) before {needle:?}; got {} bytes: {}",
                seen.len(),
                tail(&seen)
            ),
            Ok(Ok(TerminalMessage::Resize(_))) => {}
            Ok(Err(e)) => panic!("control stream failed: {e}; got: {}", tail(&seen)),
            Err(_) => panic!(
                "never saw {needle:?} in {} bytes ({} loop lines); tail: {}",
                seen.len(),
                seen.matches("0123456789012345678901234567890123456789")
                    .count(),
                tail(&seen)
            ),
        }
    }
}

/// The last part of a transcript, for a failure message that stays readable.
fn tail(s: &str) -> String {
    let from = s.len().saturating_sub(400);
    s.get(from..).unwrap_or(s).to_owned()
}

/// **The claim.** Set something that exists only in the shell's memory, lose
/// the link hard, come back, and read it out again.
///
/// A shell variable is the strongest evidence available: a freshly started
/// shell would print an empty line. This cannot pass by accident of the resume
/// quietly opening a new terminal.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs and a real pty; MIRI has neither")]
async fn a_dropped_link_leaves_the_same_shell_to_come_back_to() {
    bounded("resume the same shell", async {
        let (host, [me]) = fleet();
        let dir = tmpdir();
        let log = Log::at(dir.join("sessions.log"));
        let svc = service(&log, Limits::default(), TerminalPolicy::default());

        // Mark the shell.
        let (h1, mut c1, ep1) = dial(&host, &me).await;
        let first = serve(&svc, h1, open(), me.key, vec![Permission::Terminal]);
        let session = opened(&mut c1).await;
        send_keys(&mut c1, "MARK=survived-the-drop\n").await;
        send_keys(&mut c1, "echo SET\"\"UP\n").await;
        let _ = read_until(&mut c1, "SETUP").await;

        // Lose the link with no warning at all.
        drop(c1);
        drop(ep1);

        let ended = ok(first).await;
        assert_eq!(
            ended,
            Ended::Parked {
                session,
                deliberate: false
            },
            "a hard drop must park the session, not kill it and not read as a \
             deliberate detach"
        );
        assert_eq!(svc.parked().len(), 1, "the shell should still be running");

        // Come back, on a new connection from the same device.
        let (h2, mut c2, _ep2) = dial(&host, &me).await;
        let second = serve(
            &svc,
            h2,
            resume(session),
            me.key,
            vec![Permission::Terminal],
        );
        match started(&mut c2).await {
            TerminalStarted::Resumed { session: got, .. } => assert_eq!(got, session),
            other => panic!("expected to resume, got {other:?}"),
        }
        assert_eq!(
            svc.parked().len(),
            0,
            "a resumed session is no longer parked"
        );

        send_keys(&mut c2, "echo \"MARK IS $MARK\"\n").await;
        let seen = read_until(&mut c2, "MARK IS survived-the-drop").await;
        assert!(
            seen.contains("MARK IS survived-the-drop"),
            "the variable lives only in that shell's memory, so this is the \
             same shell: {}",
            tail(&seen)
        );

        // And it is still a working shell, not just a readable one.
        send_keys(&mut c2, "exit 7\n").await;
        let ended = ok(second).await;
        assert!(
            matches!(ended, Ended::ShellExited(x) if x.shell_code() == 7),
            "got {ended:?}"
        );

        let entries = log.read().expect("read the log");
        assert!(
            entries.iter().any(|e| matches!(
                &e.event,
                Event::TerminalParked { deliberate, .. } if !*deliberate
            )),
            "a dropped link should be recorded as not deliberate: {entries:?}"
        );
        let parked_secs = entries
            .iter()
            .find_map(|e| match &e.event {
                Event::TerminalResumed { parked_secs, .. } => Some(*parked_secs),
                _ => None,
            })
            .expect("the resume should be recorded");
        let _ = parked_secs;
        std::fs::remove_dir_all(&dir).ok();
    })
    .await;
}

/// **The part that is easy to get wrong.** With nobody attached, something must
/// still read the pseudo-terminal — or the kernel's buffer fills, the shell
/// blocks on its next write, and the job the user detached in order to keep
/// running stops running.
///
/// The job writes far more than any terminal buffer holds and *then* a marker.
/// The link is dropped once the job is provably under way and while the
/// controller has stopped reading — so the shell is already blocked on a write
/// when its connection disappears. Only something draining the terminal can
/// get it moving again, and the marker is the proof that something did.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs and a real pty; MIRI has neither")]
async fn a_job_keeps_running_while_nobody_is_attached() {
    bounded("job progresses while detached", async {
        let (host, [me]) = fleet();
        let dir = tmpdir();
        let log = Log::at(dir.join("sessions.log"));
        let svc = service(&log, Limits::default(), TerminalPolicy::default());

        const LINE: &str = "0123456789012345678901234567890123456789012345678901234567890123";

        let (h1, mut c1, ep1) = dial(&host, &me).await;
        let first = serve(&svc, h1, open(), me.key, vec![Permission::Terminal]);
        let session = opened(&mut c1).await;
        send_keys(&mut c1, "echo SET\"\"UP\n").await;
        let _ = read_until(&mut c1, "SETUP").await;

        // Well over a megabyte, then a marker.
        send_keys(
            &mut c1,
            &format!(
                "i=0; while [ $i -lt 20000 ]; do echo {LINE}; i=$((i+1)); done; echo ALL-DO\"\"NE\n"
            ),
        )
        .await;

        // Wait until the job is *provably* running before dropping the link.
        //
        // Not merely tidiness: keystrokes sit in the transport's send buffer
        // until the endpoint transmits them, so tearing the connection down
        // immediately after writing them throws them away and the shell never
        // sees the command at all. The first version of this test did exactly
        // that and measured a shell that had run nothing.
        let _ = read_until(&mut c1, LINE).await;

        // Now stop reading and drop. The controller not reading backs the
        // transport up, which backs the pump up, which fills the terminal — so
        // the shell is blocked on a write at the moment its link disappears.
        drop(c1);
        drop(ep1);
        let ended = ok(first).await;
        assert!(matches!(ended, Ended::Parked { .. }), "got {ended:?}");

        // Time to finish with nobody watching. If nothing is draining the
        // terminal this is where it stays wedged.
        tokio::time::sleep(Duration::from_secs(4)).await;

        let (h2, mut c2, _ep2) = dial(&host, &me).await;
        let second = serve(
            &svc,
            h2,
            resume(session),
            me.key,
            vec![Permission::Terminal],
        );
        let lost = match started(&mut c2).await {
            TerminalStarted::Resumed { lost, .. } => lost,
            other => panic!("expected to resume, got {other:?}"),
        };

        let seen = read_until(&mut c2, "ALL-DONE").await;
        assert!(
            seen.contains("ALL-DONE"),
            "the job must have run to completion while nobody was attached — \
             if it did not, nothing was draining the pseudo-terminal and the \
             shell stayed blocked on a write: {}",
            tail(&seen)
        );

        // The backlog is bounded, so a job this loud must have lost output —
        // and the user must be told, inline, where the gap is.
        assert!(
            lost > 0,
            "over a megabyte of output into a 256 KiB backlog must report loss"
        );
        assert!(
            seen.contains("bytes of output were lost while disconnected"),
            "the gap must be visible in the transcript, not only in a status \
             field nobody reads: {}",
            tail(&seen)
        );
        assert!(
            seen.len() < 400_000,
            "the backlog is supposed to be bounded, got {} bytes",
            seen.len()
        );

        send_keys(&mut c2, "exit 0\n").await;
        let _ = ok(second).await;
        std::fs::remove_dir_all(&dir).ok();
    })
    .await;
}

/// A parked session is a live shell running as the host's user. Another paired
/// device holding its identifier must not be able to walk into it.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs and a real pty; MIRI has neither")]
async fn another_device_cannot_resume_someone_elses_session() {
    bounded("no cross-device resume", async {
        let (host, [me, other]) = fleet();
        let dir = tmpdir();
        let log = Log::at(dir.join("sessions.log"));
        let svc = service(&log, Limits::default(), TerminalPolicy::default());

        let (h1, mut c1, ep1) = dial(&host, &me).await;
        let first = serve(&svc, h1, open(), me.key, vec![Permission::Terminal]);
        let session = opened(&mut c1).await;
        send_keys(&mut c1, "echo SET\"\"UP\n").await;
        let _ = read_until(&mut c1, "SETUP").await;
        drop(c1);
        drop(ep1);
        let _ = ok(first).await;
        assert_eq!(svc.parked().len(), 1);

        // The second device is paired, permitted, and knows the identifier.
        // It must still be refused.
        let (h2, mut c2, _ep2) = dial(&host, &other).await;
        let attempt = serve(
            &svc,
            h2,
            resume(session),
            other.key,
            vec![Permission::Terminal],
        );
        assert_eq!(
            started(&mut c2).await,
            TerminalStarted::Refused(Refusal::Unknown),
            "the identifier is not a capability; the owning key is"
        );
        let err = refused(attempt).await;
        assert!(
            matches!(err, TerminalError::Refused(Refusal::Unknown)),
            "got {err:?}"
        );

        assert_eq!(
            svc.parked().len(),
            1,
            "and the refused attempt must not have disturbed the real session"
        );

        // The owner can still get in.
        let (h3, mut c3, _ep3) = dial(&host, &me).await;
        let third = serve(
            &svc,
            h3,
            resume(session),
            me.key,
            vec![Permission::Terminal],
        );
        assert!(matches!(
            started(&mut c3).await,
            TerminalStarted::Resumed { .. }
        ));
        send_keys(&mut c3, "exit 0\n").await;
        let _ = ok(third).await;
        std::fs::remove_dir_all(&dir).ok();
    })
    .await;
}

/// A refusal must not tell a device whether the session it asked for exists.
/// Distinguishing "never existed" from "not yours" would confirm that some
/// other device has a live shell.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs and a real pty; MIRI has neither")]
async fn an_unknown_session_is_refused_exactly_like_someone_elses() {
    bounded("indistinguishable refusals", async {
        let (host, [me, other]) = fleet();
        let dir = tmpdir();
        let log = Log::at(dir.join("sessions.log"));
        let svc = service(&log, Limits::default(), TerminalPolicy::default());

        // A real session, owned by device 0.
        let (h1, mut c1, ep1) = dial(&host, &me).await;
        let first = serve(&svc, h1, open(), me.key, vec![Permission::Terminal]);
        let real = opened(&mut c1).await;
        send_keys(&mut c1, "echo SET\"\"UP\n").await;
        let _ = read_until(&mut c1, "SETUP").await;
        drop(c1);
        drop(ep1);
        let _ = ok(first).await;

        let invented = SessionId::from_bytes([0x5Au8; 16]);

        let ask = async |target: SessionId| {
            let (h, mut c, _ep) = dial(&host, &other).await;
            let t = serve(
                &svc,
                h,
                resume(target),
                other.key,
                vec![Permission::Terminal],
            );
            let answer = started(&mut c).await;
            let _ = refused(t).await;
            answer
        };
        let for_a_real_session = ask(real).await;
        let for_an_invented_one = ask(invented).await;

        assert_eq!(
            for_a_real_session, for_an_invented_one,
            "asking for another device's live session and asking for one that \
             never existed must be answered identically, or the answer is a \
             way to learn that another device has a shell open"
        );
        assert_eq!(
            for_a_real_session,
            TerminalStarted::Refused(Refusal::Unknown)
        );

        assert_eq!(svc.parked().revoke_all(), 1);
        std::fs::remove_dir_all(&dir).ok();
    })
    .await;
}

/// Returning to a shell opened while permitted must not survive losing the
/// permission. Otherwise revocation is advisory, and a standing shell is the
/// most valuable thing there is to revoke.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs and a real pty; MIRI has neither")]
async fn a_resume_is_refused_once_the_terminal_permission_is_gone() {
    bounded("permission re-checked on resume", async {
        let (host, [me]) = fleet();
        let dir = tmpdir();
        let log = Log::at(dir.join("sessions.log"));
        let svc = service(&log, Limits::default(), TerminalPolicy::default());

        let (h1, mut c1, ep1) = dial(&host, &me).await;
        let first = serve(&svc, h1, open(), me.key, vec![Permission::Terminal]);
        let session = opened(&mut c1).await;
        send_keys(&mut c1, "echo SET\"\"UP\n").await;
        let _ = read_until(&mut c1, "SETUP").await;
        drop(c1);
        drop(ep1);
        let _ = ok(first).await;
        assert_eq!(svc.parked().len(), 1);

        // Same device, same identifier — but view and control only now.
        let (h2, mut c2, _ep2) = dial(&host, &me).await;
        let attempt = serve(
            &svc,
            h2,
            resume(session),
            me.key,
            vec![Permission::View, Permission::Control],
        );
        assert_eq!(
            started(&mut c2).await,
            TerminalStarted::Refused(Refusal::NotPermitted)
        );
        let err = refused(attempt).await;
        assert!(
            matches!(err, TerminalError::Refused(Refusal::NotPermitted)),
            "got {err:?}"
        );

        assert_eq!(svc.parked().revoke_all(), 1);
        std::fs::remove_dir_all(&dir).ok();
    })
    .await;
}

/// Revoking a device kills the shells it left standing, rather than leaving
/// them for its next connection.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs and a real pty; MIRI has neither")]
async fn revoking_a_device_kills_the_shells_it_left_standing() {
    bounded("revoke kills parked shells", async {
        let (host, [me, other]) = fleet();
        let dir = tmpdir();
        let log = Log::at(dir.join("sessions.log"));
        let svc = service(&log, Limits::default(), TerminalPolicy::default());

        let park_one = async |dev: &DeviceSide| {
            let (h, mut c, ep) = dial(&host, dev).await;
            let t = serve(&svc, h, open(), dev.key, vec![Permission::Terminal]);
            let id = opened(&mut c).await;
            send_keys(&mut c, "echo SET\"\"UP\n").await;
            let _ = read_until(&mut c, "SETUP").await;
            drop(c);
            drop(ep);
            let _ = ok(t).await;
            id
        };
        let mine = park_one(&me).await;
        let _theirs = park_one(&other).await;
        assert_eq!(svc.parked().len(), 2);

        assert_eq!(svc.revoke(&me.key), 1, "one shell of device 0's");
        assert_eq!(
            svc.parked().len(),
            1,
            "and the other device's shell is untouched — revoking one device \
             must not be a way to end another's session"
        );
        assert_eq!(svc.parked().held_by(&other.key), 1);

        // The revoked device cannot come back to it.
        let (h, mut c, _ep) = dial(&host, &me).await;
        let t = serve(&svc, h, resume(mine), me.key, vec![Permission::Terminal]);
        assert_eq!(
            started(&mut c).await,
            TerminalStarted::Refused(Refusal::Unknown)
        );
        let _ = refused(t).await;

        let entries = log.read().expect("read");
        assert!(
            entries
                .iter()
                .any(|e| matches!(e.event, Event::Revoked { sessions_closed: 1 })),
            "the revocation should say how many shells it closed: {entries:?}"
        );

        assert_eq!(svc.parked().revoke_all(), 1);
        std::fs::remove_dir_all(&dir).ok();
    })
    .await;
}

/// Two connections must not both be handed the same shell, and a stale
/// identifier must not work twice.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs and a real pty; MIRI has neither")]
async fn a_session_can_only_be_resumed_once() {
    bounded("resume is not replayable", async {
        let (host, [me]) = fleet();
        let dir = tmpdir();
        let log = Log::at(dir.join("sessions.log"));
        let svc = service(&log, Limits::default(), TerminalPolicy::default());

        let (h1, mut c1, ep1) = dial(&host, &me).await;
        let first = serve(&svc, h1, open(), me.key, vec![Permission::Terminal]);
        let session = opened(&mut c1).await;
        send_keys(&mut c1, "echo SET\"\"UP\n").await;
        let _ = read_until(&mut c1, "SETUP").await;
        drop(c1);
        drop(ep1);
        let _ = ok(first).await;

        // The real resume.
        let (h2, mut c2, _ep2) = dial(&host, &me).await;
        let second = serve(
            &svc,
            h2,
            resume(session),
            me.key,
            vec![Permission::Terminal],
        );
        assert!(matches!(
            started(&mut c2).await,
            TerminalStarted::Resumed { .. }
        ));

        // The same identifier again, from the same device, while the first
        // resume is still attached.
        let (h3, mut c3, _ep3) = dial(&host, &me).await;
        let third = serve(
            &svc,
            h3,
            resume(session),
            me.key,
            vec![Permission::Terminal],
        );
        assert_eq!(
            started(&mut c3).await,
            TerminalStarted::Refused(Refusal::Unknown),
            "a session that is attached is not parked, and must not be handed \
             out a second time"
        );
        let _ = refused(third).await;

        // The first resume still owns a working shell.
        send_keys(&mut c2, "echo STI\"\"LL-MINE\n").await;
        let seen = read_until(&mut c2, "STILL-MINE").await;
        assert!(seen.contains("STILL-MINE"), "{}", tail(&seen));
        send_keys(&mut c2, "exit 0\n").await;
        let _ = ok(second).await;
        std::fs::remove_dir_all(&dir).ok();
    })
    .await;
}

/// The audit record must say how long a session actually sat with nobody
/// attached. A field that always reads zero is worse than no field: it looks
/// like an answer.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs and a real pty; MIRI has neither")]
async fn the_record_says_how_long_a_session_was_parked() {
    bounded("parked duration is real", async {
        let (host, [me]) = fleet();
        let dir = tmpdir();
        let log = Log::at(dir.join("sessions.log"));
        let svc = service(&log, Limits::default(), TerminalPolicy::default());

        let (h1, mut c1, ep1) = dial(&host, &me).await;
        let first = serve(&svc, h1, open(), me.key, vec![Permission::Terminal]);
        let session = opened(&mut c1).await;
        send_keys(&mut c1, "echo SET\"\"UP\n").await;
        let _ = read_until(&mut c1, "SETUP").await;
        drop(c1);
        drop(ep1);
        let _ = ok(first).await;

        tokio::time::sleep(Duration::from_millis(2100)).await;

        let (h2, mut c2, _ep2) = dial(&host, &me).await;
        let second = serve(
            &svc,
            h2,
            resume(session),
            me.key,
            vec![Permission::Terminal],
        );
        assert!(matches!(
            started(&mut c2).await,
            TerminalStarted::Resumed { .. }
        ));
        send_keys(&mut c2, "exit 0\n").await;
        let _ = ok(second).await;

        let entries = log.read().expect("read the log");
        let parked_secs = entries
            .iter()
            .find_map(|e| match &e.event {
                Event::TerminalResumed { parked_secs, .. } => Some(*parked_secs),
                _ => None,
            })
            .expect("the resume should be recorded");
        assert!(
            parked_secs >= 2,
            "it was parked for over two seconds, but the record says \
             {parked_secs} — a duration that is always zero is not a duration"
        );
        std::fs::remove_dir_all(&dir).ok();
    })
    .await;
}

/// A session nobody comes back for is not kept for ever. A standing shell is a
/// standing capability, and one left by a laptop that was thrown away should
/// not still be there next week.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs and a real pty; MIRI has neither")]
async fn the_park_window_ends_a_session_nobody_came_back_for() {
    bounded("park window", async {
        let (host, [me]) = fleet();
        let dir = tmpdir();
        let log = Log::at(dir.join("sessions.log"));
        let svc = service(
            &log,
            Limits {
                park_for: Duration::from_millis(400),
                ..Limits::default()
            },
            TerminalPolicy::default(),
        );

        let (h1, mut c1, ep1) = dial(&host, &me).await;
        let first = serve(&svc, h1, open(), me.key, vec![Permission::Terminal]);
        let session = opened(&mut c1).await;
        send_keys(&mut c1, "echo SET\"\"UP\n").await;
        let _ = read_until(&mut c1, "SETUP").await;
        drop(c1);
        drop(ep1);
        let _ = ok(first).await;
        assert_eq!(svc.parked().len(), 1);

        // Past the window.
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(
            svc.parked().len(),
            0,
            "an elapsed session must be reaped, not left in the registry"
        );

        // And an elapsed session is refused exactly like an unknown one.
        let (h2, mut c2, _ep2) = dial(&host, &me).await;
        let t = serve(
            &svc,
            h2,
            resume(session),
            me.key,
            vec![Permission::Terminal],
        );
        assert_eq!(
            started(&mut c2).await,
            TerminalStarted::Refused(Refusal::Unknown)
        );
        let _ = refused(t).await;
        std::fs::remove_dir_all(&dir).ok();
    })
    .await;
}

/// A job that finishes while nobody is attached keeps its exit status and its
/// last output, because that status is usually the whole reason someone
/// detached instead of waiting.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs and a real pty; MIRI has neither")]
async fn a_shell_that_exits_while_detached_keeps_its_status_and_last_words() {
    bounded("exit while detached", async {
        let (host, [me]) = fleet();
        let dir = tmpdir();
        let log = Log::at(dir.join("sessions.log"));
        let svc = service(&log, Limits::default(), TerminalPolicy::default());

        let (h1, mut c1, ep1) = dial(&host, &me).await;
        let first = serve(&svc, h1, open(), me.key, vec![Permission::Terminal]);
        let session = opened(&mut c1).await;
        send_keys(&mut c1, "echo SET\"\"UP\n").await;
        let _ = read_until(&mut c1, "SETUP").await;

        // Print something and exit with a status worth knowing, once the link
        // is gone. `sleep` gives the drop time to happen first.
        send_keys(&mut c1, "sleep 1; echo LAST-WOR\"\"DS; exit 42\n").await;
        let _ = read_until(&mut c1, "sleep 1").await;
        drop(c1);
        drop(ep1);
        let _ = ok(first).await;

        tokio::time::sleep(Duration::from_secs(3)).await;

        let (h2, mut c2, _ep2) = dial(&host, &me).await;
        let second = serve(
            &svc,
            h2,
            resume(session),
            me.key,
            vec![Permission::Terminal],
        );
        assert!(matches!(
            started(&mut c2).await,
            TerminalStarted::Resumed { .. }
        ));

        let seen = read_until(&mut c2, "LAST-WORDS").await;
        assert!(seen.contains("LAST-WORDS"), "{}", tail(&seen));

        let ended = ok(second).await;
        assert!(
            matches!(ended, Ended::ShellExited(x) if x.shell_code() == 42),
            "the exit status of a job someone detached from is the point of \
             detaching from it, got {ended:?}"
        );

        // A session that ended while parked never goes through the ordinary
        // pump, so its close record is easy to lose — and an open with no
        // close is exactly the gap an audit log exists not to have.
        let entries = log.read().expect("read the log");
        let closed = entries
            .iter()
            .find_map(|e| match &e.event {
                Event::TerminalClosed { exit_code, .. } => Some(*exit_code),
                _ => None,
            })
            .expect("a session that ended while parked must still be recorded as closed");
        assert_eq!(closed, Some(42), "with the status it ended with");
        assert!(
            entries
                .iter()
                .any(|e| matches!(e.event, Event::TerminalOpened)),
            "and its open: {entries:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    })
    .await;
}

/// A device that accumulates parked sessions loses its own oldest, not
/// somebody else's.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs and a real pty; MIRI has neither")]
async fn the_per_device_limit_evicts_that_devices_own_oldest_session() {
    bounded("per-device eviction", async {
        let (host, [me, other]) = fleet();
        let dir = tmpdir();
        let log = Log::at(dir.join("sessions.log"));
        let svc = service(
            &log,
            Limits {
                per_device: 2,
                ..Limits::default()
            },
            TerminalPolicy::default(),
        );

        // One session belonging to the other device, which must survive all of
        // this untouched.
        let (hb, mut cb, epb) = dial(&host, &other).await;
        let tb = serve(&svc, hb, open(), other.key, vec![Permission::Terminal]);
        let bystander = opened(&mut cb).await;
        send_keys(&mut cb, "echo SET\"\"UP\n").await;
        let _ = read_until(&mut cb, "SETUP").await;
        drop(cb);
        drop(epb);
        let _ = ok(tb).await;

        // Three of mine, into a limit of two.
        let park_mine = async || {
            let (h, mut c, ep) = dial(&host, &me).await;
            let t = serve(&svc, h, open(), me.key, vec![Permission::Terminal]);
            let id = opened(&mut c).await;
            send_keys(&mut c, "echo SET\"\"UP\n").await;
            let _ = read_until(&mut c, "SETUP").await;
            drop(c);
            drop(ep);
            let _ = ok(t).await;
            id
        };
        let oldest = park_mine().await;
        let _middle = park_mine().await;
        let newest = park_mine().await;

        assert_eq!(
            svc.parked().held_by(&me.key),
            2,
            "the limit must actually bind"
        );
        assert_eq!(
            svc.parked().held_by(&other.key),
            1,
            "one device filling up must never cost another device its session"
        );

        // The oldest of device 0's is the one that went.
        let (h, mut c, _ep) = dial(&host, &me).await;
        let t = serve(&svc, h, resume(oldest), me.key, vec![Permission::Terminal]);
        assert_eq!(
            started(&mut c).await,
            TerminalStarted::Refused(Refusal::Unknown),
            "the first session should have been evicted"
        );
        let _ = refused(t).await;

        // And the newest is still there.
        let (h, mut c, _ep) = dial(&host, &me).await;
        let t = serve(&svc, h, resume(newest), me.key, vec![Permission::Terminal]);
        assert!(matches!(
            started(&mut c).await,
            TerminalStarted::Resumed { .. }
        ));
        send_keys(&mut c, "exit 0\n").await;
        let _ = ok(t).await;

        // Named to say what it is: the count assertions above are the check
        // that it was never touched.
        let _ = bystander;
        svc.parked().revoke_all();
        std::fs::remove_dir_all(&dir).ok();
    })
    .await;
}

/// Idle means **no input and no output**. A session where nothing at all
/// happens ends when a timeout is set.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs and a real pty; MIRI has neither")]
async fn an_idle_session_ends_when_a_timeout_is_set() {
    bounded("idle timeout", async {
        let (host, [me]) = fleet();
        let dir = tmpdir();
        let log = Log::at(dir.join("sessions.log"));
        let svc = service(
            &log,
            Limits::default(),
            TerminalPolicy {
                idle_timeout: Some(Duration::from_millis(700)),
            },
        );

        let (h1, mut c1, _ep1) = dial(&host, &me).await;
        let first = serve(&svc, h1, open(), me.key, vec![Permission::Terminal]);
        let _ = opened(&mut c1).await;
        send_keys(&mut c1, "echo SET\"\"UP\n").await;
        let _ = read_until(&mut c1, "SETUP").await;

        // Type nothing.
        let ended = ok(first).await;
        assert_eq!(ended, Ended::Idle, "got {ended:?}");
        assert_eq!(
            svc.parked().len(),
            0,
            "an idle session is ended, not parked — parking it would defeat \
             the point of having a timeout"
        );
        std::fs::remove_dir_all(&dir).ok();
    })
    .await;
}

/// A build that prints nothing for a long time is not idle, it is working.
/// A timeout that killed it would be a data-loss bug wearing a security
/// feature's clothes — so output counts as activity, not just keystrokes.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs and a real pty; MIRI has neither")]
async fn a_session_producing_output_is_not_idle_however_little_is_typed() {
    bounded("output is activity", async {
        let (host, [me]) = fleet();
        let dir = tmpdir();
        let log = Log::at(dir.join("sessions.log"));
        let svc = service(
            &log,
            Limits::default(),
            TerminalPolicy {
                idle_timeout: Some(Duration::from_millis(700)),
            },
        );

        let (h1, mut c1, _ep1) = dial(&host, &me).await;
        let first = serve(&svc, h1, open(), me.key, vec![Permission::Terminal]);
        let _ = opened(&mut c1).await;

        // One command, then nothing typed for well over the timeout while the
        // shell keeps printing.
        send_keys(
            &mut c1,
            "i=0; while [ $i -lt 12 ]; do echo tick; sleep 0.2; i=$((i+1)); done; echo STILL-HE\"\"RE\n",
        )
        .await;

        let seen = read_until(&mut c1, "STILL-HERE").await;
        assert!(
            seen.contains("STILL-HERE"),
            "a session that is producing output must not be reaped as idle: {}",
            tail(&seen)
        );

        send_keys(&mut c1, "exit 0\n").await;
        let ended = ok(first).await;
        assert!(
            matches!(ended, Ended::ShellExited(_)),
            "the session should have ended by the user exiting, not by the \
             idle timeout, got {ended:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    })
    .await;
}

/// Coming back on a different-sized window must apply that size, or the shell
/// keeps drawing to the old width and every line wraps wrongly until the user
/// resizes by hand.
#[tokio::test]
#[cfg_attr(miri, ignore = "needs aws-lc-rs and a real pty; MIRI has neither")]
async fn resuming_on_a_different_window_applies_the_new_size() {
    bounded("resize on resume", async {
        let (host, [me]) = fleet();
        let dir = tmpdir();
        let log = Log::at(dir.join("sessions.log"));
        let svc = service(&log, Limits::default(), TerminalPolicy::default());

        let (h1, mut c1, ep1) = dial(&host, &me).await;
        let first = serve(&svc, h1, open(), me.key, vec![Permission::Terminal]);
        let session = opened(&mut c1).await;
        // `open()` asks for 80 columns.
        send_keys(&mut c1, "echo \"COLS=$(tput cols)\"\n").await;
        let seen = read_until(&mut c1, "COLS=80").await;
        assert!(seen.contains("COLS=80"), "{}", tail(&seen));
        drop(c1);
        drop(ep1);
        let _ = ok(first).await;

        // `resume()` asks for 100.
        let (h2, mut c2, _ep2) = dial(&host, &me).await;
        let second = serve(
            &svc,
            h2,
            resume(session),
            me.key,
            vec![Permission::Terminal],
        );
        assert!(matches!(
            started(&mut c2).await,
            TerminalStarted::Resumed { .. }
        ));

        send_keys(&mut c2, "echo \"NOW=$(tput cols)\"\n").await;
        let seen = read_until(&mut c2, "NOW=100").await;
        assert!(
            seen.contains("NOW=100"),
            "the window size the controller resumed with must be applied: {}",
            tail(&seen)
        );

        send_keys(&mut c2, "exit 0\n").await;
        let _ = ok(second).await;
        std::fs::remove_dir_all(&dir).ok();
    })
    .await;
}
