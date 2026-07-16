//! `ownview-engine host` and `ownview-engine connect` — a video session
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
use std::time::Duration;

use ownview_capture::gate::Policy;
use ownview_crypto::DeviceStore;
use ownview_encode::budget::Budget;
use ownview_encode::encoder::{EncodeError, EncoderConfig, VideoEncoder};
use ownview_encode::openh264::OpenH264;
use ownview_link::{ControllerLink, HostLink, parts};
use ownview_pipeline::pipeline::Pipeline;
use ownview_proto::{Hello, MediaMessage, Role};
use ownview_scale::Ceiling;
use ownview_stream::{Decoded, HostStep};
use ownview_transport::{AnyOf, Endpoint, NoPeers, OnlyPeer, Session};

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
        }
    }
}

/// What `connect` was asked to do.
#[derive(Debug, Clone, Copy)]
pub struct ConnectOptions {
    pub addr: SocketAddr,
    pub seconds: u64,
    pub present: bool,
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
fn paired() -> Result<(DeviceStore, Vec<[u8; 32]>), String> {
    let (store, dropped) = paths::devices()?;
    for note in &dropped {
        eprintln!("  · the device list needed correcting: {note}");
    }
    let keys: Vec<[u8; 32]> = store.iter().map(|d| d.public_key).collect();
    if keys.is_empty() {
        return Err("no devices are paired, so nobody could connect. Run \
             `ownview-engine pair --listen 7645` and pair one first"
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
    let endpoint = runtime.block_on(async {
        Endpoint::bind(
            &identity,
            o.listen,
            Arc::new(AnyOf::new(keys.clone())),
            Arc::new(NoPeers),
        )
        .map_err(|e| format!("binding {}: {e}", o.listen))
    })?;
    let bound = endpoint
        .local_addr()
        .map_err(|e| format!("reading the bound address: {e}"))?;

    println!("📺 ownview host");
    println!();
    println!("  listening  {bound}");
    println!("  paired     {} device(s)", keys.len());
    for device in store.iter() {
        println!("      {}  {}", device.fingerprint().hex(), device.name);
    }
    println!();

    runtime.block_on(async {
        loop {
            let conn = match endpoint.accept().await {
                Some(Ok(c)) => c,
                Some(Err(e)) => {
                    // A refused or broken handshake is one peer, not the end
                    // of the host. Reported and waited past.
                    eprintln!("  · a connection failed: {e}");
                    continue;
                }
                None => return Ok(()),
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
            match serve_one(session, o).await {
                Ok(stats) => println!("  · {from} finished: {stats}"),
                Err(e) => eprintln!("  · {from} ended: {e}"),
            }
            if o.once {
                return Ok(());
            }
        }
    })
}

/// Serve exactly one session.
async fn serve_one(mut session: Session, o: &HostOptions) -> Result<String, String> {
    println!(
        "      peer {}  {}",
        session.peer_fingerprint().hex(),
        session.peer_hello().name
    );

    let chosen = crate::backend::open(
        o.backend,
        o.monitor.map(ownview_capture::capturer::MonitorId),
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

    let (media, control) = parts(&mut session).map_err(|e| e.to_string())?;
    let mut link = HostLink::new(&media, control, pipeline).map_err(|e| e.to_string())?;
    let config = link.announce().await.map_err(|e| e.to_string())?;
    println!(
        "      sending {}x{} at up to {} fps, ssrc {:#010x}",
        config.width, config.height, config.max_fps, config.ssrc
    );

    // The controller's messages have to reach the pipeline, which lives on the
    // sending thread. One channel, one direction.
    let (to_pipeline, mut from_control) = tokio::sync::mpsc::unbounded_channel::<MediaMessage>();
    let (mut pump, mut control) = link.split();

    let sending = std::thread::spawn(move || {
        let interval = Duration::from_millis(200);
        let mut frames = 0u64;
        loop {
            // Anything the controller asked for, before the next frame, so a
            // keyframe request is acted on by the very next one.
            while let Ok(message) = from_control.try_recv() {
                if let Err(e) = pump.handle(&message) {
                    eprintln!("      · refused a message from the controller: {e}");
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
            if !pump.is_open() {
                break;
            }
        }
        (frames, pump.sent())
    });

    // The async side reads the control stream until the peer goes away.
    while let Ok(message) = control.recv::<MediaMessage>().await {
        if to_pipeline.send(message).is_err() {
            break;
        }
    }
    drop(to_pipeline);

    let (frames, sent) = sending
        .join()
        .map_err(|_| "the sending thread panicked".to_owned())?;
    Ok(format!(
        "{frames} frames, {} datagrams, {} KiB",
        sent.datagrams,
        sent.bytes / 1024
    ))
}

/// Connect to a paired host and show what it sends.
pub fn run_connect(o: &ConnectOptions) -> Result<(), String> {
    net::install_crypto();
    let identity = paths::identity()?;
    let (store, keys) = paired()?;

    // Dial only a paired host. With several paired, any of them may answer at
    // this address, so all of them are permitted and the *fingerprint* is what
    // says which one it turned out to be.
    let dial: Arc<dyn ownview_transport::PeerAuthority> = if let [only] = keys.as_slice() {
        Arc::new(OnlyPeer::new(*only))
    } else {
        Arc::new(AnyOf::new(keys.clone()))
    };
    println!("📺 ownview connect {}", o.addr);
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
        .map_err(|(e, _)| format!("opening the session: {e}"))?;

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

        let (media, control) = parts(&mut session).map_err(|e| e.to_string())?;
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
        let worker = ownview_decode::SandboxedDecoder::spawn_self(
            &ownview_decode::decoder::DecoderConfig::default(),
        )
        .map_err(|e| format!("{e}"))?;
        println!("  decoder    sandboxed — {}", worker.report().describe());
        let mut link =
            link.with_decoder(Box::new(worker) as Box<dyn ownview_decode::decoder::VideoDecoder>);

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

        let deadline =
            (o.seconds > 0).then(|| tokio::time::Instant::now() + Duration::from_secs(o.seconds));
        let started = std::time::Instant::now();
        let mut pictures = 0u64;
        let mut skipped = 0u64;
        let mut out = Vec::new();
        // **The deadline comes from the stream, not from here.**
        //
        // The first version of this hardcoded a hundred milliseconds, which is
        // a quarter of a second of stutter at 60 fps and one copy per caller.
        // `ControllerLink::due_in` derives it from the stream's own framerate
        // and says how long is left, so this loop only has to honour it.
        const IDLE_GIVE_UP: Duration = Duration::from_secs(30);
        let mut last_arrival = tokio::time::Instant::now();
        loop {
            if deadline.is_some_and(|d| tokio::time::Instant::now() >= d) {
                break;
            }
            // As long as the stream will let us, and no longer.
            let wait = link
                .due_in(std::time::Instant::now())
                .unwrap_or(Duration::from_secs(1));
            match tokio::time::timeout(wait, link.receive(&mut out)).await {
                Ok(Ok(())) => last_arrival = tokio::time::Instant::now(),
                Ok(Err(e)) => {
                    println!("  · the session ended: {e}");
                    break;
                }
                Err(_) => {
                    // Nothing arrived in time, so whatever is part-assembled
                    // is as complete as it is going to get. `tick` does
                    // nothing if the deadline has not really passed.
                    link.tick(std::time::Instant::now(), &mut out);
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
                                p.present(&picture, &ownview_capture::damage::Damage::Everything)
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
            if presenter.as_mut().is_some_and(|p| {
                p.poll()
                    .is_ok_and(|e| e.contains(&ownview_present::PresentEvent::CloseRequested))
            }) {
                println!("  · the window was closed");
                break;
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
        let path = link.transport();
        let jitter = link.jitter();
        let seconds = started.elapsed().as_secs_f64().max(0.001);
        println!("  pictures   {pictures} shown, {skipped} not shown");
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
