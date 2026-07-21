//! `esper-engine host` and `esper-engine connect` — a video session
//! between two machines.
//!
//! # Who may connect
//!
//! Only a paired device. The device list is consulted during the **TLS
//! handshake**, so an unpaired peer never reaches session code — it does not
//! get to send a `Hello`, open a stream, or occupy a task. That is the whole
//! reason the check lives in the transport rather than here.
//!
//! # Two threads on the host, and why
//!
//! Capture blocks. So the capture-and-encode-and-send loop gets a thread, and
//! the async side serves the control stream. They talk over one channel
//! carrying the controller's messages *towards* the pipeline, because those
//! are the only thing that crosses: a keyframe request has to reach the
//! encoder, and nothing the encoder produces needs to reach the async side.
//!
//! One direction of one channel is a small enough seam to be sure about.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use esper_audit::{Entry, Event, Log};
use esper_capture::gate::Policy;
use esper_crypto::DeviceStore;
use esper_encode::budget::Budget;
use esper_encode::encoder::{EncodeError, EncoderConfig, VideoEncoder};
use esper_encode::openh264::OpenH264;
use esper_link::{ControllerLink, HostLink, parts};
use esper_pipeline::pipeline::Pipeline;
use esper_proto::{Hello, MediaMessage, Permission, Role};
use esper_scale::Ceiling;
use esper_stream::{Decoded, Expired, Handled, HostStep, PARK_TIMEOUT, Parked};
use esper_transport::{
    Answered, AnyOf, Control, Endpoint, GrantStoredPermissions, NoPeers, OnlyPeer, Session,
    TerminalPolicy, TerminalService, answer_session,
};

use crate::backend::Choice;
use crate::{net, paths};

/// What `host` was asked to do.
#[derive(Debug, Clone, Copy)]
pub struct HostOptions {
    pub listen: SocketAddr,
    pub backend: Choice,
    pub monitor: Option<u32>,
    pub fps: u32,
    pub budget: f64,
    pub max_resolution: Option<u32>,
    /// Serve one session and exit, which is what a test wants.
    pub once: bool,
    /// Advertise on the LAN. On by default, and the one deliberate exception
    /// to strict defaults: it reveals that a machine runs Esper to a network
    /// segment that is usually the operator's own house, and it buys the best
    /// thing about the product on a home network.
    pub mdns: bool,
}

impl Default for HostOptions {
    fn default() -> Self {
        Self {
            listen: SocketAddr::from(([0, 0, 0, 0], net::DEFAULT_PORT)),
            backend: Choice::default(),
            monitor: None,
            fps: 30,
            budget: Budget::default().cores,
            max_resolution: Ceiling::default().long_edge(),
            once: false,
            mdns: true,
        }
    }
}

/// What `connect` was asked to do.
#[derive(Debug, Clone, Copy)]
pub struct ConnectOptions {
    pub addr: SocketAddr,
    pub seconds: u64,
    pub present: bool,
    /// Send this window's keys and pointer to the host.
    pub control: bool,
    /// A monitor to ask the host for, when it is not the one announced.
    pub monitor: Option<u32>,
}

/// Parse `host` flags.
pub fn parse_host(args: &[String]) -> Result<HostOptions, String> {
    let mut o = HostOptions::default();
    let mut i = 0usize;
    while let Some(arg) = args.get(i) {
        let value = |name: &str| -> Result<String, String> {
            args.get(i + 1)
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match arg.as_str() {
            "--listen" => {
                o.listen = net::parse_listen(&value("--listen")?)?;
                i += 2;
            }
            "--backend" => {
                o.backend = Choice::parse(&value("--backend")?)?;
                i += 2;
            }
            "--monitor" => {
                o.monitor = Some(
                    value("--monitor")?
                        .parse()
                        .map_err(|_| "--monitor wants a monitor number".to_owned())?,
                );
                i += 2;
            }
            "--fps" => {
                o.fps = value("--fps")?
                    .parse()
                    .map_err(|_| "--fps wants a number".to_owned())?;
                i += 2;
            }
            "--budget" => {
                o.budget = value("--budget")?
                    .parse()
                    .map_err(|_| "--budget wants a number of cores".to_owned())?;
                i += 2;
            }
            "--max-resolution" => {
                let raw = value("--max-resolution")?;
                o.max_resolution = if raw == "none" {
                    None
                } else {
                    Some(raw.parse().map_err(|_| {
                        "--max-resolution wants pixels on the long edge, or `none`".to_owned()
                    })?)
                };
                i += 2;
            }
            "--once" => {
                o.once = true;
                i += 1;
            }
            "--no-mdns" => {
                o.mdns = false;
                i += 1;
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }
    Ok(o)
}

/// Parse `connect` flags.
pub fn parse_connect(args: &[String]) -> Result<ConnectOptions, String> {
    let mut addr: Option<SocketAddr> = None;
    let mut seconds = 0u64;
    let mut present = false;
    let mut control = false;
    let mut monitor: Option<u32> = None;
    let mut i = 0usize;
    while let Some(arg) = args.get(i) {
        match arg.as_str() {
            "--seconds" => {
                seconds = args
                    .get(i + 1)
                    .ok_or_else(|| "--seconds needs a number".to_owned())?
                    .parse()
                    .map_err(|_| "--seconds wants a number".to_owned())?;
                i += 2;
            }
            "--present" => {
                present = true;
                i += 1;
            }
            "--control" => {
                control = true;
                i += 1;
            }
            "--monitor" => {
                monitor = Some(
                    args.get(i + 1)
                        .ok_or_else(|| "--monitor needs a monitor number".to_owned())?
                        .parse()
                        .map_err(|_| "--monitor wants a monitor number".to_owned())?,
                );
                i += 2;
            }
            other if !other.starts_with('-') && addr.is_none() => {
                addr = Some(net::parse_addr(other)?);
                i += 1;
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }
    Ok(ConnectOptions {
        addr: addr.ok_or("connect needs the host's address, as `connect <address>`")?,
        seconds,
        present,
        control,
        monitor,
    })
}

fn encoder_for(config: &EncoderConfig) -> Result<Box<dyn VideoEncoder>, EncodeError> {
    Ok(Box::new(OpenH264::new(config)?))
}

/// The paired devices, or a message saying there are none.
///
/// A host with nothing paired is refused rather than started, because a
/// listening endpoint that can accept nobody is a listening endpoint whose
/// only possible outcome is a confusing failure at the far end.
pub(crate) fn paired() -> Result<(DeviceStore, Vec<[u8; 32]>), String> {
    let (store, dropped) = paths::devices()?;
    for note in &dropped {
        eprintln!("  · the device list needed correcting: {note}");
    }
    let keys: Vec<[u8; 32]> = store.iter().map(|d| d.public_key).collect();
    if keys.is_empty() {
        return Err("no devices are paired, so nobody could connect. Run \
             `esper-engine pair --listen 7645` and pair one first"
            .to_owned());
    }
    Ok((store, keys))
}

/// Serve video to whoever connects.
pub fn run_host(o: &HostOptions) -> Result<(), String> {
    net::install_crypto();
    let identity = paths::identity()?;
    let (store, keys) = paired()?;

    // Bound inside the runtime: quinn needs one, and binding outside it fails
    // with "no async runtime found", which says nothing about the cause.
    let runtime = net::runtime()?;
    let (stopping, permits, endpoint) = runtime.block_on(async {
        // Both of these are wanted *before* the endpoint exists: the stop
        // signal so a `SIGTERM` during startup is honoured, and the watched
        // store because it is what decides which keys may connect at all.
        let stopping = net::stop_signal()?;
        let permits = crate::permits::watch(stopping.clone());
        let endpoint = Endpoint::bind(
            &identity,
            o.listen,
            // **Not the list this process started with.** `AnyOf` is a
            // snapshot, so a device forgotten while the host runs could go on
            // completing handshakes until it was restarted — authenticating a
            // device this machine no longer knows. Reading the same watched
            // store the sessions read means `forget` reaches the TLS layer
            // within the poll interval and without a restart.
            Arc::new(PairedNow(permits.clone())),
            Arc::new(NoPeers),
        )
        .map_err(|e| format!("binding {}: {e}", o.listen))?;
        Ok::<_, String>((stopping, permits, endpoint))
    })?;
    let bound = endpoint
        .local_addr()
        .map_err(|e| format!("reading the bound address: {e}"))?;

    println!("📺 esper host");
    println!();
    println!("  listening  {bound}");
    println!("  paired     {} device(s)", keys.len());
    for device in store.iter() {
        println!("      {}  {}", device.fingerprint().hex(), device.name);
    }
    println!();

    // **The indicator and the panic key, before any session.** Not a setting
    // and not a feature: software that can watch a screen has to make being
    // watched obvious, and software that can drive a keyboard has to leave the
    // person in front of it a way to stop that.
    //
    // A host that cannot provide either says so loudly rather than serving a
    // session quietly, because the thing it is failing to provide is
    // something a person relies on.
    let (indicator, mut presses) = match crate::presence_task::start() {
        Ok((i, presses)) => {
            let panic_key = if i.armed() {
                "Ctrl+Alt+Shift+Esc ends every session and locks the screen"
            } else {
                "PANIC KEY UNAVAILABLE: something else on this display holds it"
            };
            println!("  presence   {} — {panic_key}", i.backend());
            if !i.can_show() {
                println!(
                    "             ⚠ no session indicator: this display has no SHAPE extension"
                );
            }
            println!();
            (Some(i), Some(presses))
        }
        Err(e) => {
            println!("  presence   ⚠ NONE ({e})");
            println!("             This machine cannot show that it is being watched and has no");
            println!("             panic key. Sessions will still be served.");
            println!();
            (None, None)
        }
    };

    // **The audit trail, opened before any session.** A host that recorded
    // nothing would be a host whose owner cannot answer "who was on this
    // machine, and when" — which is a question people ask after the fact,
    // when it is too late to start recording.
    let log_path = paths::audit_log()?;
    println!("  audit      {}", log_path.display());
    let log = Log::at(log_path);

    // **Terminal sessions outlive their connections**, so the service that
    // owns them belongs to the host and not to one session: a shell parked
    // when a link dropped is resumed by the *next* connection from the same
    // device, and a service created per session would have nowhere to keep it.
    let terminals = Arc::new(TerminalService::new(
        log.clone(),
        esper_transport::Limits::default(),
        TerminalPolicy::default(),
    ));
    println!();

    // A session whose transport went away, waiting for its controller to come
    // back. Held here rather than inside `serve_one` because it has to outlive
    // the session it came from — that is the whole point of it.
    let mut parked: Option<Parked> = None;

    runtime.block_on(async {
        // Advertised while listening, and taken back when this stops. Without
        // the goodbye, every browser on the segment keeps offering a machine
        // that has gone until the record expires, and somebody clicks a device
        // that is not there.
        let mdns = if o.mdns {
            match crate::discover_cmd::advertise(bound).await {
                Ok(handle) => {
                    println!("  announcing on this network as {}", handle.instance());
                    println!();
                    Some(handle)
                }
                Err(e) => {
                    // Not fatal: a host reachable by address works without
                    // discovery, and saying so beats refusing to start.
                    println!("  not announcing on this network ({e})");
                    println!();
                    None
                }
            }
        } else {
            None
        };

        // Every loop that can wait a long time waits on this too. See
        // `net::stop_signal`: noticing a signal in one place and not another
        // is worse than not noticing it at all.
        let mut stopping = stopping;

        // **A parked shell belongs to a device, whether or not anybody is
        // connected.** Revoking the terminal permission of a device that is
        // not connected right now would otherwise leave its shell standing
        // for the fifteen seconds of the park timeout — and leave it
        // resumable by that device in the meantime, which is the part that
        // matters.
        tokio::spawn(drop_parked_when_revoked(
            Arc::clone(&terminals),
            permits.clone(),
        ));

        let outcome = loop {
            // Let go of a parked session nobody came back for, before waiting
            // for a connection that may never arrive. Otherwise a host that
            // sees no more traffic holds a capture backend open for as long as
            // it runs.
            if let Some(p) = parked.take_if(|p| p.is_expired(Instant::now())) {
                let (w, h) = p.encode_size();
                println!(
                    "  · let go of the parked {w}x{h} session: {}",
                    Expired::TimedOut.describe()
                );
                drop(p.release());
            }

            // A parked session has to expire on a clock, not on somebody
            // happening to connect. Waiting for a connection with no deadline
            // would hold the capture backend — and on a host, the screen being
            // captured — open for as long as the host runs, which is the whole
            // thing `PARK_TIMEOUT` exists to prevent.
            // A panic waits like a stop does — a person who hit it while
            // nobody was connected still wants the screen locked, and a
            // parked session let go of.
            let accepted =
                match accept_deadline(parked.as_ref().map(|p| p.parked_for(Instant::now()))) {
                    Some(left) => tokio::select! {
                        _ = stopping.changed() => break Ok(()),
                        () = wait_for_panic(presses.as_mut()) => {
                            panic_now(indicator.as_ref());
                            continue;
                        }
                        waited = tokio::time::timeout(left, endpoint.accept()) => match waited {
                            Ok(accepted) => accepted,
                            // Nobody came. Round the loop, where it is let go.
                            Err(_) => continue,
                        },
                    },
                    None => tokio::select! {
                        _ = stopping.changed() => break Ok(()),
                        () = wait_for_panic(presses.as_mut()) => {
                            panic_now(indicator.as_ref());
                            continue;
                        }
                        accepted = endpoint.accept() => accepted,
                    },
                };
            let conn = match accepted {
                Some(Ok(c)) => c,
                Some(Err(e)) => {
                    // A refused or broken handshake is one peer, not the end
                    // of the host. Reported and waited past.
                    eprintln!("  · a connection failed: {e}");
                    continue;
                }
                None => break Ok(()),
            };
            // `Session::open` is done here rather than inside `serve_one` so
            // that nothing in this crate has to name a `quinn::Connection`.
            // The transport's choice of QUIC is the transport's business.
            let session = match Session::open(
                conn,
                Hello::new(Role::Host, identity.public_key(), &net::this_device()),
            )
            .await
            {
                Ok(s) => s,
                Err((e, _)) => {
                    eprintln!("  · a session failed to open: {e}");
                    continue;
                }
            };
            let from = session.remote_address();
            println!("  · {from} connected");
            match serve_one(
                session,
                o,
                parked.take(),
                stopping.clone(),
                Machine {
                    indicator: indicator.as_ref(),
                    presses: presses.as_mut(),
                    terminals: &terminals,
                    log: &log,
                    permits: permits.clone(),
                },
            )
            .await
            {
                Ok((stats, still)) => {
                    println!("  · {from} finished: {stats}");
                    parked = still;
                }
                Err((e, still)) => {
                    eprintln!("  · {from} ended: {e}");
                    parked = still;
                }
            }
            if o.once {
                break Ok(());
            }
        };

        // Nothing is coming back to a host that is exiting, so the reason
        // travels rather than the session simply vanishing.
        if let Some(p) = parked.take() {
            println!(
                "      the parked session ends here: {}",
                Expired::HostGone.describe()
            );
            drop(p.release());
        }
        if let Some(mdns) = mdns {
            mdns.stop().await;
        }
        // The close frames of any session that just ended are queued, not
        // sent. Exiting here would leave a controller waiting out its idle
        // timeout for a host that told it exactly why it was going.
        let _ = tokio::time::timeout(Duration::from_secs(2), endpoint.wait_idle()).await;
        outcome
    })
}

/// Wait for something to say about the pointer, or for ever when nothing is
/// watching it.
///
/// The same shape as [`wait_for_panic`], and for the same reason: a `select!`
/// branch needs a future either way, and a host with no cursor source must not
/// fall out of its loop immediately.
async fn wait_for_cursor(
    pointer: Option<&mut crate::cursor_task::Messages>,
) -> Option<MediaMessage> {
    match pointer {
        Some(queue) => queue.recv().await.map(MediaMessage::Cursor),
        None => std::future::pending().await,
    }
}

/// Wait for the panic key, or for ever when there is none.
///
/// A `select!` branch needs a future either way, and a host with no working
/// panic key must not fall out of its loop immediately — so the absence is a
/// future that never completes rather than a special case at every call site.
async fn wait_for_panic(presses: Option<&mut crate::presence_task::Presses>) {
    match presses {
        // One press, taken from the queue: acted on once by whoever was
        // waiting. See `presence_task::Presses` for what the first version of
        // this did instead.
        Some(queue) => {
            let _ = queue.recv().await;
        }
        None => std::future::pending().await,
    }
}

/// Act on the panic key: say so, and lock the screen.
///
/// Ending the sessions is the caller's part — it is the caller that holds
/// them — and it happens whatever this does, because a machine with no screen
/// locker is still a machine somebody wants to stop being watched.
fn panic_now(indicator: Option<&crate::presence_task::Indicator>) {
    println!();
    println!("  ⚠ PANIC KEY: ending every session and locking the screen");
    if let Some(i) = indicator {
        i.hide();
    }
    match esper_presence::lock_screen() {
        Ok(how) => println!("    locked the screen with {how}"),
        // Said, not swallowed. Somebody who believes their screen is locked
        // and is wrong is worse off than somebody who has been told.
        Err(e) => println!("    ⚠ COULD NOT LOCK THE SCREEN: {e}"),
    }
    println!();
}

/// How long to wait for the next connection: forever, or until the parked
/// session expires.
///
/// Without a deadline here, `accept` blocks indefinitely and a parked session
/// is released only when somebody happens to connect — so a host nobody comes
/// back to holds a capture backend, and the screen it is capturing, for as
/// long as it runs. Pure, because arithmetic behind a network is arithmetic
/// nothing tests.
fn accept_deadline(parked_for: Option<Duration>) -> Option<Duration> {
    parked_for.map(|waited| PARK_TIMEOUT.saturating_sub(waited))
}

/// Serve exactly one session, resuming `parked` if this is who it belongs to.
///
/// Returns whatever is left parked afterwards, either untouched because
/// somebody else connected or newly parked because this transport broke.
// A parked session travels on both arms, because a session that failed is
// exactly the one worth keeping. Boxing it would allocate on the path whose
// job is not to lose somebody's session.
#[allow(clippy::result_large_err)]
/// What to tell somebody whose device the host would not accept.
///
/// The transport already distinguishes "you were refused" from "the network
/// failed"; this turns the first into something to *do*. A user told
/// "connection lost" checks their router; a user told the host does not know
/// this device pairs again.
pub(crate) fn opening_failed(e: esper_transport::SessionError) -> String {
    match e {
        esper_transport::SessionError::Refused(why) => [
            format!("this host does not accept this device ({why})."),
            "  It may have been unpaired there. Pair again, on both machines:".to_owned(),
            "      esper-engine pair --listen <addr>            (on the host)".to_owned(),
            "      esper-engine pair <host-addr> --code <code>  (here)".to_owned(),
        ]
        .join("\n"),
        other => format!("opening the session: {other}"),
    }
}

/// The codec this build can decode, by the name the protocol uses.
///
/// One entry, because software H.264 is what Phase 3 shipped. It is named
/// rather than left empty: `codecs` is how a controller says what it can
/// render, and a controller that says nothing is asking the host to guess.
const fn config_codec_name() -> &'static str {
    esper_proto::Codec::H264.name()
}

/// How long a host waits for a controller to close after being told the last
/// thing it needs to hear.
///
/// Long enough for a round trip on any link this product is used over, short
/// enough that a controller which never closes costs one session's worth of
/// waiting and not the host.
const GOODBYE: Duration = Duration::from_secs(2);

/// Agree what this session may do.
///
/// The store is read **per session**, not once at startup: a permission
/// granted or taken away while the host is running has to apply to the next
/// connection, and a host that had to be restarted for that would be a host
/// whose permissions nobody trusts.
async fn agree(control: &mut Control, device: &[u8; 32]) -> Result<Vec<Permission>, String> {
    let (store, _) = paths::devices()?;
    let record = store
        .get(device)
        .ok_or_else(|| "this device is not in the paired list any more".to_owned())?;
    let id = esper_transport::parked::new_session_id()
        .map_err(|e| format!("no randomness for a session identifier: {e}"))?;

    match answer_session(control, &GrantStoredPermissions, record, *id.as_bytes()).await {
        Ok(Answered::Granted(g)) => Ok(g.granted),
        // The peer has already been told the same reason.
        Ok(Answered::Denied(d)) => Err(format!("refused: {}", d.detail)),
        Err(e) => Err(format!("agreeing what the session may do: {e}")),
    }
}

/// The keys this machine is paired with, as they are **now**.
///
/// `AnyOf` answers from a list taken when it was built, which is the wrong
/// answer for a host that runs for weeks: a device unpaired in the meantime
/// would still be authenticated by the TLS layer, and only refused a session
/// afterwards. Reading the watched store instead means the two layers agree.
#[derive(Debug)]
struct PairedNow(crate::permits::Watching);

impl esper_transport::PeerAuthority for PairedNow {
    fn is_authorized(&self, public_key: &[u8; 32]) -> bool {
        // Called during the handshake, so: no I/O and no blocking. A watch
        // borrow reads a lock the watcher holds only for the length of a
        // swap, and the answer is a lookup in a list of a handful of devices.
        self.0.borrow().of(public_key).is_some()
    }

    fn describe(&self) -> &'static str {
        "any device in this machine's paired list, as it stands now"
    }
}

/// Drop parked shells belonging to devices that may no longer have one.
///
/// The other half of [`close_when_revoked`], which only runs while a session
/// is live. Between them, revoking the terminal permission reaches every shell
/// that device has on this machine.
async fn drop_parked_when_revoked(
    terminals: Arc<TerminalService>,
    mut permits: crate::permits::Watching,
) {
    let mut before = Arc::clone(&*permits.borrow());
    loop {
        if permits.changed().await.is_err() {
            return;
        }
        let now = Arc::clone(&*permits.borrow_and_update());
        for peer in before.lost_since(&now, Permission::Terminal) {
            let dropped = terminals.revoke(&peer);
            if dropped > 0 {
                println!(
                    "  · dropped {dropped} parked shell(s): that device may no longer have one"
                );
            }
        }
        before = now;
    }
}

/// Close a session when its device stops being allowed to have it.
///
/// Returns as soon as it has closed one, so the caller can tell a revocation
/// from an ordinary ending by whether this finished.
async fn close_when_revoked(
    media: esper_transport::MediaChannel,
    mut live: crate::permits::Live,
    needs: Permission,
) {
    use crate::permits::Standing;
    loop {
        live.changed().await;
        let why = match live.standing() {
            Standing::Unchanged => continue,
            Standing::Forgotten => "that device is no longer paired".to_owned(),
            Standing::Narrowed(lost) => {
                // Only a withdrawal of what this session *is* ends it. Losing
                // a permission it is not using — a clipboard it never asked
                // to read — is applied to the next session, because ending a
                // working session over something it was not doing is a worse
                // answer than either.
                if !lost.contains(&needs) {
                    continue;
                }
                format!(
                    "{} was taken away",
                    lost.iter().map(|p| p.name()).collect::<Vec<_>>().join(", ")
                )
            }
        };
        println!("      · ending this session: {why}");
        media.close(esper_proto::Reason::REVOKED);
        return;
    }
}

/// An open session in the audit trail, closed however the session ends.
///
/// A guard rather than two calls, because a session with an open record and no
/// close is exactly the gap an audit log exists to not have — and `serve_one`
/// has eight ways out.
struct Recorded<'a> {
    log: &'a Log,
    peer: [u8; 32],
    started: Instant,
}

impl<'a> Recorded<'a> {
    /// Record the open. Best-effort: a full disk must not end a session.
    fn open(log: &'a Log, peer: [u8; 32], granted: &[Permission]) -> Self {
        if let Err(e) = log.append(&Entry::now(
            &peer,
            Event::SessionOpened {
                granted: granted.to_vec(),
                // The connectivity ladder arrives in Phase 8. Until there is
                // more than one way to reach a host, saying "direct" is
                // accurate rather than a placeholder.
                tier: "direct".to_owned(),
            },
        )) {
            tracing::warn!(error = %e, "could not record the start of a session");
        }
        Self {
            log,
            peer,
            started: Instant::now(),
        }
    }
}

impl Drop for Recorded<'_> {
    fn drop(&mut self) {
        if let Err(e) = self.log.append(&Entry::now(
            &self.peer,
            Event::SessionClosed {
                duration_secs: self.started.elapsed().as_secs(),
                reason: "the session ended".to_owned(),
            },
        )) {
            tracing::warn!(error = %e, "could not record the end of a session");
        }
    }
}

type Served = Result<(String, Option<Parked>), (String, Option<Parked>)>;

#[allow(clippy::result_large_err)]
async fn serve_one(
    mut session: Session,
    o: &HostOptions,
    parked: Option<Parked>,
    stopping: tokio::sync::watch::Receiver<bool>,
    machine: Machine<'_>,
) -> Served {
    let Machine {
        indicator,
        presses,
        terminals,
        log,
        permits,
    } = machine;
    println!(
        "      peer {}  {}",
        session.peer_fingerprint().hex(),
        session.peer_hello().name
    );

    let device = *session.peer_key();
    let (media, mut control) = match parts(&mut session) {
        Ok(parts) => parts,
        Err(e) => return Err((e.to_string(), parked)),
    };

    // **What this device may do, agreed before anything starts.** The
    // controller says what it wants and the host grants a subset of what the
    // device holds; the answer goes out before a frame or a shell exists, so
    // the far side never has to infer its permissions from what turns up.
    let granted = match agree(&mut control, &device).await {
        Ok(g) => g,
        Err(e) => return Err((e, parked)),
    };
    println!(
        "      granted {}",
        granted
            .iter()
            .map(|p| p.name())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let _recorded = Recorded::open(log, device, &granted);

    // **A terminal session is not a screen session.** No capture, no encoder,
    // no presenter — which is the whole claim of terminal mode, and it would
    // not be true if the host set a screen up first and answered the shell
    // request on the side.
    // What this device may do, re-read whenever it is asked rather than frozen
    // here. The terminal service uses it to decide whether to *keep* a shell
    // standing, which is a question about now and not about when the session
    // opened.
    let live = crate::permits::Live::new(permits.clone(), device, granted.clone());

    if granted.contains(&Permission::Terminal) && !granted.contains(&Permission::View) {
        // **A revocation has to reach a session that is already running.**
        // The watchdog closes the connection; the terminal service then
        // unwinds through its own teardown, and because it re-reads the grant
        // when it decides whether to park, a revoked device's shell is hung
        // up rather than left waiting for it to come back.
        //
        // A task rather than a `select!` arm: cancelling the service
        // mid-session would drop a live pseudo-terminal at whatever await it
        // happened to be sitting on, and teardown is exactly what must not be
        // skipped.
        let watchdog = tokio::spawn(close_when_revoked(
            media.clone(),
            live.clone(),
            Permission::Terminal,
        ));
        let outcome = crate::terminal_cmd::serve(&mut control, terminals, &device, &live).await;
        let was_revoked = watchdog.is_finished();
        watchdog.abort();

        if was_revoked {
            // Any *other* shell of theirs, parked from an earlier connection,
            // goes too. Revoking one session and leaving another standing for
            // the same device would be half a revocation.
            let dropped = terminals.revoke(&device);
            println!("      revoked: {dropped} parked session(s) of theirs ended too");
        }

        // **The last thing the shell said is stream data, not a close
        // reason.** Closing a QUIC connection discards whatever has not been
        // sent, and dropping the session is what closes it — so the exit
        // status the controller is waiting for can be thrown away by this
        // function returning. Waiting for the peer to close first is the
        // graceful shutdown QUIC asks for, and this is the only place in the
        // engine where the last message is stream data rather than a close
        // reason (a `Reason` travels *in* the close frame, which is why the
        // media path needs nothing like this).
        //
        // Only when something is owed. A parked session has nothing queued,
        // and waiting there would add the whole grace to every detach.
        //
        // Honest about the evidence: a session was once observed reporting
        // "connection lost" for a shell that had exited with code 7, and this
        // is the mechanism that would explain it — but with this wait removed
        // again the loss did not reproduce in ten runs, nor with a slow
        // controller, so what happened that once is not established.
        if outcome.as_ref().is_ok_and(|f| f.owed) {
            let _ = tokio::time::timeout(GOODBYE, session.closed()).await;
        }

        return match outcome {
            Ok(f) => Ok((f.what, parked)),
            Err(e) => Err((e, parked)),
        };
    }
    if granted.contains(&Permission::Terminal) {
        // Granted and not served, so it is said out loud. Two modes on one
        // connection needs a way to say which control stream is which, and
        // the protocol has no message for that yet.
        println!("      terminal granted, but a shell needs a session of its own");
    }
    if !granted.contains(&Permission::View) {
        // Nothing to serve, said with a reason code rather than by dropping
        // the connection: this is a device that holds control but not view, or
        // asked for something this host cannot do.
        session.close(esper_proto::Reason::PERMISSION_DENIED);
        return Err((
            "nothing this device is permitted to do can be served".to_owned(),
            parked,
        ));
    }

    // A returning controller resumes what it left; anybody else gets a new
    // session and the parked one is not disturbed by their arrival.
    let (mut link, parked) = match parked {
        Some(p) if p.may_resume(&device) => {
            let (w, h) = p.encode_size();
            let waited = p.parked_for(Instant::now());
            match HostLink::resume(&media, control, p, &device) {
                Ok(link) => {
                    println!("      resumed the {w}x{h} session, parked {waited:?}");
                    (link, None)
                }
                // The session survives a refused resume: it goes back to
                // waiting rather than being destroyed by the attempt to save
                // it.
                Err((p, e)) => {
                    eprintln!("      · could not resume: {e}");
                    return Err((e.to_string(), Some(p)));
                }
            }
        }
        other => match fresh(&media, control, o) {
            Ok(link) => (link, other),
            Err(e) => return Err((e, other)),
        },
    };
    let config = match link.announce().await {
        Ok(c) => c,
        Err(e) => return Err((e.to_string(), parked)),
    };
    println!(
        "      sending {}x{} at up to {} fps, ssrc {:#010x}",
        config.width, config.height, config.max_fps, config.ssrc
    );

    // Where the captured monitor is on the desktop, so a pointer position can
    // be expressed as a fraction of it. Taken from the config that was just
    // announced, so both ends are working from the same list.
    let selected_geometry = config
        .monitors
        .iter()
        .find(|m| m.id == config.selected)
        .map_or((0, 0, config.width, config.height), |m| {
            (m.x, m.y, m.width, m.height)
        });

    // **Whether this device may drive the machine comes from the grant**, and
    // the grant came from the stored record clamped by the transport — never
    // from anything the peer said about itself.
    let may_control = granted.contains(&Permission::Control);

    let input = if may_control {
        let chosen = crate::injector::open();
        for line in &chosen.attempts {
            println!("      {line}");
        }
        if chosen.real {
            Some(esper_input::Dispatcher::new(
                chosen.sink,
                monitors_from(&config),
                true,
            ))
        } else {
            // Said out loud rather than accepted quietly. A host that took
            // keystrokes and dropped them would look like a working session
            // where nothing happens, which is the hardest fault to diagnose
            // from the other end.
            println!("      view only: nothing on this machine can inject input");
            None
        }
    } else {
        // "not granted" rather than "not held": a controller that did not ask
        // for control and one that asked and was refused both land here, and
        // only the grant is a fact this side knows.
        println!("      view only: control is not granted for this session");
        None
    };

    let waiting_for_input = match input {
        Some(dispatcher) => {
            // Accepted in a task because the controller opens this stream
            // lazily — a session that sends no input never opens it, and
            // waiting here would hold up the video.
            let accepting = session.control_acceptor();
            let stopping = stopping.clone();
            let live = live.clone();
            Some(tokio::spawn(async move {
                match accepting.accept().await {
                    Ok(control) => serve_input(control, dispatcher, stopping, live).await,
                    Err(e) => tracing::debug!("no input stream on this session: {e}"),
                }
            }))
        }
        None => None,
    };

    // **The indicator goes up before a single frame goes out.** A person
    // whose screen starts being watched has to be told at the moment it
    // starts, not once the far end has a picture.
    if let Some(indicator) = indicator {
        indicator.show(esper_presence::Watching {
            peer: session.peer_hello().name.clone(),
            controlling: may_control,
        });
    }

    // **The pointer, read on its own schedule.** A cursor sampled at the frame
    // rate is a cursor sampled at five hertz on a quiet desktop, which is why
    // this is not part of the sending loop.
    //
    // A host with no cursor source — a synthetic screen has no pointer — says
    // so once and sends nothing, rather than inventing a pointer in the middle
    // of the picture.
    let (watch, pointer) = match crate::cursor_task::start((
        config.selected,
        selected_geometry.0,
        selected_geometry.1,
        selected_geometry.2,
        selected_geometry.3,
    )) {
        Ok((watch, messages)) => {
            println!("      pointer    {} — sent out of band", watch.backend());
            (Some(watch), Some(messages))
        }
        Err(e) => {
            println!("      pointer    not sent ({e})");
            (None, None)
        }
    };

    let served = serve(
        link,
        media,
        device,
        parked,
        stopping,
        Attending {
            indicator,
            presses,
            pointer,
            watch: watch.as_ref(),
            live: live.clone(),
        },
    )
    .await;
    drop(watch);
    if let Some(indicator) = indicator {
        indicator.hide();
    }
    if let Some(task) = waiting_for_input {
        // Aborted rather than awaited: it is waiting on a stream that will
        // never arrive now, and the dispatcher's `Drop` is what releases
        // anything held.
        task.abort();
    }
    served
}

/// Open a capture backend and build a sender around it.
fn fresh(
    media: &esper_transport::MediaChannel,
    control: esper_transport::Control,
    o: &HostOptions,
) -> Result<HostLink, String> {
    let chosen = crate::backend::open(
        o.backend,
        o.monitor.map(esper_capture::capturer::MonitorId),
        (1280, 720),
    )
    .map_err(|e| format!("opening a capture backend: {e}"))?;
    for line in &chosen.attempts {
        println!("      {line}");
    }
    let pipeline = Pipeline::new(
        chosen.capturer,
        &encoder_for,
        Budget::of(o.budget),
        Policy {
            max_fps: o.fps,
            ..Policy::default()
        },
    )
    .map_err(|e| format!("building the pipeline: {e}"))?
    .with_ceiling(o.max_resolution.map_or_else(Ceiling::none, Ceiling::of));

    HostLink::new(media, control, pipeline).map_err(|e| e.to_string())
}

/// Serve the controller's input on a stream of its own.
///
/// # Why a task rather than a branch in the session loop
///
/// The input stream is opened by the *controller*, lazily — a session that
/// never sends input never opens it — so accepting it means waiting for
/// something that may never come. A task can wait for ever without holding up
/// anything else.
///
/// # Everything is let go of when this ends
///
/// However it ends. The dispatcher releases on drop, and this says so in the
/// log when something was still held, because a session that ended with keys
/// down is worth knowing about even when the release worked.
async fn serve_input(
    session_input: esper_transport::Control,
    mut dispatcher: esper_input::Dispatcher,
    mut stopping: tokio::sync::watch::Receiver<bool>,
    mut live: crate::permits::Live,
) {
    let mut control = session_input;
    let mut refusals = 0u64;
    let mut let_go = 0u64;
    loop {
        // **While something is held, the controller has to keep saying so.**
        // A controller that crashes mid-keystroke sends no release, and the
        // transport takes its thirty-second idle timeout to notice — thirty
        // seconds of a modifier held down on somebody else's machine,
        // measured. So a held session that goes quiet for a few seconds is
        // treated as gone, and everything is let go.
        //
        // Silence with nothing held is not a signal at all: a person watching
        // a screen types nothing, and that is a session working perfectly.
        // **Control withdrawn stops injection here, at the source.** The
        // session ends too, a moment later and for the same reason — but the
        // thing that must not outlive the permission is the injecting, so it
        // is stopped by the code doing it rather than by the teardown of
        // something else.
        let message = if dispatcher.held().is_empty() {
            tokio::select! {
                _ = stopping.changed() => break,
                () = live.changed() => {
                    if !esper_transport::Grants::allows(&live, esper_proto::Permission::Control) {
                        break;
                    }
                    continue;
                }
                m = control.recv::<esper_proto::InputMessage>() => m,
            }
        } else {
            tokio::select! {
                _ = stopping.changed() => break,
                () = live.changed() => {
                    if !esper_transport::Grants::allows(&live, esper_proto::Permission::Control) {
                        break;
                    }
                    continue;
                }
                m = control.recv::<esper_proto::InputMessage>() => m,
                _ = tokio::time::sleep(HELD_SILENCE) => {
                    if dispatcher.release_all() {
                        let_go = let_go.saturating_add(1);
                        println!(
                            "      · nothing from the controller for {}s with input held: \
                             released it",
                            HELD_SILENCE.as_secs()
                        );
                    }
                    continue;
                }
            }
        };
        let Ok(message) = message else { break };
        if let Err(e) = dispatcher.apply(&message) {
            // **Counted, and said once.** A key this platform does not have
            // is ordinary, and a line per keystroke would bury the session's
            // own output. The first is worth seeing.
            refusals = refusals.saturating_add(1);
            if refusals == 1 {
                eprintln!("      · input refused: {e}");
            }
        }
    }
    let (injected, refused) = dispatcher.counts();
    if dispatcher.release_all() {
        println!("      released the input still held when the session ended");
    }
    if injected > 0 || refused > 0 {
        println!("      input: {injected} injected, {refused} refused");
    }
    if let_go > 0 {
        println!("      input was let go of {let_go} time(s) after the controller went quiet");
    }
}

/// How long a held key survives silence from the controller.
///
/// Three heartbeats' worth. Long enough that a lost packet or a stalled
/// controller does not drop a modifier somebody is deliberately holding; short
/// enough that a crashed controller does not leave one held for the
/// transport's whole idle timeout.
const HELD_SILENCE: Duration = Duration::from_secs(3);

/// The monitors as the input side needs them, from the config the host
/// announced.
///
/// Taken from the announcement rather than asked for again, so the controller
/// and the host are working from the same list: a pointer is a fraction of the
/// monitor the *controller was told about*, and a host that resolved it
/// against a newer list would put the pointer somewhere the person did not
/// click.
fn monitors_from(config: &esper_proto::StreamConfig) -> Vec<esper_input::Monitor> {
    config
        .monitors
        .iter()
        .map(|m| esper_input::Monitor {
            id: m.id,
            x: m.x,
            y: m.y,
            width: m.width,
            height: m.height,
        })
        .collect()
}

/// Run a session until it ends, and decide what survives it.
#[allow(clippy::result_large_err)]
/// The things a session leans on that are not the session.
///
/// Bundled because passing them one by one had grown past the point where a
/// reader could tell which was which — and because they belong together: each
/// is something the *host* provides for the duration of a session, owned by
/// the caller and outliving nothing.
/// What a host owns for as long as it runs, handed to each session in turn.
///
/// The outer half of [`Attending`]: these outlive any one session — the panic
/// key is grabbed before the first connection, and a parked shell is resumed
/// by a *later* one — so they are borrowed per session rather than created in
/// it.
struct Machine<'a> {
    /// The session indicator and the panic key.
    indicator: Option<&'a crate::presence_task::Indicator>,
    /// Presses of the panic key, taken one at a time.
    presses: Option<&'a mut crate::presence_task::Presses>,
    /// Terminal sessions, including the ones nobody is attached to.
    terminals: &'a TerminalService,
    /// What happened, for somebody reading it afterwards.
    log: &'a Log,
    /// What every paired device may do, as it changes.
    permits: crate::permits::Watching,
}

struct Attending<'a> {
    /// The session indicator and the panic key.
    indicator: Option<&'a crate::presence_task::Indicator>,
    /// Presses of the panic key, taken one at a time.
    presses: Option<&'a mut crate::presence_task::Presses>,
    /// What the host's pointer is doing.
    pointer: Option<crate::cursor_task::Messages>,
    /// The handle that says which monitor the pointer is watched on.
    watch: Option<&'a crate::cursor_task::CursorWatch>,
    /// What this session's device may do, as it changes.
    live: crate::permits::Live,
}

#[allow(clippy::result_large_err)]
async fn serve(
    link: HostLink,
    media: esper_transport::MediaChannel,
    device: [u8; 32],
    parked: Option<Parked>,
    mut stopping: tokio::sync::watch::Receiver<bool>,
    mut attending: Attending<'_>,
) -> Served {
    // The controller's messages have to reach the pipeline, which lives on the
    // sending thread; and a monitor switch has to be announced back, which
    // only the control stream can do. One channel each way.
    let (to_pipeline, mut from_control) = tokio::sync::mpsc::unbounded_channel::<MediaMessage>();
    let (to_controller, mut from_pipeline) = tokio::sync::mpsc::unbounded_channel::<MediaMessage>();
    let (mut pump, mut control) = link.split();

    let sending = std::thread::spawn(move || {
        let interval = Duration::from_millis(200);
        let mut frames = 0u64;
        // What the controller was last told the picture's size is. Read from
        // the pump rather than passed in, so there is one source for it.
        let opening = pump.config();
        let mut announced = (opening.width, opening.height);
        loop {
            // Anything the controller asked for, before the next frame, so a
            // keyframe request is acted on by the very next one.
            while let Ok(message) = from_control.try_recv() {
                let answer = match pump.handle(&message) {
                    Ok(Handled::Nothing) => None,
                    // The stream changed shape, and the controller has to be
                    // told or it will go on reassembling a stream that no
                    // longer exists.
                    Ok(Handled::Announce(config)) => {
                        println!(
                            "      now sending monitor {}: {}x{}, ssrc {:#010x}",
                            config.selected, config.width, config.height, config.ssrc
                        );
                        Some(config)
                    }
                    // Told, and still answered: the config the host is
                    // keeping is how the controller learns it was heard.
                    Ok(Handled::Refused { config, why }) => {
                        eprintln!("      · {why}");
                        Some(config)
                    }
                    Err(e) => {
                        eprintln!("      · refused a message from the controller: {e}");
                        None
                    }
                };
                if let Some(config) = answer
                    && to_controller.send(MediaMessage::Config(config)).is_err()
                {
                    break;
                }
            }
            match pump.pump(interval) {
                Ok(HostStep::Sent { .. }) => frames = frames.saturating_add(1),
                Ok(HostStep::Finished) => break,
                Ok(_) => {}
                Err(e) => {
                    eprintln!("      · the pipeline stopped: {e}");
                    break;
                }
            }

            // **The governor changes the encode size, and the controller has
            // to be told.**
            //
            // A monitor switch is announced because a controller asked for it.
            // This one nobody asked for: the CPU budget's resolution rung
            // lowers the picture on its own, and a controller still working
            // from the opening announcement sizes its window and its pointer
            // arithmetic from a size that is no longer being sent.
            //
            // Found live — the viewer said "this presenter shows 1280x720, not
            // 960x540" and stopped showing anything, on a session where
            // nothing had gone wrong except that the host got busy.
            let now_sending = pump.config();
            if (now_sending.width, now_sending.height) != announced {
                announced = (now_sending.width, now_sending.height);
                println!(
                    "      now sending {}x{} (the CPU budget moved)",
                    now_sending.width, now_sending.height
                );
                if to_controller
                    .send(MediaMessage::Config(now_sending))
                    .is_err()
                {
                    break;
                }
            }
            if !pump.is_open() {
                break;
            }
        }
        // The pump comes back so its pipeline can outlive the transport.
        (frames, pump.sent(), pump)
    });

    // The async side owns the control stream in both directions: what the
    // controller says goes to the pipeline, and what the pipeline answers goes
    // back out. `Control::recv` is cancel-safe, which is what makes waiting on
    // both at once sound; `send` is awaited outside the select, where nothing
    // can cancel it half-written.
    enum Turn {
        FromController(Result<MediaMessage, esper_transport::ControlError>),
        ToController(Option<MediaMessage>),
        /// The operator asked this process to stop.
        Stop,
        /// The person at the host hit the panic key.
        Panic,
        /// What this device may do changed while the session was running.
        Permissions,
    }
    // Which reason this side ended it with, if it did. A session the host
    // closed is not one to park — parking is for a controller that has not
    // chosen to leave — and the reason is worth naming in the log rather than
    // printing one fixed sentence for two different endings.
    let mut ours: Option<esper_proto::Reason> = None;
    loop {
        let turn = tokio::select! {
            _ = stopping.changed() => Turn::Stop,
            () = wait_for_panic(attending.presses.as_deref_mut()) => Turn::Panic,
            () = attending.live.changed() => Turn::Permissions,
            incoming = control.recv::<MediaMessage>() => Turn::FromController(incoming),
            outgoing = from_pipeline.recv() => Turn::ToController(outgoing),
            cursor = wait_for_cursor(attending.pointer.as_mut()) => Turn::ToController(cursor),
        };
        match turn {
            // **The reason travels.** A controller whose host simply stopped
            // sending cannot tell a shutdown from a network that broke, and
            // there is a code for this one.
            Turn::Stop => {
                media.close(esper_proto::Reason::HOST_SHUTTING_DOWN);
                ours = Some(esper_proto::Reason::HOST_SHUTTING_DOWN);
                break;
            }
            // **Its own reason.** A controller told "the host is shutting
            // down" would wait for it to come back, and told "you were
            // disconnected" would reconnect. Neither happened: a person
            // standing at the machine decided they had seen enough.
            Turn::Panic => {
                media.close(esper_proto::Reason::STOPPED_AT_HOST);
                ours = Some(esper_proto::Reason::STOPPED_AT_HOST);
                // **The whole action, here.** Whoever takes the press does
                // all of it: the first version closed the session here and
                // left the announcement and the screen lock to the accept
                // loop, which then found the queue empty — so the session
                // ended correctly and nothing was said and nothing was
                // locked.
                panic_now(attending.indicator);
                break;
            }
            // **A revocation reaches the session that device already has.**
            // Ending it rather than narrowing it: telling a controller
            // mid-session that it may now do less needs a message on a stream
            // that can carry one, and the first control stream carries media.
            // So the session ends with a reason code that says why, and the
            // controller's next attempt gets the narrower grant.
            Turn::Permissions => match attending.live.standing() {
                crate::permits::Standing::Unchanged => continue,
                crate::permits::Standing::Forgotten => {
                    println!("      · ending this session: that device is no longer paired");
                    media.close(esper_proto::Reason::REVOKED);
                    ours = Some(esper_proto::Reason::REVOKED);
                    break;
                }
                crate::permits::Standing::Narrowed(lost) => {
                    println!(
                        "      · ending this session: {} was taken away",
                        lost.iter().map(|p| p.name()).collect::<Vec<_>>().join(", ")
                    );
                    media.close(esper_proto::Reason::REVOKED);
                    ours = Some(esper_proto::Reason::REVOKED);
                    break;
                }
            },
            Turn::FromController(Ok(message)) => {
                if to_pipeline.send(message).is_err() {
                    break;
                }
            }
            // The peer went away, which is the ordinary end of a session.
            Turn::FromController(Err(_)) => break,
            Turn::ToController(Some(message)) => {
                // **A new config means a new monitor to watch.** Without this
                // a switch would leave every pointer position expressed as a
                // fraction of the screen that is no longer on show — drawn at
                // the right fraction of the wrong monitor.
                if let MediaMessage::Config(config) = &message
                    && let Some(watch) = attending.watch
                    && let Some(m) = config.monitors.iter().find(|m| m.id == config.selected)
                {
                    watch.watch(m.id, m.x, m.y, m.width, m.height);
                }
                if control.send(&message).await.is_err() {
                    break;
                }
            }
            // The sending thread is done, so there is nothing left to say.
            Turn::ToController(None) => break,
        }
    }
    drop(to_pipeline);

    let Ok((frames, sent, pump)) = sending.join() else {
        return Err(("the sending thread panicked".to_owned(), parked));
    };

    // Whether this session is worth keeping turns entirely on *who ended it*.
    // A reason means somebody decided, and a decision is not something to
    // reconnect around. Silence means the path broke under a controller that
    // has not chosen to leave, and that is what parking is for.
    let keep = if let Some(reason) = ours {
        println!(
            "      told the controller: {}",
            reason.describe().unwrap_or("it was over")
        );
        None
    } else {
        match media.peer_close_reason() {
            Some((code, text)) => {
                let described = code.describe().unwrap_or(&text);
                if code.is_failure() {
                    println!("      the controller left: {described} ({})", code.0);
                } else {
                    println!("      the controller left: {described}");
                }
                None
            }
            None => {
                // Saying "the connection ended" beats reporting a reason nobody
                // gave — and the session waits, because the controller never said
                // it was done.
                let config = pump.config();
                println!(
                    "      the connection ended without a reason from either side; \
                 parking the {}x{} session for {:?}",
                    config.width,
                    config.height,
                    esper_stream::PARK_TIMEOUT
                );
                media.close(esper_proto::Reason::PEER_GONE);
                Some(pump.park(device, Instant::now()))
            }
        }
    };

    // Only one session parks at a time. A session that has just become
    // parkable replaces whatever was waiting, and the one it replaces is told
    // why rather than dropped in silence.
    let survivor = match (keep, parked) {
        (Some(new), Some(old)) => {
            println!(
                "      the previously parked session ends: {}",
                Expired::Superseded.describe()
            );
            drop(old.release());
            Some(new)
        }
        (Some(new), None) => Some(new),
        (None, waiting) => waiting,
    };
    let stats = format!(
        "{frames} frames, {} datagrams, {} KiB",
        sent.datagrams,
        sent.bytes / 1024
    );
    Ok((stats, survivor))
}

/// Connect to a paired host and show what it sends.
pub fn run_connect(o: &ConnectOptions) -> Result<(), String> {
    net::install_crypto();
    let identity = paths::identity()?;
    let (store, keys) = paired()?;

    // Dial only a paired host. With several paired, any of them may answer at
    // this address, so all of them are permitted and the *fingerprint* is what
    // says which one it turned out to be.
    let dial: Arc<dyn esper_transport::PeerAuthority> = if let [only] = keys.as_slice() {
        Arc::new(OnlyPeer::new(*only))
    } else {
        Arc::new(AnyOf::new(keys.clone()))
    };
    println!("📺 esper connect {}", o.addr);
    println!();

    net::runtime()?.block_on(async {
        // Bound inside the runtime; quinn needs one.
        let endpoint = Endpoint::bind(&identity, net::any_port(o.addr), Arc::new(NoPeers), dial)
            .map_err(|e| format!("binding a local port: {e}"))?;
        let conn = endpoint
            .connect(o.addr)
            .await
            .map_err(|e| format!("connecting to {}: {e}", o.addr))?;
        let mut session = Session::open(
            conn,
            Hello::new(Role::Controller, identity.public_key(), &net::this_device()),
        )
        .await
        .map_err(|(e, _)| opening_failed(e))?;

        let fingerprint = session.peer_fingerprint();
        let known = store
            .iter()
            .find(|d| d.public_key == *session.peer_key())
            .map(|d| d.name.clone());
        println!(
            "  host       {}",
            known.unwrap_or_else(|| "unknown".to_owned())
        );
        for line in fingerprint.display().lines() {
            println!("      {line}");
        }

        let (media, mut control) = parts(&mut session).map_err(|e| e.to_string())?;

        // **Say what this session is for, and hear what it may actually do.**
        // The grant may be smaller than the request and the controller renders
        // what it holds: asking for control and assuming it was given is how a
        // client offers a cursor that does nothing.
        let grant = esper_transport::request_session(
            &mut control,
            &esper_proto::SessionRequest {
                want: if o.control {
                    vec![Permission::View, Permission::Control]
                } else {
                    vec![Permission::View]
                },
                codecs: vec![config_codec_name().to_owned()],
                reason: "a screen".to_owned(),
            },
        )
        .await
        .map_err(|e| match e {
            esper_transport::AuthorizeError::Refused(d) => {
                format!("the host refused: {} ({})", d.detail, d.reason.0)
            }
            other => format!("agreeing what the session may do: {other}"),
        })?;
        println!(
            "  granted    {}",
            grant
                .granted
                .iter()
                .map(|p| p.name())
                .collect::<Vec<_>>()
                .join(", ")
        );
        // Asked for and not given: said here rather than discovered later by
        // a keystroke that had no effect.
        if o.control && !grant.allows(Permission::Control) {
            println!("  control    refused by the host — this session is view only");
        }
        let may_drive = grant.allows(Permission::Control);

        let link = ControllerLink::open(media, control)
            .await
            .map_err(|e| format!("the host did not announce a stream: {e}"))?;
        let config = link.config().clone();
        println!(
            "  stream     {}x{} {} at up to {} fps",
            config.width,
            config.height,
            config.codec.name(),
            config.max_fps
        );
        for m in &config.monitors {
            println!(
                "      {} {}  {}x{} at {},{}{}",
                if m.id == config.selected { "→" } else { " " },
                m.name,
                m.width,
                m.height,
                m.x,
                m.y,
                if m.primary { "  primary" } else { "" }
            );
        }
        println!();

        // **Sandboxed, with no fallback.** This is a peer's bitstream reaching
        // a large C parser, which is the one thing in this product that must
        // not run in the application's own address space. There is deliberately
        // no flag to turn it off: a fallback is how a security property becomes
        // a preference, and the one time it would trigger is the one time it
        // matters.
        let worker = esper_decode::SandboxedDecoder::spawn_self(
            &esper_decode::decoder::DecoderConfig::default(),
        )
        .map_err(|e| format!("{e}"))?;
        println!("  decoder    sandboxed — {}", worker.report().describe());
        let mut link =
            link.with_decoder(Box::new(worker) as Box<dyn esper_decode::decoder::VideoDecoder>);

        // **The monitor a person asked for, asked for once the session is up.**
        // It cannot go in the connection: which monitors exist is something
        // only the host knows, and it says so in the config that has just
        // arrived. A monitor that is not there comes back as a refusal on the
        // control stream and the session carries on with the one it has.
        let mut asked_for: Option<u32> = None;
        if let Some(wanted) = o.monitor {
            if wanted == config.selected {
                println!("  monitor    {wanted} is the one the host is already sending");
            } else {
                println!("  monitor    asking for {wanted}");
                link.select_monitor(wanted);
                link.flush_outbound()
                    .await
                    .map_err(|e| format!("asking for monitor {wanted}: {e}"))?;
                asked_for = Some(wanted);
            }
        }

        let mut presenter = if o.present {
            match crate::loopback::open_window(config.width, config.height) {
                Ok(p) => {
                    println!("  showing    {}", p.backend());
                    Some(p)
                }
                Err(e) => {
                    println!("  showing    unavailable ({e})");
                    None
                }
            }
        } else {
            None
        };

        // **Driving needs a window to drive from.** Input comes from the
        // viewer's own window, so `--control` without `--present` has nothing
        // to read: said plainly rather than accepted and silently doing
        // nothing.
        let mut driving = None;
        if o.control && may_drive {
            match presenter.as_mut() {
                Some(p) => match session.open_control().await {
                    Ok(control) => {
                        p.forward_input(true);
                        println!("  control    keys and pointer go to the host");
                        driving = Some((
                            crate::input_source::InputSource::new(
                                config.selected,
                                (config.width, config.height),
                            ),
                            control,
                        ));
                    }
                    Err(e) => println!("  control    unavailable ({e})"),
                },
                None => println!("  control    needs a window; add --present"),
            }
        }

        let deadline =
            (o.seconds > 0).then(|| tokio::time::Instant::now() + Duration::from_secs(o.seconds));
        let started = std::time::Instant::now();
        let mut closed_by_peer: Option<(esper_proto::Reason, String)> = None;
        let mut pictures = 0u64;
        let mut skipped = 0u64;
        let mut cursor_shapes = 0u64;
        let mut cursor_moves = 0u64;
        let mut out = Vec::new();
        // **The deadline comes from the stream, not from here.**
        //
        // The first version of this hardcoded a hundred milliseconds, which is
        // a quarter of a second of stutter at 60 fps and one copy per caller.
        // `ControllerLink::due_in` derives it from the stream's own framerate
        // and says how long is left, so this loop only has to honour it.
        const IDLE_GIVE_UP: Duration = Duration::from_secs(30);
        /// The longest a keystroke may sit in the loop before it is sent.
        const INPUT_POLL: Duration = Duration::from_millis(10);
        let mut last_arrival = tokio::time::Instant::now();
        loop {
            if deadline.is_some_and(|d| tokio::time::Instant::now() >= d) {
                break;
            }
            // As long as the stream will let us, and no longer.
            let mut wait = link
                .due_in(std::time::Instant::now())
                .unwrap_or(Duration::from_secs(1));
            // **Except while driving.** The window's events are read once
            // round this loop, so the wait is also the longest a keystroke
            // can sit in it — and with nothing arriving from the host, that
            // wait is a second. Measured: a key pressed and released 30 ms
            // apart reached the host a second apart, so the host held it down
            // and the far end's auto-repeat turned one keystroke into twenty.
            //
            // Ten milliseconds is below anything a person can perceive in a
            // keystroke echo, and the extra wake-ups happen only while
            // somebody is actually driving. Waiting on the window's own file
            // descriptor alongside the datagrams would cost nothing at all
            // and is the better version of this; it needs the presenter to
            // hand out a pollable handle, which is worth doing when the
            // viewer is rebuilt rather than bolting a raw descriptor onto the
            // trait now.
            if driving.is_some() {
                wait = wait.min(INPUT_POLL);
            }
            match tokio::time::timeout(wait, link.pump(&mut out)).await {
                Ok(Ok(())) => {
                    last_arrival = tokio::time::Instant::now();
                    // The host said something about the shape of the stream.
                    // Either it switched, or it is telling us what it is
                    // keeping — and both are said out loud, because a viewer
                    // that silently changes size, or silently does not, looks
                    // like a fault either way.
                    if let Some(announced) = link.take_announcement() {
                        match asked_for.take_if(|m| *m != announced.selected) {
                            Some(refused) => println!(
                                "  · the host would not switch to monitor {refused}; \
                                 it is still sending monitor {}",
                                announced.selected
                            ),
                            None => {
                                asked_for = None;
                                println!(
                                    "  · now showing monitor {}: {}x{}",
                                    announced.selected, announced.width, announced.height
                                );
                                // The same window position is now a fraction
                                // of a different screen.
                                if let Some((source, _)) = driving.as_mut() {
                                    source.showing(
                                        announced.selected,
                                        (announced.width, announced.height),
                                    );
                                }
                                if let Some(p) = presenter.as_mut()
                                    && let Err(e) = p.resize(announced.width, announced.height)
                                {
                                    println!("  · the window could not follow: {e}");
                                    presenter = None;
                                }
                            }
                        }
                    }
                }
                Ok(Err(e)) => {
                    // The error is often the peer's close arriving, and the
                    // close carries a reason. Reporting the error text
                    // instead — and then announcing our own reason on top of
                    // it — tells the person the session ended for a reason
                    // nobody gave, when the host had said exactly why.
                    match link.peer_close_reason() {
                        Some((code, text)) => closed_by_peer = Some((code, text)),
                        None => println!("  · the session ended: {e}"),
                    }
                    break;
                }
                Err(_) => {
                    // Nothing arrived in time, so whatever is part-assembled
                    // is as complete as it is going to get. `tick` does
                    // nothing if the deadline has not really passed.
                    link.tick(std::time::Instant::now(), &mut out);
                    // A host that closed with a reason has said why, and
                    // saying it back is the difference between "the session
                    // ended" and "something went wrong, we think".
                    if let Some((code, text)) = link.peer_close_reason() {
                        closed_by_peer = Some((code, text));
                        break;
                    }
                    if last_arrival.elapsed() > IDLE_GIVE_UP {
                        println!("  · nothing arrived for {} seconds", IDLE_GIVE_UP.as_secs());
                        break;
                    }
                }
            }
            for d in out.drain(..) {
                match d {
                    Decoded::Picture { picture, .. } => {
                        pictures = pictures.saturating_add(1);
                        if let Some(p) = presenter.as_mut() {
                            // The whole picture: the receiving side is not
                            // told which regions changed, because the damage
                            // travelled with the *capture* and a decoded
                            // frame is whatever the decoder reconstructed.
                            if let Err(e) =
                                p.present(&picture, &esper_capture::damage::Damage::Everything)
                            {
                                println!("  · the window stopped taking pictures: {e}");
                                presenter = None;
                            }
                        }
                    }
                    Decoded::Lost { why, .. } => {
                        tracing::debug!("a frame was lost: {why}");
                    }
                    Decoded::Skipped { why, timestamp } => {
                        if skipped == 0 {
                            println!("  · a frame at {timestamp} was not shown: {why}");
                        }
                        skipped = skipped.saturating_add(1);
                    }
                }
            }
            // **What the host said about its pointer.**
            //
            // The shape is handed to the window, which then draws it at the
            // *local* pointer — so it moves at the speed of the person's hand
            // rather than at the speed of the link. That is the whole reason
            // the cursor travels separately from the video.
            //
            // The position is not used for drawing in a session being driven:
            // the local pointer is already where the person put it, and a
            // second opinion arriving a network away later would only make it
            // stutter. It is counted so a session can say the pointer was
            // being tracked.
            for message in link.take_cursor() {
                match message {
                    esper_proto::CursorMessage::Shape(shape) => {
                        cursor_shapes = cursor_shapes.saturating_add(1);
                        let image = match &shape.image {
                            esper_proto::CursorImage::Hidden => None,
                            esper_proto::CursorImage::Bitmap {
                                width,
                                height,
                                hotspot_x,
                                hotspot_y,
                                rgba,
                            } => Some(esper_capture::cursor::CursorImage {
                                width: *width,
                                height: *height,
                                hotspot_x: *hotspot_x,
                                hotspot_y: *hotspot_y,
                                rgba: rgba.clone(),
                            }),
                        };
                        if let Some(p) = presenter.as_mut()
                            && let Err(e) = p.set_cursor(image.as_ref())
                        {
                            // Not fatal: a viewer that cannot wear the remote
                            // pointer still shows the screen, and saying so
                            // once beats a line per shape change.
                            if cursor_shapes == 1 {
                                println!("  · the window cannot wear the remote pointer ({e})");
                            }
                        }
                    }
                    esper_proto::CursorMessage::Moved(at) => {
                        cursor_moves = cursor_moves.saturating_add(1);
                        // **Drawn or worn, and this is where that is decided.**
                        //
                        // Worn is better whenever it is honest: the server
                        // draws the pointer at the person's own hand, with no
                        // latency at all. It is honest exactly when the local
                        // pointer *is* the host's pointer — a session that is
                        // driving, and whose host is following.
                        //
                        // Otherwise the host's own position has to be drawn:
                        // a session only watching has no local pointer to
                        // speak of, and a host holding its pointer has one
                        // that is no longer under the person's hand.
                        let draw = match driving.as_ref() {
                            None => true,
                            Some((source, _)) => {
                                source.host_is_not_following(at, std::time::Instant::now())
                            }
                        };
                        if let Some(p) = presenter.as_mut() {
                            let showing = link.config();
                            let where_to =
                                draw.then(|| at.to_pixels(showing.width, showing.height));
                            let same_monitor = at.monitor == showing.selected;
                            // A pointer on another monitor is not in this
                            // picture: nothing to draw, and nothing to wear
                            // either — a worn pointer would be showing the
                            // person a cursor for a screen they cannot see.
                            let target = if same_monitor { where_to } else { None };
                            if let Err(e) = p.draw_cursor_at(target)
                                && cursor_moves == 1
                            {
                                println!("  · the pointer cannot be drawn ({e})");
                            }
                        }
                    }
                }
            }

            // What the person did in the window: closed it, or drove the
            // host with it.
            let happened = presenter
                .as_mut()
                .and_then(|p| p.poll().ok())
                .unwrap_or_default();
            if happened.contains(&esper_present::PresentEvent::CloseRequested) {
                println!("  · the window was closed");
                break;
            }
            if let Some((source, control)) = driving.as_mut() {
                let mut failed = None;
                // **Pointer moves collapse to the last one in the batch.** A
                // mouse dragged across a window produces a motion event per
                // pixel of the path, and every one but the last describes a
                // place the pointer has already left. Sending them all would
                // spend the link on a trail nobody needs — and a host
                // injecting them all would move its pointer along the same
                // path a moment late.
                let mut latest_move = None;
                for event in &happened {
                    let Some(message) = source.translate(event) else {
                        continue;
                    };
                    if matches!(message, esper_proto::InputMessage::Pointer(_)) {
                        latest_move = Some(message);
                        continue;
                    }
                    // A press has to arrive *after* the move that positioned
                    // it, or the host clicks where the pointer used to be.
                    if let Some(m) = latest_move.take()
                        && let Err(e) = control.send(&m).await
                    {
                        failed = Some(e);
                        break;
                    }
                    if let Err(e) = control.send(&message).await {
                        failed = Some(e);
                        break;
                    }
                }
                if failed.is_none()
                    && let Some(m) = latest_move
                    && let Err(e) = control.send(&m).await
                {
                    failed = Some(e);
                }
                // **Said only while something is held.** It is what lets the
                // host let go if this process disappears without a word.
                if failed.is_none()
                    && let Some(beat) = source.heartbeat(std::time::Instant::now())
                    && let Err(e) = control.send(&beat).await
                {
                    failed = Some(e);
                }
                if let Some(e) = failed {
                    // The input stream is gone. The session carries on
                    // showing the screen: a viewer that could no longer type
                    // is still a viewer, and ending it would take the picture
                    // away too.
                    println!("  · input stopped reaching the host ({e}); watching only");
                    driving = None;
                }
            }
        }
        // **Say what is being let go of, rather than leaving the host to work
        // it out.** A session ending with a key held is ordinary — the window
        // closes while somebody is mid-shortcut — and the host's silence timer
        // would get there in a few seconds. Saying it takes one message and
        // gets there at once.
        if let Some((source, control)) = driving.as_mut()
            && source.is_holding()
        {
            match control.send(&esper_proto::InputMessage::ReleaseAll).await {
                Ok(()) => println!("  · released the keys still held"),
                Err(e) => println!("  · could not release the keys still held ({e})"),
            }
        }

        // And once more at the end, so a session that stops mid-frame still
        // shows what it had.
        link.flush(&mut out);
        for d in out.drain(..) {
            if let Decoded::Picture { .. } = d {
                pictures = pictures.saturating_add(1);
            }
        }
        let stats = link.stats();
        println!();
        // **Why it ended, in the vocabulary the protocol has for it.** Every
        // reason code used to be a failure, so a session ending normally
        // looked like one; the 0xx class exists so "the person closed the
        // window" and "the host refused you" are different sentences.
        match closed_by_peer {
            Some((code, text)) => {
                let described = code.describe().unwrap_or(&text);
                if code.is_failure() {
                    println!("  · the host ended the session: {described} ({})", code.0);
                } else {
                    println!("  · the host ended the session: {described}");
                }
            }
            None => {
                // This side is done, so say so with the reason rather than
                // dropping the connection and leaving the host to guess.
                let reason = if o.seconds > 0 {
                    esper_proto::Reason::SESSION_COMPLETE
                } else {
                    esper_proto::Reason::CLOSED_BY_USER
                };
                println!(
                    "  · ending the session: {}",
                    reason.describe().unwrap_or("closed")
                );
                link.close(reason);
                // **And wait for the close to be delivered.**
                //
                // `close` is best-effort: it queues a CONNECTION_CLOSE and
                // returns. A process that exits immediately afterwards never
                // sends it, so the host hears nothing and waits out its idle
                // timeout — a session that ended cleanly looks like a peer
                // that vanished, and the reason this code went to the trouble
                // of choosing is thrown away.
                //
                // A close is a message, not an instruction: it has to be
                // delivered, and delivery is the caller's to wait for. The
                // pairing teardown waits for the same reason.
                tokio::time::timeout(Duration::from_secs(2), endpoint.wait_idle())
                    .await
                    .ok();
            }
        }
        println!();

        let path = link.transport();
        let jitter = link.jitter();
        let seconds = started.elapsed().as_secs_f64().max(0.001);
        println!("  pictures   {pictures} shown, {skipped} not shown");
        if cursor_shapes > 0 || cursor_moves > 0 {
            println!(
                "  pointer    {cursor_shapes} shape(s), {cursor_moves} position(s) — out of \
                 band, drawn locally"
            );
        }
        // Said only when it happened. Input this window produced and the
        // protocol has no way to carry — a key HID does not name, a pointer
        // outside the picture — is dropped here rather than approximated, and
        // a person whose side button does nothing deserves to know it was
        // this end that decided.
        if let Some((source, _)) = driving.as_ref()
            && source.dropped() > 0
        {
            println!(
                "  input      {} event(s) this window produced could not be sent",
                source.dropped()
            );
        }
        println!(
            "  frames     {} lost, {} keyframes asked for",
            stats.frames_lost, stats.keyframes_requested
        );
        println!(
            "  received   {} datagrams, {} KiB, {:.0} kbit/s",
            stats.datagrams,
            stats.bytes / 1024,
            (stats.bytes as f64 * 8.0 / 1000.0) / seconds
        );
        println!(
            "  path       {:.1} ms round trip, {} packets lost, {} KiB window",
            path.rtt.as_secs_f64() * 1000.0,
            path.lost_packets,
            path.congestion_window / 1024
        );
        // The number that says whether the deadline is long enough: a worst
        // gap past it means frames are being abandoned for packets that were
        // merely late.
        println!(
            "  jitter     {:.1} ms interarrival, worst gap {:.0} ms, {} deadline(s) fired",
            link.jitter_ms(),
            jitter.worst_gap.as_secs_f64() * 1000.0,
            jitter.deadlines
        );
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_nothing_parked_the_host_waits_indefinitely() {
        assert_eq!(accept_deadline(None), None);
    }

    #[test]
    fn a_parked_session_bounds_the_wait_by_what_is_left_of_its_park() {
        assert_eq!(accept_deadline(Some(Duration::ZERO)), Some(PARK_TIMEOUT));
        assert_eq!(
            accept_deadline(Some(Duration::from_secs(10))),
            Some(PARK_TIMEOUT.saturating_sub(Duration::from_secs(10)))
        );
    }

    /// **A park already past its timeout must expire now, not in another
    /// fifteen seconds.** Subtracting without saturating is either a panic or a
    /// wrap to nearly forever, and forever is what this deadline exists to
    /// prevent.
    #[test]
    fn an_overdue_park_does_not_buy_itself_more_time() {
        for over in [
            Duration::ZERO,
            Duration::from_millis(1),
            Duration::from_secs(3600),
        ] {
            let waited = PARK_TIMEOUT.saturating_add(over);
            assert_eq!(
                accept_deadline(Some(waited)),
                Some(Duration::ZERO),
                "a session parked {waited:?} against a {PARK_TIMEOUT:?} timeout"
            );
        }
    }
}
