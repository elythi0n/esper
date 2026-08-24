//! Terminal mode, both halves: serving a shell and being the terminal.
//!
//! # Why this is a session of its own
//!
//! A terminal session starts no capture, no encoder and no presenter, and on a
//! headless server there is nothing for any of them to do. So a controller
//! asking for [`Permission::Terminal`] and not [`Permission::View`] gets a
//! session that never touches the media path at all — which is the whole
//! claim of terminal mode, and it would not be true if the host set up a
//! screen first and then answered a shell request on the side.
//!
//! Two modes at once — a screen and a shell over one connection — needs a way
//! to say which control stream is which, and the protocol has no message for
//! that yet. Until it does, a controller that wants both opens two
//! connections, which costs a handshake and nothing else.
//!
//! # What the controller has to get right
//!
//! Raw mode, and giving it back. Everything else here is pumping bytes; the
//! part that can leave a person with an unusable shell is the terminal
//! settings, which is why [`esper_pty::RawMode`] restores on `Drop` and
//! installs a panic hook as well.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use esper_crypto::identity::KEY_LEN;
use esper_proto::{
    Hello, Permission, Role, SessionId, SessionRequest, TerminalData, TerminalMessage,
    TerminalOpen, TerminalResume, TerminalStart, TerminalStarted,
};
use esper_pty::RawMode;
use esper_transport::{
    AnyOf, Control, Ended, Endpoint, EscapeWatcher, Grants, NoPeers, OnlyPeer, PeerAuthority,
    Session, TerminalService, Typed, request_session, send_resize,
};
use tokio::io::AsyncWriteExt as _;

use crate::{net, paths, session_cmd};

/// How long a host waits for a controller to say which shell it wants.
///
/// It has already been granted a session, so this is a peer that asked for a
/// terminal and then said nothing — holding a control stream and a slot. Short
/// for the same reason the session request's own wait is short.
const ASK_TIMEOUT: Duration = Duration::from_secs(10);

/// How much of the user's typing to carry in one message.
///
/// A person types a few bytes at a time and a paste arrives all at once, so
/// this is sized for the paste. Bounded because it becomes a protocol message.
const TYPING_CHUNK: usize = 4096;

/// How long either side waits for the other to close before giving up on it.
const GOODBYE: Duration = Duration::from_secs(2);

// ── the host's half ─────────────────────────────────────────────────────────

/// Serve one terminal session on a control stream.
///
/// The peer has already been granted its permissions. What is left is to hear
/// which shell it wants — a new one, or one it left behind — and hand the
/// stream to the service that owns them.
pub async fn serve(
    control: &mut Control,
    service: &TerminalService,
    peer_key: &[u8; KEY_LEN],
    granted: &dyn Grants,
) -> Result<Finished, String> {
    let start = match tokio::time::timeout(ASK_TIMEOUT, control.recv::<TerminalStart>()).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return Err(format!("reading the terminal request: {e}")),
        Err(_) => {
            return Err(format!(
                "the controller asked for a terminal and then said nothing for {ASK_TIMEOUT:?}"
            ));
        }
    };

    match service.serve(control, &start, peer_key, granted).await {
        // The exit status has just been queued on the stream, and the
        // controller is waiting for it.
        Ok(Ended::ShellExited(exit)) => Ok(Finished {
            what: format!("the shell exited ({})", describe(&exit)),
        }),
        Ok(Ended::Parked {
            session,
            deliberate,
        }) => Ok(Finished {
            what: format!(
                "the shell is still running, {} — resume it with `terminal --resume {session}`",
                if deliberate {
                    "detached"
                } else {
                    "waiting for the link to come back"
                }
            ),
        }),
        Ok(Ended::Disconnected) => Ok(Finished {
            what: "the controller went away".to_owned(),
        }),
        Ok(Ended::Idle) => Ok(Finished {
            what: "idle for as long as the policy allows".to_owned(),
        }),
        Err(e) => Err(e.to_string()),
    }
}

/// An exit status as a sentence.
fn describe(exit: &esper_proto::TerminalExit) -> String {
    match (exit.code, exit.signal) {
        (Some(c), _) => format!("code {c}"),
        (None, Some(s)) => format!("signal {s}"),
        (None, None) => "no status".to_owned(),
    }
}

/// How a terminal session ended, for the host's own console.
#[derive(Debug, Clone)]
pub struct Finished {
    /// What happened.
    pub what: String,
}

// ── the controller's half ───────────────────────────────────────────────────

/// What `terminal` was asked to do.
#[derive(Debug, Clone)]
pub struct Options {
    /// The host to reach.
    pub addr: SocketAddr,
    /// A session left behind, to return to rather than opening a new shell.
    pub resume: Option<SessionId>,
}

/// Parse `terminal <addr> [--resume <id>]`.
pub fn parse(args: &[String]) -> Result<Options, String> {
    let mut addr: Option<SocketAddr> = None;
    let mut resume: Option<SessionId> = None;
    let mut rest = args.iter();
    while let Some(a) = rest.next() {
        match a.as_str() {
            "--resume" => {
                let hex = rest
                    .next()
                    .ok_or_else(|| "--resume needs the session id the host printed".to_owned())?;
                resume = Some(parse_session_id(hex)?);
            }
            other if !other.starts_with('-') && addr.is_none() => {
                addr = Some(net::parse_addr(other)?);
            }
            other => return Err(format!("unknown option {other:?}")),
        }
    }
    let addr = addr.ok_or_else(|| {
        format!(
            "terminal needs the host's address: {} terminal <addr> [--resume <id>]",
            crate::net::me()
        )
    })?;
    Ok(Options { addr, resume })
}

/// Read a session id from the hex the host printed.
fn parse_session_id(hex: &str) -> Result<SessionId, String> {
    let clean: String = hex.trim().chars().filter(|c| !c.is_whitespace()).collect();
    if clean.len() != esper_proto::SESSION_ID_LEN * 2 {
        return Err(format!(
            "a session id is {} hex characters; {:?} has {}",
            esper_proto::SESSION_ID_LEN * 2,
            clean,
            clean.len()
        ));
    }
    let mut bytes = [0u8; esper_proto::SESSION_ID_LEN];
    for (i, b) in bytes.iter_mut().enumerate() {
        let pair = clean
            .get(i * 2..i * 2 + 2)
            .ok_or_else(|| "a session id is hex".to_owned())?;
        *b = u8::from_str_radix(pair, 16).map_err(|_| format!("{pair:?} is not hex"))?;
    }
    Ok(SessionId::from_bytes(bytes))
}

/// Open a shell on a paired host and be its terminal.
pub fn run(o: &Options) -> Result<(), String> {
    net::install_crypto();
    let identity = paths::identity()?;
    let (store, keys) = session_cmd::paired()?;

    let dial: Arc<dyn PeerAuthority> = if let [only] = keys.as_slice() {
        Arc::new(OnlyPeer::new(*only))
    } else {
        Arc::new(AnyOf::new(keys.clone()))
    };

    println!("⌨️  esper terminal {}", o.addr);
    println!();

    net::runtime()?.block_on(async {
        let endpoint = Endpoint::bind(&identity, net::any_port(o.addr), Arc::new(NoPeers), dial)
            .map_err(|e| format!("binding a local port: {e}"))?;
        let conn = endpoint.connect(o.addr).await.map_err(|e| e.to_string())?;
        let mut session = Session::open(
            conn,
            Hello::new(Role::Controller, identity.public_key(), &net::this_device()),
        )
        .await
        .map_err(|(e, _)| crate::session_cmd::opening_failed(e))?;

        let known = store
            .iter()
            .find(|d| d.public_key == *session.peer_key())
            .map(|d| d.name.clone());
        println!(
            "  host       {}",
            known.unwrap_or_else(|| "unknown".to_owned())
        );
        println!("      {}", session.peer_fingerprint().hex());

        let mut control = session
            .take_control()
            .ok_or_else(|| "the session has no control stream".to_owned())?;

        // **Terminal and nothing else.** Asking for view as well would make
        // the host set up a screen for a session that will never look at one.
        let grant = request_session(
            &mut control,
            &SessionRequest {
                kind: esper_proto::SessionKind::Terminal,
                want: vec![Permission::Terminal],
                codecs: Vec::new(),
                reason: "a shell".to_owned(),
            },
        )
        .await
        .map_err(|e| match e {
            esper_transport::AuthorizeError::Refused(d) => {
                format!(
                    "the host refused: {} ({})",
                    d.detail,
                    d.reason
                        .describe()
                        .unwrap_or("no description for that reason")
                )
            }
            other => format!("asking for a terminal: {other}"),
        })?;
        if !grant.allows(Permission::Terminal) {
            return Err(
                "the host granted a session but not a terminal, so there is nothing to do"
                    .to_owned(),
            );
        }

        // **This machine's own record of where it opened a shell.** The host
        // has recorded the other side of this since Phase 2; without this
        // half, the trail on a laptop can say who came in and not where its
        // owner went.
        let far_end = *session.peer_key();
        crate::trail::went_out(&far_end, esper_audit::Event::TerminalOpened);
        let opened_at = std::time::Instant::now();

        let (cols, rows) = RawMode::window_size();
        let term = std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".to_owned());
        let start = match o.resume {
            Some(session) => TerminalStart::Resume(TerminalResume {
                session,
                cols,
                rows,
            }),
            None => TerminalStart::Open(TerminalOpen {
                cols,
                rows,
                term: term.clone(),
            }),
        };
        control
            .send(&start)
            .await
            .map_err(|e| format!("asking for a shell: {e}"))?;

        let session_id = match control
            .recv::<TerminalStarted>()
            .await
            .map_err(|e| format!("waiting for the host's answer: {e}"))?
        {
            TerminalStarted::Opened { session } => {
                println!("  shell      new session {session}");
                session
            }
            TerminalStarted::Resumed { session, lost } => {
                println!("  shell      back on {session}");
                if lost > 0 {
                    println!("      · {lost} bytes of output were lost while nobody was attached");
                }
                session
            }
            TerminalStarted::Refused(why) => {
                return Err(format!("the host refused: {}", why.message()));
            }
        };
        println!("  term       {term}, {cols}x{rows}");
        println!("  detach     press Enter, then ~ .");
        println!();

        let outcome = pump(&mut control, session_id, &session).await;

        // And how long it lasted, whichever way it ended. The trail records
        // the shape of what happened, never a keystroke of it.
        crate::trail::went_out(
            &far_end,
            esper_audit::Event::TerminalClosed {
                duration_secs: opened_at.elapsed().as_secs(),
                // The shell's status belongs to the host, which is where the
                // shell ran; this end knows only that its session ended.
                exit_code: None,
                exit_signal: None,
            },
        );

        // **A close is a message, and a message has to be delivered.**
        // `finish` and `close` only *queue* their frames; a process that
        // exits here never sends them, so a deliberate detach looks to the
        // host like a peer that vanished — and the host parks the shell as a
        // dropped link rather than as somebody stepping away. The host closes
        // the connection as soon as it has read the finish, so this returns
        // in a round trip rather than waiting out the whole grace.
        let _ = tokio::time::timeout(GOODBYE, endpoint.wait_idle()).await;
        outcome
    })
}

/// What ended the terminal.
enum Over {
    /// The shell exited, with its status.
    Exited(esper_proto::TerminalExit),
    /// The user asked to detach.
    Detached,
    /// The link or the host went away.
    Lost(String),
}

/// Be the terminal: keystrokes out, output in, until something ends it.
async fn pump(control: &mut Control, session: SessionId, link: &Session) -> Result<(), String> {
    // **Raw mode before the first keystroke, restored however this ends.**
    // The guard restores on `Drop`, which covers an early return and a panic;
    // the hook covers a panic that aborts.
    RawMode::install_panic_hook();
    let mut raw =
        RawMode::enable().map_err(|e| format!("putting the terminal in raw mode: {e}"))?;

    let mut keys = keystrokes();
    // **Standard input ending is not the session ending.** A script piping
    // commands in runs out of them immediately, and the output of the last one
    // has not arrived yet — so the typing side closes and the session carries
    // on until the shell exits. `ssh` does the same thing, and for the same
    // reason.
    //
    // What it cannot do is *tell* the shell that input ended: the protocol has
    // no message for a half-closed terminal, so a piped script that never
    // exits leaves a shell waiting for input until the host's idle timeout.
    // Ending the script with `exit` is the answer until there is a message
    // for it.
    let mut typing = true;
    let mut out = tokio::io::stdout();
    let mut watcher = EscapeWatcher::new();
    let mut winch = window_changes();

    let over = loop {
        tokio::select! {
            typed = keys.recv(), if typing => match typed {
                Some(bytes) => {
                    match watcher.feed(&bytes) {
                        Typed::Detach => break Over::Detached,
                        Typed::Send(bytes) if bytes.is_empty() => {}
                        Typed::Send(bytes) => {
                            if let Err(e) = control
                                .send(&TerminalMessage::Data(TerminalData { bytes }))
                                .await
                            {
                                break Over::Lost(format!("sending: {e}"));
                            }
                        }
                    }
                }
                None => {
                    typing = false;
                    // A held `~` is a real keystroke the user typed and
                    // nobody has decided what it meant. Sent, not dropped.
                    if let Some(b) = watcher.take_pending() {
                        let _ = control
                            .send(&TerminalMessage::Data(TerminalData { bytes: vec![b] }))
                            .await;
                    }
                }
            },
            message = control.recv::<TerminalMessage>() => match message {
                Ok(TerminalMessage::Data(d)) => {
                    if out.write_all(&d.bytes).await.is_err() || out.flush().await.is_err() {
                        break Over::Lost("writing to this terminal failed".to_owned());
                    }
                }
                Ok(TerminalMessage::Exit(exit)) => break Over::Exited(exit),
                // Controller to host only. A host sending one is confused.
                Ok(TerminalMessage::Resize(_)) => {}
                Err(e) => break Over::Lost(e.to_string()),
            },
            Some(()) = winch.recv() => {
                let (cols, rows) = RawMode::window_size();
                if let Err(e) = send_resize(control, cols, rows).await {
                    break Over::Lost(format!("sending a resize: {e}"));
                }
            }
        }
    };

    // The terminal comes back **before** anything is printed about what
    // happened, so the summary is not written into a raw-mode terminal that
    // does not move the cursor to the start of the line.
    raw.restore();
    println!();

    match over {
        Over::Exited(exit) => {
            println!("  the shell exited: {}", describe(&exit));
            Ok(())
        }
        Over::Detached => {
            // Finishing the send side is what tells the host this was
            // deliberate: it parks the shell rather than tearing it down.
            let _ = control.finish();
            println!("  detached — the shell is still running on the host");
            println!(
                "  come back with:  {} terminal --resume {session}",
                crate::net::me()
            );
            Ok(())
        }
        // **A lost link usually said why.** A host that ended the session
        // deliberately — the operator revoked this device, hit the panic key,
        // or is shutting down — closed the connection with a reason code, and
        // reporting the symptom instead ("connection lost") throws away the
        // one thing the person needs.
        Over::Lost(why) => Err(match link.peer_close_reason() {
            Some((code, text)) => {
                let described = code.describe().unwrap_or(&text);
                format!("the host ended the session: {described} ({})", code.0)
            }
            None => why,
        }),
    }
}

/// Standard input, read on a thread and delivered as chunks.
///
/// A thread rather than `tokio::io::stdin()` inside the `select!`: a read of
/// tokio's stdin is **not cancel-safe**, and a `select!` that drops it can
/// lose bytes the blocking side has already taken — which in a terminal is a
/// keystroke that vanished. A channel is cancel-safe by construction.
fn keystrokes() -> tokio::sync::mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    std::thread::spawn(move || {
        use std::io::Read as _;
        let mut stdin = std::io::stdin();
        let mut buf = vec![0u8; TYPING_CHUNK];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => {
                    let chunk = buf.get(..n).unwrap_or(&[]).to_vec();
                    if tx.blocking_send(chunk).is_err() {
                        return;
                    }
                }
            }
        }
    });
    rx
}

/// Window-size changes, as a channel so the pump has one shape.
///
/// A platform with no such signal simply never sends: the shell then keeps the
/// size it was started with, which is what a terminal that cannot be resized
/// looks like anyway.
fn window_changes() -> tokio::sync::mpsc::Receiver<()> {
    let (tx, rx) = tokio::sync::mpsc::channel::<()>(1);

    // Dropping the sender closes the channel, and a closed channel makes the
    // pump's `Some(()) = winch.recv()` branch stop matching — so the branch
    // disables itself rather than spinning.
    #[cfg(not(unix))]
    drop(tx);

    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut sig) = signal(SignalKind::window_change()) {
            tokio::spawn(async move {
                while sig.recv().await.is_some() {
                    // A full channel means a resize is already pending and the
                    // pump has not read it yet; the newest size is read at
                    // that point, so dropping this one loses nothing.
                    if tx.try_send(()).is_err() && tx.is_closed() {
                        return;
                    }
                }
            });
        }
    }
    rx
}

/// What `terminal` takes.
pub fn usage() -> String {
    let me = crate::net::me();
    format!(
        "usage: {me} terminal <addr> [--resume <session>]\n\
         \n\
         Open a shell on a paired host. The host decides whether it may be\n\
         asked at all, and a session it allows is recorded on its own trail.\n\
         \n\
         \x20 --resume <session>   rejoin a session the host parked, by the id\n\
         \x20                      it printed when the link dropped\n\
         \n\
         for example: {me} terminal 192.168.1.24:7420"
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn a_session_id_round_trips_through_the_hex_the_host_prints() {
        let id = SessionId::from_bytes([
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ]);
        let printed = id.to_string();
        assert_eq!(parse_session_id(&printed).unwrap(), id);
        // Whitespace a person copied along with it.
        assert_eq!(parse_session_id(&format!(" {printed}\n")).unwrap(), id);
    }

    #[test]
    fn a_session_id_that_is_not_one_is_refused_with_its_length() {
        let e = parse_session_id("abcd").unwrap_err();
        assert!(e.contains("32 hex characters"), "{e}");
        let e = parse_session_id(&"z".repeat(32)).unwrap_err();
        assert!(e.contains("not hex"), "{e}");
    }

    #[test]
    fn terminal_needs_an_address_and_takes_a_session_to_resume() {
        assert!(parse(&[]).is_err());
        let o = parse(&["127.0.0.1:7645".to_owned()]).unwrap();
        assert!(o.resume.is_none());

        let id = SessionId::from_bytes([7u8; 16]);
        let o = parse(&[
            "127.0.0.1:7645".to_owned(),
            "--resume".to_owned(),
            id.to_string(),
        ])
        .unwrap();
        assert_eq!(o.resume, Some(id));

        // A flag with nothing after it is a mistake worth naming.
        assert!(parse(&["127.0.0.1:7645".to_owned(), "--resume".to_owned()]).is_err());
    }
}
