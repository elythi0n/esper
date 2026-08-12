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
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use esper_audit::{Entry, Event, Log};
use esper_capture::gate::Policy;
use esper_crypto::{DeviceStore, LockAfter};
use esper_encode::budget::Budget;
use esper_encode::encoder::{EncodeError, EncoderConfig, VideoEncoder};
use esper_encode::openh264::OpenH264;
use esper_link::{ControllerLink, HostLink, parts};
use esper_pipeline::pipeline::Pipeline;
use esper_proto::{Hello, MediaMessage, Permission, Profile, Role, Sharpness};
use esper_scale::Ceiling;
use esper_stream::{Decoded, Expired, Handled, HostStep, PARK_TIMEOUT, Parked};
use esper_transport::{
    Answered, AnyOf, Control, Endpoint, NoPeers, OnlyPeer, Session, TerminalPolicy,
    TerminalService, answer_session,
};

use crate::backend::Choice;
use crate::{net, paths};

/// What `host` was asked to do.
#[derive(Debug, Clone)]
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
    /// A broker to wait at, so a controller with no route to this machine can
    /// still find it. **No default**: if nobody names one, this tier does not
    /// exist.
    pub via: Option<esper_rendezvous::Via>,
    /// The targets a forward may reach. Loopback when nobody says otherwise.
    pub forward: esper_transport::forward::Allowlist,
    /// How many forwarded connections one session may hold at once.
    pub forwards_at_once: usize,
    /// A supervisor's socket to connect back to, so it can ask this host
    /// whether it is working rather than only whether it exists.
    ///
    /// Passed by `esper host`, which is what supervises this process. Nothing
    /// is lost when it is absent: a host started by hand is watched by the
    /// person who started it.
    pub watched_by: Option<std::path::PathBuf>,
}

impl HostOptions {
    /// What the settings say, before any flag has been read.
    ///
    /// **Not `Default`.** A default is what the program chose; this is what
    /// somebody configured, and the difference is the whole point of having a
    /// configuration file — so the flags in `parse_host` override *this*
    /// rather than a set of literals.
    fn from_settings() -> Result<Self, String> {
        Ok(Self {
            listen: crate::settings::text("host.listen")
                .parse()
                .map_err(|_| "host.listen is not an address".to_owned())?,
            backend: Choice::parse(&crate::settings::text("host.backend"))?,
            monitor: crate::settings::whole("host.monitor").and_then(|n| u32::try_from(n).ok()),
            fps: crate::settings::whole("host.fps")
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(30),
            budget: crate::settings::number("host.budget").unwrap_or(Budget::default().cores),
            max_resolution: crate::settings::whole("host.max_resolution")
                .and_then(|n| u32::try_from(n).ok()),
            once: false,
            watched_by: None,
            mdns: crate::settings::flag("discovery.mdns", true),
            via: crate::settings::broker()?,
            // **Checked when the host starts, not when a forward asks.** A
            // typo in the list is a message somebody sees while they are
            // looking at the file, rather than a refusal on a machine nobody
            // is watching, hours later.
            forward: esper_transport::forward::Allowlist::parse(&crate::settings::text(
                "forward.allow",
            ))
            .map_err(|e| format!("forward.allow: {e}"))?,
            forwards_at_once: usize::try_from(
                crate::settings::whole("forward.max_connections").unwrap_or(64),
            )
            .unwrap_or(64),
        })
    }
}

/// What `connect` was asked to do.
#[derive(Debug, Clone)]
pub struct ConnectOptions {
    /// Where the host is, when it is known. `None` with `via` set: the whole
    /// point of a broker is that the address is what you do not have.
    pub addr: Option<SocketAddr>,
    pub seconds: u64,
    pub present: bool,
    /// Send this window's keys and pointer to the host.
    pub control: bool,
    /// Screens to ask the host for, when they are not the ones announced.
    ///
    /// Several are shown as one picture, in the arrangement the host's desktop
    /// has. The order is the order they were asked for, which only matters to
    /// the refusal message: where they *sit* is the desk's business, not the
    /// controller's.
    pub monitors: Option<Vec<u32>>,
    /// Do not ask for the host's sound.
    ///
    /// For a session on a machine whose speakers somebody else is listening
    /// to, or a link too small to spend on sound. **It stops the sound being
    /// sent**, not merely played: a grant is the intersection of what the
    /// device holds and what the session asked for, so a session that does not
    /// ask is a session the host sends nothing to — which is better than
    /// declining, because nobody pays for it.
    pub no_audio: bool,
    /// Ask for every screen the host has, whatever they turn out to be.
    ///
    /// Which screens exist is something only the host knows, so this cannot be
    /// a list at connect time: it becomes one when the config arrives.
    pub all_monitors: bool,
    /// Where to write a recording of the session, if one was asked for.
    ///
    /// **Opt-in, controller-side, and the host is told.** A recording is a
    /// copy of somebody else's screen that outlives the session, which is a
    /// different thing from watching it — so it is never on by default, and
    /// the machine being recorded says so on its own screen for as long as it
    /// lasts.
    pub record: Option<std::path::PathBuf>,
    /// A broker to be introduced through, when nothing direct works.
    pub via: Option<esper_rendezvous::Via>,
    /// Which paired host, by name or fingerprint prefix.
    ///
    /// The alternative to an address, and the usual way in: where a machine
    /// *is* changes, and which machine it is does not.
    pub named: Option<String>,
    /// What the person is doing, which is the only thing this side knows that
    /// the host does not.
    ///
    /// **The one input a host cannot work out for itself.** A slideshow and a
    /// video look the same to a damage tracker until it is too late to have
    /// chosen differently, and the right thing to give up is opposite in the
    /// two cases: reading a screen, frames are nearly free to lose and glyph
    /// edges are the whole point; watching motion, stutter is the first thing
    /// anybody notices and moving pictures hide quantiser noise so well that
    /// video codecs have leaned on it for thirty years.
    pub profile: Profile,
    /// Ask the segment where it is. On by default, like the host's own
    /// advertising, and off with `--no-mdns` for somebody who would rather
    /// not be heard asking.
    pub mdns: bool,
    /// What to ask the host for the clipboard. The host's grant decides what
    /// is actually allowed; this is only what is asked for.
    pub clipboard: ClipboardWish,
}

/// Parse `host` flags.
pub fn parse_host(args: &[String]) -> Result<HostOptions, String> {
    let mut o = HostOptions::from_settings()?;
    let mut via_addr: Option<SocketAddr> = None;
    let mut via_key: Option<[u8; 32]> = None;
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
            "--forward-allow" => {
                o.forward = esper_transport::forward::Allowlist::parse(&value("--forward-allow")?)
                    .map_err(|e| format!("--forward-allow: {e}"))?;
                i += 2;
            }
            "--via" => {
                via_addr = Some(net::parse_addr(&value("--via")?)?);
                i += 2;
            }
            "--broker-key" => {
                via_key = Some(crate::broker_cmd::parse_key(&value("--broker-key")?)?);
                i += 2;
            }
            "--watched-by" => {
                o.watched_by = Some(std::path::PathBuf::from(value("--watched-by")?));
                i += 2;
            }
            other => {
                return Err(format!("unknown flag {other}\n{}", host_usage()));
            }
        }
    }
    o.via = broker_from(via_addr, via_key)?;
    Ok(o)
}

/// A broker is an address **and** a pinned key, or neither.
///
/// Refused rather than half-configured: an address with no key would mean
/// trusting whatever answered there, which is the one thing pinning exists to
/// prevent — and a key with no address is a peer that cannot say where to go.
pub fn broker_from(
    addr: Option<SocketAddr>,
    key: Option<[u8; 32]>,
) -> Result<Option<esper_rendezvous::Via>, String> {
    match (addr, key) {
        (Some(addr), Some(key)) => Ok(Some(esper_rendezvous::Via { addr, key })),
        (None, None) => Ok(None),
        (Some(_), None) => Err("--via needs --broker-key: the broker's key is pinned, and \
             `esper-broker serve` prints it"
            .to_owned()),
        (None, Some(_)) => Err("--broker-key needs --via, which is where the broker is".to_owned()),
    }
}

/// Parse `connect` flags.
/// What a controller asks for the clipboard, said the way a person thinks
/// about it.
///
/// The permissions are written from the host's point of view — `clipboard out`
/// means the controller may read the host's — which is the right way round for
/// a host's records and the wrong way round for a person at a keyboard. So the
/// option is named after what it does: copy on the host and paste here, or the
/// other way, or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClipboardWish {
    /// Both ways, as far as the host allows.
    #[default]
    Both,
    /// Copy on the host, paste here.
    FromHost,
    /// Copy here, paste on the host.
    ToHost,
    /// Neither. A session with no clipboard at all.
    None,
}

impl ClipboardWish {
    /// The permissions this wish asks the host for.
    fn wanted(self) -> Vec<Permission> {
        match self {
            Self::Both => vec![Permission::ClipboardOut, Permission::ClipboardIn],
            Self::FromHost => vec![Permission::ClipboardOut],
            Self::ToHost => vec![Permission::ClipboardIn],
            Self::None => Vec::new(),
        }
    }

    fn parse(word: &str) -> Option<Self> {
        match word {
            "both" => Some(Self::Both),
            "from-host" => Some(Self::FromHost),
            "to-host" => Some(Self::ToHost),
            "none" => Some(Self::None),
            _ => None,
        }
    }
}

pub fn parse_connect(args: &[String]) -> Result<ConnectOptions, String> {
    let mut addr: Option<SocketAddr> = None;
    let mut seconds = 0u64;
    let mut profile = Profile::default();
    let mut clipboard = ClipboardWish::default();
    let mut present = false;
    let mut control = false;
    let mut monitors: Option<Vec<u32>> = None;
    let mut all_monitors = false;
    let mut no_audio = false;
    let mut record: Option<std::path::PathBuf> = None;
    let mut via_addr: Option<SocketAddr> = None;
    let mut via_key: Option<[u8; 32]> = None;
    let mut named: Option<String> = None;
    let mut mdns = crate::settings::flag("discovery.mdns", true);
    let mut i = 0usize;
    while let Some(arg) = args.get(i) {
        match arg.as_str() {
            "--via" => {
                via_addr =
                    Some(net::parse_addr(args.get(i + 1).ok_or_else(|| {
                        "--via needs the broker's address".to_owned()
                    })?)?);
                i += 2;
            }
            "--broker-key" => {
                via_key = Some(crate::broker_cmd::parse_key(
                    args.get(i + 1)
                        .ok_or_else(|| "--broker-key needs the broker's key".to_owned())?,
                )?);
                i += 2;
            }
            "--no-mdns" => {
                mdns = false;
                i += 1;
            }
            "--clipboard" => {
                let name = args.get(i + 1).ok_or_else(|| {
                    "--clipboard needs both, from-host, to-host or none".to_owned()
                })?;
                clipboard = ClipboardWish::parse(name).ok_or_else(|| {
                    format!(
                        "{name} is not a clipboard setting; it is both, from-host, to-host or none"
                    )
                })?;
                i += 2;
            }
            "--profile" => {
                let name = args
                    .get(i + 1)
                    .ok_or_else(|| "--profile needs text, balanced or motion".to_owned())?;
                profile = match name.as_str() {
                    "text" => Profile::Text,
                    "balanced" => Profile::Balanced,
                    "motion" => Profile::Motion,
                    other => {
                        return Err(format!(
                            "{other} is not a profile; it is text, balanced or motion"
                        ));
                    }
                };
                i += 2;
            }
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
                let list = args
                    .get(i + 1)
                    .ok_or_else(|| "--monitor needs a monitor number".to_owned())?;
                let mut wanted = Vec::new();
                for part in list.split(',') {
                    let n: u32 = part.trim().parse().map_err(|_| {
                        "--monitor wants monitor numbers, like `1` or `0,1`".to_owned()
                    })?;
                    // Refused rather than deduplicated: a person who typed
                    // `0,0` meant something, and it was not "show screen 0
                    // once".
                    if wanted.contains(&n) {
                        return Err(format!("--monitor names monitor {n} twice"));
                    }
                    wanted.push(n);
                }
                if wanted.is_empty() {
                    return Err("--monitor needs a monitor number".to_owned());
                }
                monitors = Some(wanted);
                i += 2;
            }
            "--all-monitors" => {
                all_monitors = true;
                i += 1;
            }
            "--no-audio" => {
                no_audio = true;
                i += 1;
            }
            "--record" => {
                let Some(path) = args.get(i + 1) else {
                    return Err("--record wants a file to write, like session.mkv".to_owned());
                };
                record = Some(std::path::PathBuf::from(path));
                i += 2;
            }
            // **An address or a name.** An address says where without saying
            // which; a name says which without saying where. Both are
            // ordinary things to type, and the key check settles the identity
            // either way.
            other if !other.starts_with('-') && addr.is_none() && named.is_none() => {
                match net::parse_addr(other) {
                    Ok(a) => addr = Some(a),
                    Err(_) => named = Some(other.to_owned()),
                }
                i += 1;
            }
            other => {
                return Err(format!("unknown flag {other}\n{}", connect_usage()));
            }
        }
    }
    // A broker on the command line replaces the configured one; without one,
    // whatever the settings say.
    // Two ways of naming the same thing, and no way to tell which was meant.
    // Refused by name, the way `screenshot` refuses the same pair.
    if all_monitors && monitors.is_some() {
        return Err("--all-monitors and --monitor ask for different things".to_owned());
    }
    let via = match broker_from(via_addr, via_key)? {
        Some(via) => Some(via),
        None => crate::settings::broker()?,
    };
    Ok(ConnectOptions {
        record,
        addr,
        seconds,
        profile,
        present,
        control,
        monitors,
        all_monitors,
        no_audio,
        via,
        named,
        mdns,
        clipboard,
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
/// Undo what a host that is no longer running left in the display server.
///
/// Says what it did, because a screen coming back on by itself is otherwise
/// unexplainable — and because the line is the only evidence a person has that
/// the previous host died rather than exited.
fn restore_what_a_dead_host_left() {
    let Ok(home) = crate::paths::home() else {
        return;
    };
    let journal = esper_keeper::journal_path(&home);
    let left = match esper_keeper::read_journal(&journal) {
        Ok(left) => left,
        Err(e) => {
            // Kept rather than deleted: it is the record of something that
            // needs putting back, and a file this build cannot read may be
            // readable by the next one.
            eprintln!("  ⚠ there is a restore journal this build cannot read: {e}");
            return;
        }
    };
    if left.is_empty() {
        return;
    }
    println!(
        "  recovery   a previous host left {} thing(s) changed on this display",
        left.len()
    );
    let mut done = 0usize;
    for undo in &left {
        match crate::put_back::put_back(undo) {
            Ok(()) => {
                println!("             put back {}", undo.describe());
                done = done.saturating_add(1);
            }
            Err(e) => println!("             could not put back {}: {e}", undo.describe()),
        }
    }
    if done == left.len() {
        let _ = esper_keeper::clear_journal(&journal);
    }
    println!();
}

pub(crate) fn paired() -> Result<(DeviceStore, Vec<[u8; 32]>), String> {
    let (store, dropped) = paths::devices()?;
    for note in &dropped {
        eprintln!("  · the device list needed correcting: {note}");
    }
    let keys: Vec<[u8; 32]> = store.iter().map(|d| d.public_key).collect();
    if keys.is_empty() {
        return Err(format!(
            "no devices are paired, so nobody could connect. Run \
             `{me} pair --listen 7645` and pair one first",
            me = net::me()
        ));
    }
    Ok((store, keys))
}

/// Serve video to whoever connects.
pub fn run_host(o: &HostOptions) -> Result<(), String> {
    net::install_crypto();
    // **Invitations that are over do not survive a restart.** Ordinarily the
    // session that spends one also removes it; this is for the times that did
    // not happen — a host that crashed mid-session, an invitation nobody took
    // up. They admit nothing either way, so this is tidying, and it is done
    // where somebody will see it said.
    for gone in crate::pairing::sweep(paths::now_secs()) {
        println!("  · the one-session pairing with {gone} is over, and is now removed");
    }
    // **The retention policy, before anything can refuse to start.** A limit
    // nobody applies is a setting that lies, and this one had no caller at all
    // until today. It is about the *file*, not about serving: a host that
    // cannot start because nothing is paired still has a trail that should
    // obey the policy somebody set for it — which is exactly the host whose
    // trail nobody is watching. Once here, and once a day in the accept loop,
    // because a host that runs for a month must not sit over its ceiling for
    // the rest of it.
    crate::trail::keep_only_what_the_policy_allows();

    // **Anything a previous host left behind, put back before this one
    // starts.**
    //
    // The keeper undoes a dead host's work as it dies, which covers every way
    // a process can end except being killed itself. This is the other half: a
    // journal on disk, applied here, so a machine whose keeper was killed too
    // comes back to itself the next time somebody starts a host rather than
    // staying dark until they think to reboot.
    restore_what_a_dead_host_left();
    // Shared rather than cloned: `Identity` is deliberately not `Clone` — a
    // private key should not be duplicated by a stray `.clone()` — and the
    // task that waits at a broker needs to sign requests for as long as the
    // host runs.
    let identity = Arc::new(paths::identity()?);
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

        // **A host dials nobody unless it is using a broker.** Then it dials
        // exactly two kinds of peer: the broker it waits at, and a device
        // that has come looking for it — the second only to punch a path
        // open. Without `--via` the outbound authority stays `NoPeers`, so
        // the default posture is unchanged.
        let dial: Arc<dyn esper_transport::PeerAuthority> = match o.via {
            Some(via) => {
                let mut may = keys.clone();
                may.push(via.key);
                Arc::new(AnyOf::new(may))
            }
            None => Arc::new(NoPeers),
        };
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
            dial,
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

    // **A supervisor that can see more than whether this process exists.**
    //
    // `esper host` is supervised rather than handed over, and until this
    // channel existed the only thing the supervisor could observe was whether
    // the pid was still there — so a host wedged holding a session it could
    // not end looked exactly like one serving happily. What gets reported is
    // kept in a watch channel because this loop is inside a session for hours
    // at a time, and the question has to be answerable *during* one.
    let (doing, watching) = tokio::sync::watch::channel(crate::supervised::Doing {
        since: paths::now_secs(),
        listening: bound.to_string(),
        serving: String::new(),
        served: 0,
    });
    if let Some(socket) = &o.watched_by {
        crate::supervised::watch_from(socket, watching, stopping.clone())?;
        println!("  watched    by whatever started this host");
        println!();
    }

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
            (None, None)
        }
    };

    // **A screen nobody can be warned about is a screen this host does not
    // serve.**
    //
    // The rule is about whether there is somebody to warn, which is why it
    // asks the machine rather than the error: a display this product cannot
    // draw on has a person in front of it who would get no indication at all,
    // and a machine with no display at all has nobody to indicate to. The
    // second is the ordinary server case and has been served since Phase 2.
    //
    // Until this existed, the first case printed a line and served anyway —
    // the documented version of a guarantee rather than the enforced one, and
    // Phase 15 is where that distinction is the whole point.
    let a_screen_is_attended =
        std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some();
    let can_indicate = indicator
        .as_ref()
        .is_some_and(crate::presence_task::Indicator::can_show);
    // **What a controller will be told, decided here** — the one place that
    // knows both whether there is a screen and whether anything can be drawn
    // on it. A session that worked this out for itself from whether it holds
    // an indicator would report a headless server and a broken display as the
    // same thing, and those are opposite situations.
    let seen = if can_indicate {
        esper_proto::Seen::Shown
    } else if a_screen_is_attended {
        esper_proto::Seen::NotShown
    } else {
        esper_proto::Seen::NoScreen
    };
    if a_screen_is_attended && !can_indicate {
        if crate::settings::flag("host.serve_without_indicator", false) {
            println!("             ⚠ serving anyway: host.serve_without_indicator is on.");
            println!("             Somebody at this machine will not be told it is watched.");
            println!();
        } else {
            println!("             This machine has a display and cannot show that it is being");
            println!("             watched, so it will not serve. Set");
            println!("             `host.serve_without_indicator true` to serve anyway, or run");
            println!("             the host where the indicator works.");
            println!();
            return Err(
                "this display cannot show a session indicator, so no session was offered"
                    .to_owned(),
            );
        }
    } else if !a_screen_is_attended && indicator.is_none() {
        println!("             No display, so nobody is here to be told: sessions are served.");
        println!();
    }

    // **The audit trail, opened before any session.** A host that recorded
    // nothing would be a host whose owner cannot answer "who was on this
    // machine, and when" — which is a question people ask after the fact,
    // when it is too late to start recording.
    let log_path = paths::audit_log()?;
    println!("  audit      {}", log_path.display());
    let log = Log::at(log_path);
    // **One for the host, not one per session.** It owns the terminal reader,
    // and its record of who was just accepted is what stops a reconnect after
    // a network blip asking the same person the same question again.
    let policy = Arc::new(crate::ask_screen::wherever_somebody_is());
    // **A policy that fails closed has to fail visibly.** Said here, on the
    // line before the audit path, because somebody who switched this on and
    // whose machine cannot be asked should read it when they start the host
    // rather than meet it in a refusal at three in the morning.
    if let Some(state) = policy.lock_state() {
        use esper_presence::locked::Locked;
        // **What it means, per state, rather than one sentence for three.**
        // The first version said "unattended devices wait" whatever the answer
        // was, which on an unlocked screen is the opposite of the truth.
        let effect = match state {
            Locked::No(_) => "unattended devices may walk in",
            Locked::Yes(_) => "unattended devices wait until somebody unlocks it",
            Locked::CannotTell => "counted as locked, so none will walk in unasked",
        };
        println!("  locked     {} — {effect}", state.describe());
        if matches!(state, Locked::CannotTell) {
            println!("             Turn policy.deny_when_locked off if that is not what you want.");
        }
    }
    if policy.can_ask() {
        println!(
            "  consent    a device without `unattended` is asked about at {}",
            policy.asked_at()
        );
    } else {
        println!("  consent    nothing to ask at: only devices holding `unattended` may connect");
    }

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
        // **Waiting at a broker, if one was named.** Tier 3: a controller with
        // no route to this machine asks the broker, the broker introduces
        // them, and the two find a direct path. There is no default broker
        // anywhere, so this happens only because somebody said so.
        if let Some(via) = o.via {
            let candidates = crate::broker_cmd::candidates(bound, via.addr);
            println!("  broker     {} — waiting to be found", via.addr);
            println!(
                "      offering {}",
                candidates
                    .iter()
                    .map(std::string::ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            tokio::spawn(crate::broker_cmd::wait_to_be_found(
                endpoint.clone(),
                Arc::clone(&identity),
                via,
                keys.clone(),
                candidates,
            ));
        }

        // Advertised while listening, and taken back when this stops. Without
        // the goodbye, every browser on the segment keeps offering a machine
        // that has gone until the record expires, and somebody clicks a device
        // that is not there.
        let mdns = if o.mdns {
            match crate::discover_cmd::advertise(bound).await {
                Ok(handle) => {
                    println!("  announcing on this network as {}", handle.instance());
                    println!("             through {}", handle.wires());
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

        // When the trail's retention was last applied. A host that runs for
        // months applies it as it goes rather than only at the start.
        let mut trail_pruned_at = Instant::now();

        let outcome = loop {
            // The retention policy, again, if a day has passed. Checked here
            // rather than on a timer of its own because this loop is already
            // woken often enough and a task for one file read a day would be a
            // task to reason about for nothing.
            if trail_pruned_at.elapsed() >= crate::trail::RETENTION_INTERVAL {
                trail_pruned_at = Instant::now();
                crate::trail::keep_only_what_the_policy_allows();
            }

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
                        // **Pause does nothing with nothing connected.** There
                        // is no picture to hold, and a key that appeared to do
                        // something while no session existed would be a key
                        // whose effect nobody could predict.
                        event = wait_for_press(presses.as_mut()) => {
                            if event == esper_presence::HostEvent::Panic {
                                panic_now(indicator.as_ref());
                            }
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
                        event = wait_for_press(presses.as_mut()) => {
                            if event == esper_presence::HostEvent::Panic {
                                panic_now(indicator.as_ref());
                            }
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
            // **The cell this connection's pacing reads from**, taken while
            // the connection is still nameable here. What goes into it is what
            // the controller measures and reports; the transport's congestion
            // controller is what acts on it. See `esper_transport::pacing`.
            let capacity = endpoint.capacity_of(&conn);
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
            // Taken before the session is moved into `serve_one`, because the
            // rule for locking afterwards is this device's to answer.
            let peer = *session.peer_key();
            println!("  · {from} connected");
            // Who this host is serving, for anybody watching it. Taken here
            // because the session is about to be moved into `serve_one`.
            doing.send_modify(|d| d.serving = session.peer_fingerprint().inline());
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
                    policy: &policy,
                    capacity,
                    seen,
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
            doing.send_modify(|d| {
                d.serving.clear();
                d.served = d.served.saturating_add(1);
            });
            // **However it ended**, including badly: a session that dropped
            // because the network went away left the screen exactly as
            // readable as one that closed politely.
            let (rule, decided) = lock_rule_for(&peer);
            lock_after_session(rule, decided, policy.how_the_last_one_was_allowed());
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

/// Wait for `SIGUSR1`, or for ever when it could not be listened for.
///
/// The same shape as [`wait_for_press`] and for the same reason: a `select!`
/// branch needs a future either way.
async fn wait_for_usr1(signal: Option<&mut tokio::signal::unix::Signal>) {
    match signal {
        Some(s) => {
            s.recv().await;
        }
        None => std::future::pending().await,
    }
}

/// Whether the picture stops, given the two separate reasons it might.
///
/// **One function because both reasons are live at once**, and the first
/// version had them fighting: the indicator coming back sent the *person's*
/// pause state, and holding for a missing frame sent `true` — so a person who
/// pressed pause during a compositor restart had their pause released by the
/// frame returning, and a screen they had deliberately hidden came back
/// without them touching anything.
const fn picture_is_held(paused: bool, indicator_missing: bool) -> bool {
    paused || indicator_missing
}

/// How often a running session's lock state is re-asked.
///
/// Five seconds. The bound exists so somebody can lock up and leave, and
/// nobody walks away in under five seconds — while a shorter interval would
/// mean a subprocess more often than a session needs anything else.
const LOCK_CHECKED_EVERY: Duration = Duration::from_secs(5);

/// Watch for the screen locking, and say so once, or `None` when nothing asked.
///
/// **Nothing is spawned when the policy is off**, which is the default: the
/// probe runs a subprocess and opens a display connection, and a host nobody
/// switched this on for should pay for neither.
///
/// Uses only what is free to ask — see [`crate::consent::LockState::locked_cheaply`].
/// A machine where nothing free can answer gets the bound on arrival and not
/// during a session, which the host says when it starts.
fn watch_for_a_lock(
    policy: Arc<crate::consent::AskAtTheHost>,
    mut stopping: tokio::sync::watch::Receiver<bool>,
) -> Option<tokio::sync::mpsc::Receiver<String>> {
    policy.lock_state_cheaply()?;
    // One deep: the first answer ends the session, so a second would have
    // nowhere to go.
    let (tx, rx) = tokio::sync::mpsc::channel(1);
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = stopping.changed() => return,
                () = tokio::time::sleep(LOCK_CHECKED_EVERY) => {}
            }
            // On a blocking thread: the probe shells out to the session
            // manager, and doing that on the runtime would stall every task
            // sharing it — including the one sending pictures.
            let policy = Arc::clone(&policy);
            let Ok(state) = tokio::task::spawn_blocking(move || policy.lock_state_cheaply()).await
            else {
                return;
            };
            // **Only a positive answer ends a session.** "Cannot tell" is
            // where the free probes have nothing to say, and treating that as
            // locked here would end every session on such a machine five
            // seconds in — the opposite of what an arriving device gets, and
            // right for the opposite reason: refusing to *start* is
            // recoverable, and ending a session somebody is using is not.
            if let Some(locked @ esper_presence::locked::Locked::Yes(_)) = state {
                // The whole sentence rather than the source's name: the first
                // version sent only where the answer came from, and the trail
                // read "the screen at this host is the file
                // ESPER_SCREEN_LOCKED names".
                let _ = tx.send(locked.describe()).await;
                return;
            }
        }
    });
    Some(rx)
}

/// Wait for the screen to be reported locked, or for ever when nobody is
/// watching.
///
/// The same shape as [`wait_for_press`] and for the same reason: a `select!`
/// branch needs a future either way.
async fn wait_for_a_lock(watching: Option<&mut tokio::sync::mpsc::Receiver<String>>) -> String {
    match watching {
        Some(rx) => match rx.recv().await {
            Some(how) => how,
            // The watcher stopped, which happens when the host is shutting
            // down. Waiting for ever is right: the branch that owns the
            // shutdown is the one that should end this session.
            None => std::future::pending().await,
        },
        None => std::future::pending().await,
    }
}

/// Wait for an answer to a question somebody at this machine is being asked.
///
/// **Pending while no question is out**, so a session with nothing to wait for
/// pays nothing — and, more to the point, so the picture keeps flowing while
/// somebody decides. A prompt waits as long as this host's
/// `consent.answer_within`, which is forty-five seconds by default, and a
/// session loop that blocked for it would hold the screen still for the whole
/// time.
///
/// A question can outlive the session that asked it: a blocking task cannot be
/// cancelled, so a controller that disconnects while somebody is deciding
/// leaves the prompt up until it times out, and the answer then reaches
/// nothing. The same is true of the prompt that opens a session, and for the
/// same reason — the asking layer has no cancellation and inventing one here
/// would only cover this one caller.
async fn wait_for_an_answer(asking: Option<&mut Asking>) -> TheyAnswered {
    match asking {
        Some(handle) => match handle.await {
            Ok(answer) => answer,
            // The task panicked or was cancelled. Neither is a person saying
            // yes, and refusing is the only safe reading.
            Err(_) => TheyAnswered {
                wanted: esper_proto::Permission::View,
                verdict: crate::consent::Widening::Refused(
                    esper_proto::Reason::PROMPT_TIMEOUT,
                    "the question could not be put".to_owned(),
                ),
                keyboard: None,
            },
        },
        None => std::future::pending().await,
    }
}

/// A question that is in front of somebody at this machine.
type Asking = tokio::task::JoinHandle<TheyAnswered>;

/// What came back from asking.
struct TheyAnswered {
    /// What was asked for.
    wanted: esper_proto::Permission,
    /// What the rule and the person between them decided.
    verdict: crate::consent::Widening,
    /// An injector opened while asking, for a session that had none.
    ///
    /// **Opened before the question rather than after the answer**, so a host
    /// with nothing to inject with refuses without disturbing anybody: a
    /// prompt whose yes cannot be honoured is worse than a refusal, because
    /// somebody answered it. Dropped unopened if the answer is no.
    keyboard: Option<esper_input::Dispatcher>,
}

/// Wait until an indicator that is off the screen has been off it too long.
///
/// Pending while nothing is wrong, so the branch costs nothing in the
/// ordinary case; and computed from *when* it went rather than reset on every
/// pass through the loop, so a frame that flickers cannot keep a session alive
/// for ever by renewing its own grace.
async fn out_of_grace(since: Option<std::time::Instant>) {
    match since {
        Some(when) => {
            let left = INDICATOR_GRACE.saturating_sub(when.elapsed());
            tokio::time::sleep(left).await;
        }
        None => std::future::pending().await,
    }
}

/// Wait for the screen's opinion of the indicator to change.
///
/// **Never on a host with no indicator**, which is the headless case: a
/// machine with no display shows nothing and has nobody to show it to, so this
/// branch is a future that never completes rather than a session that stops
/// waiting for an answer nobody can give.
async fn wait_for_the_screen(sightings: Option<&mut crate::presence_task::Sightings>) {
    match sightings {
        Some(s) => {
            // A changed channel whose sender is gone would otherwise make this
            // branch fire continuously: the indicator's thread ending is not
            // news about the screen, and the session has other ways to notice
            // the process is going.
            if s.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
        None => std::future::pending().await,
    }
}

/// Wait for one of the host's hotkeys, or for ever when there are none.
///
/// A `select!` branch needs a future either way, and a host with no working
/// hotkey must not fall out of its loop immediately — so the absence is a
/// future that never completes rather than a special case at every call site.
///
/// **Which key it was comes back**, because there are two now and they do
/// opposite things: one ends every session, the other holds one.
async fn wait_for_press(
    presses: Option<&mut crate::presence_task::Presses>,
) -> esper_presence::HostEvent {
    match presses {
        // One press, taken from the queue: acted on once by whoever was
        // waiting. See `presence_task::Presses` for what the first version of
        // this did instead.
        //
        // A closed queue means the indicator's thread is gone, which happens
        // when the host is shutting down — and then waiting for ever is right,
        // because the branch that owns the shutdown is the one that should act.
        Some(queue) => match queue.recv().await {
            Some(event) => event,
            None => std::future::pending().await,
        },
        None => std::future::pending().await,
    }
}

/// Which of two rules decided whether to lock, for the host to say so.
///
/// **Because a per-device answer that took effect silently would be one
/// nobody could tell from the host's own rule**, and the two are set in
/// different places by different commands. A person who set one device never
/// to lock and finds the screen locked needs to know which rule they are
/// arguing with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decided {
    /// The device's own `lock_after`.
    ByTheDevice,
    /// `host.lock_after_session`, which is every other device's answer.
    ByTheHost,
}

impl Decided {
    const fn describe(self) -> &'static str {
        match self {
            Self::ByTheDevice => "this device",
            Self::ByTheHost => "this host",
        }
    }
}

/// The host's own rule, from `host.lock_after_session`.
///
/// An unknown word cannot reach here — the setting is a choice, so a typo is
/// refused rather than defaulted — which is why anything unrecognised falls to
/// the shipping rule rather than to `Never`. A misreading that silently
/// stopped locking would be a misreading nobody could see.
fn lock_rule_here() -> LockAfter {
    LockAfter::from_word(&crate::settings::text("host.lock_after_session"))
        .unwrap_or(LockAfter::Unattended)
}

/// Which of the two rules applies, and which one it was.
///
/// Pure, so the precedence is testable without a device list and a config
/// file: **the device's answer wins where it has one**, which is the whole
/// feature, and a rule that silently went the other way would look exactly
/// like a machine whose setting nobody had noticed.
const fn lock_rule(theirs: Option<LockAfter>, host: LockAfter) -> (LockAfter, Decided) {
    match theirs {
        Some(rule) => (rule, Decided::ByTheDevice),
        None => (host, Decided::ByTheHost),
    }
}

/// Which rule applies to a session with this device, and which one it was.
///
/// **The store is read here rather than remembered from the session's start**:
/// somebody may have changed their mind while it ran, and the answer wanted is
/// the one in force when the screen is about to be left.
fn lock_rule_for(peer: &[u8; 32]) -> (LockAfter, Decided) {
    let theirs = paths::devices()
        .ok()
        .and_then(|(store, _)| store.get(peer).and_then(|d| d.lock_after));
    lock_rule(theirs, lock_rule_here())
}

/// Whether to lock the screen now that a session has ended.
///
/// # Why an ordinary ending is worth locking for
///
/// The panic key has always locked, and nothing else did. So a device that
/// connects to an unattended machine, does something, and disconnects leaves
/// that machine logged in, unlocked, and showing whatever it was left showing
/// — for anybody who walks past. That is the same exposure a screen lock
/// exists for, reached by a door nobody thought of as a door.
///
/// # Why it is derived rather than configured
///
/// The host already knows whether anybody was here: a session somebody
/// answered a prompt for was allowed by a person who is still at the machine,
/// and one nobody was asked about was not. So the rule needs no setting to be
/// right in both cases, and the setting exists only for the two deliberate
/// overrides.
///
/// **Not knowing counts as nobody.** A session that reached here without the
/// policy recording how it was allowed is one this host cannot vouch for, and
/// the safe reading of an unknown is the one that locks.
///
/// # Why the display is a parameter
///
/// **A machine with no screen has nothing to leave readable.** A server
/// reached over ssh cannot lock and does not need to, and a warning after
/// every session on such a host would be an alarm about a door that is not
/// there. Passed in rather than read here so the rule can be tested without
/// arranging an environment — the same reason the clock is a parameter
/// everywhere else in this tree.
///
/// The question is whether there is a *display*, not whether there is a
/// locker: a desktop with no locker installed is a real problem and does get
/// the warning.
const fn lock_now(
    after: LockAfter,
    allowed: Option<crate::consent::Allowed>,
    has_display: bool,
) -> bool {
    if !has_display {
        return false;
    }
    match after {
        LockAfter::Never => false,
        LockAfter::Always => true,
        LockAfter::Unattended => match allowed {
            Some(how) => !crate::consent::somebody_answered(how),
            None => true,
        },
    }
}

/// Whether this machine has a screen somebody could read.
fn has_a_display() -> bool {
    std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some()
}

/// Lock the screen after a session, if this machine should.
fn lock_after_session(
    after: LockAfter,
    decided: Decided,
    allowed: Option<crate::consent::Allowed>,
) {
    if !lock_now(after, allowed, has_a_display()) {
        // **Said only when a device's own answer is what decided.** The host's
        // rule not locking is the ordinary case, and a line about it after
        // every session would be a line nobody reads. A device set apart from
        // the rest is worth one.
        if decided == Decided::ByTheDevice {
            println!(
                "  · not locking the screen: {} is set to {}",
                decided.describe(),
                after.word()
            );
        }
        return;
    }
    // **Only as far as the display this host was serving.** Nobody asked for
    // this lock -- a session ended, that is all -- so it must not reach past
    // the screen the session was on. A locker that asks the session manager
    // would lock the *person's* desktop when a host is serving a second X
    // server, another seat or a test, and the first thing that would tell
    // them is their own screen going black.
    //
    // The panic key keeps the full reach, and correctly: somebody standing at
    // the machine has asked for it all to stop.
    match esper_presence::lock_reaching(esper_presence::Reach::OnlyThisDisplay) {
        Ok(how) => println!(
            "  · locked the screen with {how} ({} asked for it)",
            decided.describe()
        ),
        // Said, not swallowed, for the same reason the panic key says it:
        // somebody who believes their screen is locked and is wrong is worse
        // off than somebody who has been told.
        // The rule is named here as well as on the way out, so a machine
        // with no locker still says which of the two decided it should have
        // locked — otherwise the one line a person sees when it matters is
        // the one that does not say why.
        Err(e) => println!(
            "  · ⚠ COULD NOT LOCK THE SCREEN AFTER THE SESSION ({} asked for it): {e}",
            decided.describe()
        ),
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
/// Dial every direct rung at once and keep the first session that opens.
///
/// `None` when there was nothing to try. `Some(Err(..))` names every address
/// that failed, because a ladder that gave up without saying what it attempted
/// is the error message this project refuses to print.
async fn reach_directly(
    endpoint: &Endpoint,
    rungs: &[crate::ladder::Rung],
    hello: Hello,
) -> Option<Result<(Session, crate::ladder::Tier), Vec<(SocketAddr, String)>>> {
    if rungs.is_empty() {
        return None;
    }
    let addresses: Vec<SocketAddr> = rungs.iter().map(|r| r.addr).collect();
    match esper_rendezvous::race_to(endpoint, &addresses, DIAL_EACH).await {
        Ok(conn) => {
            let at = conn.remote_address();
            let tier = rungs
                .iter()
                .find(|r| r.addr == at)
                .map_or(crate::ladder::Tier::Given, |r| r.tier);
            match Session::open(conn, hello).await {
                Ok(session) => Some(Ok((session, tier))),
                // A peer that answered and then refused is not a reason to
                // try the next address: it is *the* device, and it said no.
                Err((e, _)) => Some(Err(vec![(at, opening_failed(e))])),
            }
        }
        Err(failures) => Some(Err(failures)),
    }
}

/// How long each direct candidate gets.
///
/// They are dialled together, so this is how long the slowest may take rather
/// than a sum. Five seconds is a slow handshake on a bad network.
const DIAL_EACH: Duration = Duration::from_secs(5);

/// What to say when no rung worked and there is no broker.
fn no_way_in(failures: &[(SocketAddr, String)], rungs: &[crate::ladder::Rung]) -> String {
    if rungs.is_empty() {
        return "there is nowhere to try: that device has not been seen on this network, \
             has no remembered address, and no broker was named. Give an address, or \
             `--via <broker> --broker-key <key>`"
            .to_owned();
    }
    let mut out = String::from("could not reach it:");
    for (addr, why) in failures {
        let tier = rungs
            .iter()
            .find(|r| r.addr == *addr)
            .map_or("", |r| r.tier.describe());
        out.push_str(&format!("\n      {addr} ({tier}): {why}"));
    }
    out.push_str("\n  No broker was named, so there was nothing else to try.");
    out
}

/// Ask the segment where a device is.
///
/// A multicast question and whatever answers within [`ladder::LISTEN_FOR`].
/// Failures are not reported: a machine with no multicast route is a machine
/// with no LAN rung, which is ordinary, and the other rungs are unaffected.
async fn on_the_segment(key: &[u8; 32]) -> Vec<SocketAddr> {
    let wanted = esper_crypto::Fingerprint::of(key).hex();
    let Ok(mut browser) =
        esper_discovery::Browser::start(&crate::settings::mdns_interfaces()).await
    else {
        return Vec::new();
    };
    if browser.ask().await.is_err() {
        return Vec::new();
    }
    if browser.listen(crate::ladder::LISTEN_FOR).await.is_err() {
        return Vec::new();
    }
    browser
        .found()
        .into_iter()
        .filter(|f| f.fingerprint.as_deref() == Some(wanted.as_str()))
        .map(|f| f.address)
        .collect()
}

/// Remember where a device answered.
///
/// Best-effort and quiet: a store that cannot be written is worth a line on
/// the console but is not a reason to end a session that is working.
fn remember_where(key: &[u8; 32], addr: SocketAddr, private: bool) {
    let Ok((mut store, _)) = paths::devices() else {
        return;
    };
    let Some(device) = store.get_mut(key) else {
        return;
    };
    let text = addr.to_string();
    // **The hardware address, while the neighbour entry is fresh.** A session
    // that just ended used this address, so the kernel has one; asked for
    // later it would be gone. It is only ever learned, never asked for, and
    // only for one purpose — see `wake_cmd`.
    let mac = crate::wake_cmd::mac_for(addr.ip());
    let mac_known = mac.is_none() || device.last_mac == mac;
    let known = device.last_address.as_deref() == Some(text.as_str());
    let private_known = !private || device.overlay_address.as_deref() == Some(text.as_str());
    if known && private_known && mac_known {
        // Nothing changed, so nothing is written: a session should not cost a
        // disk write for a fact already recorded.
        return;
    }
    if private {
        device.remember_private(&text, paths::now_secs());
    } else {
        device.remember(&text, paths::now_secs());
    }
    if let Some(mac) = mac {
        device.last_mac = Some(mac);
    }
    if let Err(e) = paths::save_devices(&store) {
        eprintln!("      · could not remember where that device was ({e})");
    }
}

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
            format!(
                "      {} pair --listen <addr>            (on the host)",
                net::me()
            ),
            format!("      {} pair <host-addr> --code <code>  (here)", net::me()),
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

/// How long a session waits for the screen to confirm the indicator.
///
/// **A round trip to the display server, generously.** The frame is created
/// and read back on the indicator's own thread, which is polling at
/// thirty-millisecond intervals, so this is that plus room for a machine under
/// load — and short enough that a session refused for this reason is refused
/// while the person who started it is still watching it start.
const INDICATOR_PATIENCE: Duration = Duration::from_secs(2);

/// How long a running session tolerates an indicator that is not on screen.
///
/// **Held rather than ended, for as long as this.** A compositor restarting
/// takes the frame away for a moment and putting it back is automatic, so
/// ending the session over it would be a product that drops connections when
/// somebody logs into their desktop. Something *drawn over* the frame is not
/// recoverable from here, and streaming a screen nobody can see is being
/// watched is the thing this must not do — so the picture stops at once and
/// the session ends if the frame does not come back.
const INDICATOR_GRACE: Duration = Duration::from_secs(10);

/// How long the sending thread sleeps between looks while the picture is held.
///
/// Short enough that letting go is not perceptible — a person who presses the
/// key expects the far end to see their screen again, not to see it in a
/// moment — and long enough that a paused session is not a thread spinning.
const HELD_TICK: Duration = Duration::from_millis(50);

/// How long to wait for a grant before saying it is being waited for.
///
/// The ordinary answer arrives inside a round trip, so a note printed at once
/// would be a note about nothing. A host that has to ask somebody takes as
/// long as they take, and that is the case worth explaining.
const SAY_WE_ARE_WAITING: Duration = Duration::from_millis(750);

/// Agree what this session may do.
///
/// The store is read **per session**, not once at startup: a permission
/// granted or taken away while the host is running has to apply to the next
/// connection, and a host that had to be restarted for that would be a host
/// whose permissions nobody trusts.
async fn agree(
    control: &mut Control,
    device: &[u8; 32],
    policy: &Arc<crate::consent::AskAtTheHost>,
) -> Result<(esper_proto::SessionKind, Vec<Permission>), String> {
    let (store, _) = paths::devices()?;
    let record = store
        .get(device)
        .ok_or_else(|| "this device is not in the paired list any more".to_owned())?;
    let id = esper_transport::parked::new_session_id()
        .map_err(|e| format!("no randomness for a session identifier: {e}"))?;

    match answer_session(
        control,
        Arc::clone(policy) as _,
        record,
        *id.as_bytes(),
        // What this host can send. One entry, because software H.264 is what
        // Phase 3 shipped -- and stated rather than assumed, so the day a
        // second codec arrives the refusal below is already correct.
        &[esper_proto::Codec::H264],
    )
    .await
    {
        Ok(Answered::Granted { grant, kind }) => {
            if let Some(how) = policy.how_the_last_one_was_allowed() {
                println!("      allowed by {}", crate::consent::describe(how));
            }
            Ok((kind, grant.granted))
        }
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
        //
        // **Whether the pairing still admits a connection**, not merely
        // whether the device is in the list: a support pairing is spent by the
        // session it was made for, and the one place that has to be true is
        // here, before a handshake completes. A check further in would be a
        // check a spent key had already got past.
        self.0.borrow().admits(public_key, paths::now_secs())
    }

    fn describe(&self) -> &'static str {
        "any device whose pairing this machine still honours, as it stands now"
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

/// Record that a revocation closed a session.
///
/// Its own entry beside the session's close, because a `session_closed` that
/// said only "the session ended" reads exactly like a user finishing — and the
/// difference between those two is the whole reason a security log exists.
fn record_revocation(log: &Log, peer: &[u8; 32]) {
    if let Err(e) = log.append(&Entry::now(peer, Event::Revoked { sessions_closed: 1 })) {
        tracing::warn!(error = %e, "could not record a revocation");
    }
}

/// Record that a live session was narrowed without being ended.
///
/// Its own event rather than a [`Event::Revoked`] with a count: nothing
/// closed, and a log saying a session was closed when it is still running is
/// worse than no line at all.
fn record_narrowing(log: &Log, peer: &[u8; 32], granted: &[Permission]) {
    if let Err(e) = log.append(&Entry::now(
        peer,
        Event::SessionNarrowed {
            granted: granted.to_vec(),
        },
    )) {
        tracing::warn!(error = %e, "could not record a narrowing");
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
    log: Log,
    peer: [u8; 32],
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
        record_revocation(&log, &peer);
        media.close(esper_proto::Reason::REVOKED);
        return;
    }
}

/// A one-session pairing, spent when the session opens and removed when it
/// ends.
///
/// **Spent on the way in, not on the way out.** A host that marked the pairing
/// as used only at the end would hand a second session to anybody who could
/// end the first one abruptly — a crash, a pulled cable — and the crash case
/// is exactly when nobody is watching. Written to the store immediately, so a
/// host that dies mid-session leaves a pairing that is used rather than one
/// that is fresh.
///
/// Removing the record is the tidying, and it is not the security property:
/// the spent mark already refuses everything.
struct Spent {
    peer: [u8; 32],
    /// False for a pairing that lasts, which makes this a guard the caller can
    /// hold unconditionally rather than a branch at every exit.
    lapses: bool,
}

impl Spent {
    /// Mark the pairing used, if it is one that is spent by being used.
    fn open(peer: [u8; 32]) -> Self {
        Self {
            peer,
            lapses: spend_pairing(peer),
        }
    }
}

/// Mark a one-session pairing as used, and say whether it was one.
///
/// Written to the store immediately: a host that dies mid-session must leave a
/// pairing that is used rather than one that is fresh.
fn spend_pairing(peer: [u8; 32]) -> bool {
    let Ok((mut store, _)) = paths::devices() else {
        return false;
    };
    let Some(device) = store.get_mut(&peer) else {
        return false;
    };
    if !device.pairing.is_once() {
        return false;
    }
    device.spend();
    if let Err(e) = paths::save_devices(&store) {
        // Said, and the session continues: refusing to serve because a disk is
        // full would be a worse outcome than a pairing spent a moment later.
        eprintln!("      · could not mark the invitation as used ({e})");
    }
    true
}

/// Remove a device record, quietly. For a pairing that has ended by its own
/// terms rather than by anybody's decision.
fn forget(peer: &[u8; 32]) {
    let Ok((mut store, _)) = paths::devices() else {
        return;
    };
    if store.remove(peer).is_some() {
        let _ = paths::save_devices(&store);
    }
}

impl Drop for Spent {
    fn drop(&mut self) {
        if !self.lapses {
            return;
        }
        let Ok((mut store, _)) = paths::devices() else {
            return;
        };
        let Some(gone) = store.remove(&self.peer) else {
            return;
        };
        if let Err(e) = paths::save_devices(&store) {
            eprintln!("      · could not remove the spent invitation ({e})");
            return;
        }
        println!(
            "      the invitation is used up: {} is no longer paired",
            gone.name
        );
        if let Ok(path) = paths::audit_log() {
            let entry = Entry::now(&self.peer, Event::Unpaired { name: gone.name });
            if let Err(e) = Log::at(path).append(&entry) {
                eprintln!("      · not recorded in the audit log: {e}");
            }
        }
    }
}

/// An open session in the audit trail, closed however the session ends.
///
/// A guard rather than two calls, because a session with an open record and no
/// close is exactly the gap an audit log exists to not have — and `serve_one`
/// has eight ways out.
///
/// Owns its log rather than borrowing it, because a forward session outlives
/// the function that opened it: the record has to travel into the task that is
/// still running. A `Log` is a path, so the clone is free.
struct Recorded {
    log: Log,
    peer: [u8; 32],
    started: Instant,
    /// Why it ended, set by whatever ended it. `None` means it simply
    /// finished.
    why: Mutex<Option<String>>,
}

impl Recorded {
    /// Record the open. Best-effort: a full disk must not end a session.
    fn open(log: &Log, peer: [u8; 32], granted: &[Permission], allowed: Option<String>) -> Self {
        if let Err(e) = log.append(&Entry::now(
            &peer,
            Event::SessionOpened {
                granted: granted.to_vec(),
                // The connectivity ladder arrives in Phase 8. Until there is
                // more than one way to reach a host, saying "direct" is
                // accurate rather than a placeholder.
                tier: "direct".to_owned(),
                allowed,
            },
        )) {
            tracing::warn!(error = %e, "could not record the start of a session");
        }
        Self {
            log: log.clone(),
            peer,
            started: Instant::now(),
            why: Mutex::new(None),
        }
    }

    /// Say why this session is ending, before it does.
    ///
    /// **Because "the session ended" is what every ending used to say.** A
    /// session closed by a revocation, by a locked screen, by the host
    /// shutting down and by a person finishing all wrote the same line — and
    /// telling those apart afterwards is most of what a security log is for.
    /// The controller's own outbound entry has carried its reason since it
    /// existed; this is the host's side of the same sentence.
    ///
    /// Takes `&self` so the loop that decides can say so without owning the
    /// record, and keeps the *first* reason: the thing that ended a session is
    /// the first thing that ended it, and whatever unwinds afterwards is
    /// consequence rather than cause.
    fn ending(&self, why: &str) {
        if let Ok(mut held) = self.why.lock()
            && held.is_none()
        {
            *held = Some(why.to_owned());
        }
    }
}

impl Drop for Recorded {
    fn drop(&mut self) {
        let why = self
            .why
            .lock()
            .ok()
            .and_then(|held| held.clone())
            .unwrap_or_else(|| "the session ended".to_owned());
        if let Err(e) = self.log.append(&Entry::now(
            &self.peer,
            Event::SessionClosed {
                duration_secs: self.started.elapsed().as_secs(),
                reason: why,
            },
        )) {
            tracing::warn!(error = %e, "could not record the end of a session");
        }
    }
}

type Served = Result<(String, Option<Parked>), (String, Option<Parked>)>;

#[allow(clippy::result_large_err)]
async fn serve_one(
    session: Session,
    o: &HostOptions,
    parked: Option<Parked>,
    stopping: tokio::sync::watch::Receiver<bool>,
    machine: Machine<'_>,
) -> Served {
    // **Whatever happens, the peer gets a chance to read our last word.**
    // Every refusal and every ending here is a message on a stream, and
    // returning from this function closes the connection — which discards
    // what has not been sent. Held before the session is moved on, and waited
    // on after, so none of the paths inside have to remember.
    let closer = session.media_channel();
    let outcome = serve_session(session, o, parked, stopping, machine).await;
    closer.linger(GOODBYE).await;
    outcome
}

#[allow(clippy::result_large_err)]
async fn serve_session(
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
        policy,
        capacity,
        seen: machine_seen,
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
    let (kind, granted) = match agree(&mut control, &device, policy).await {
        Ok(g) => g,
        Err(e) => {
            // **A session that was refused is the one worth recording.** An
            // attempt somebody declined, or that nobody was there to allow,
            // is a fact about this machine's security posture -- and it was
            // the only refusal in the product that left no trace, because
            // until now nothing could refuse a session for a reason a person
            // chose.
            if let Err(w) = log.append(&Entry::now(&device, Event::Refused { reason: e.clone() })) {
                tracing::warn!(error = %w, "could not record a refused session");
            }
            return Err((e, parked));
        }
    };
    println!(
        "      granted {} — for a {} session",
        granted
            .iter()
            .map(|p| p.name())
            .collect::<Vec<_>>()
            .join(", "),
        kind.name()
    );
    // **How it was allowed, on the line that records it opening.** The host
    // prints this for whoever is watching; a log that did not carry it would
    // read the same for a session somebody agreed to and one nobody was asked
    // about, which are the two answers an auditor is trying to tell apart.
    let recorded = Recorded::open(
        log,
        device,
        &granted,
        policy
            .how_the_last_one_was_allowed()
            .map(|how| crate::consent::describe(how).to_owned()),
    );
    // **One session means this one.** Spent here, at the moment the session is
    // agreed, and the record removed when this guard drops — however it drops.
    let spent = Spent::open(device);

    // What this device may do, re-read whenever it is asked rather than frozen
    // here. The terminal service uses it to decide whether to *keep* a shell
    // standing, which is a question about now and not about when the session
    // opened.
    let live = crate::permits::Live::new(permits.clone(), device, granted.clone());

    // **The controller said which kind of session this is, and the permission
    // it needs was checked before the answer went out.** It used to be
    // inferred from the shape of the grant — terminal and not view must mean a
    // shell — which meant a device holding both could never open one and a
    // third kind had nothing to be inferred from.
    if kind == esper_proto::SessionKind::Forward {
        // **A forward does not hold the host.** It is open for as long as
        // somebody has a database client running, which is all day — and this
        // loop serves one session at a time, so an awaited forward would mean
        // no shell, no screen and no second device until it ended. Nothing in
        // it touches the screen, the capture backend or a parked session, so
        // it is the one kind that can simply be let go of.
        let allow = o.forward.clone();
        let at_once = o.forwards_at_once;
        let log = log.clone();
        let live = live.clone();
        let closer = media.clone();
        let from = session.remote_address();
        tokio::spawn(async move {
            // The control stream the session was agreed on is held for the
            // life of the forward: dropping it would reset a stream the peer
            // has no reason to think is finished.
            let _agreed = control;
            let watchdog = tokio::spawn(close_when_revoked(
                closer.clone(),
                live.clone(),
                Permission::Forward,
                log.clone(),
                device,
            ));
            match crate::forward_cmd::serve(&session, &allow, at_once, &live, &log, &device).await {
                Ok(what) => println!("  · {from} stopped forwarding: {what}"),
                Err(e) => eprintln!("  · {from} stopped forwarding: {e}"),
            }
            watchdog.abort();
            // The same last word every other ending gets, since this one does
            // not go back through `serve_one`.
            closer.linger(GOODBYE).await;
            drop((recorded, spent));
        });
        return Ok(("forwarding, in the background".to_owned(), parked));
    }

    // **A file session is not a screen session either.** A copy over one would
    // start capture, an encoder and a presenter on a host that was asked to
    // hand over one file — and would have needed the `view` permission to do
    // it, which is a much larger grant than moving a file into a directory.
    // **Lock the screen, say what happened, and end.**
    //
    // The shortest session this product has: no capture, no encoder, no
    // presenter, no input. A device may hold this without holding `view`,
    // which is the point of it being its own kind — "you may put my machine
    // away when you are done" is a smaller thing to grant than "you may watch
    // my screen".
    if kind == esper_proto::SessionKind::Lock {
        // **The display this host serves, and no further out.** A remote
        // request is not somebody standing at the machine, so it must not
        // reach the login session the way the panic key deliberately does —
        // the same rule an automatic lock follows, and for the same reason: a
        // host on a scratch display would otherwise lock the person's desktop.
        let locked =
            esper_presence::lock::lock_reaching(esper_presence::lock::Reach::OnlyThisDisplay);
        let (how, worked) = match &locked {
            Ok(program) => {
                println!("      locked     this machine's screen with {program}");
                ((*program).to_owned(), true)
            }
            Err(why) => {
                println!("      locked     could not: {why}");
                (why.clone(), false)
            }
        };
        if let Err(e) = log.append(&Entry::now(&device, Event::Locked { how, worked })) {
            tracing::warn!(error = %e, "could not record a lock request");
        }
        // Closed with a reason either way: a controller that asked for this
        // and heard nothing cannot tell a locked screen from a lost link.
        media.close(if worked {
            esper_proto::Reason::SESSION_COMPLETE
        } else {
            esper_proto::Reason::NO_SCREEN_LOCKER
        });
        return match locked {
            Ok(program) => Ok((format!("locked the screen with {program}"), parked)),
            Err(why) => Err((format!("could not lock the screen: {why}"), parked)),
        };
    }

    // **Log off, or restart, and end.** The same shape as a lock: no capture,
    // no encoder, no input. Their own kinds and their own permissions because
    // they are different amounts of damage — one closes what somebody is
    // working on, the other takes the machine away and every other session on
    // it with it.
    if let Some(action) = match kind {
        esper_proto::SessionKind::Logoff => Some(esper_presence::power::Action::Logoff),
        esper_proto::SessionKind::Reboot => Some(esper_presence::power::Action::Reboot),
        _ => None,
    } {
        // **The trail is written before the action, which for a restart is
        // the only chance.** The machine is gone a moment later, and a line
        // written afterwards would be a line nobody wrote. So the attempt is
        // recorded, then made, then the outcome is recorded if there is still
        // a machine to record it on.
        if let Err(e) = log.append(&Entry::now(
            &device,
            Event::Powered {
                what: action.name().to_owned(),
                how: "asked for".to_owned(),
                worked: false,
            },
        )) {
            tracing::warn!(error = %e, "could not record a power request");
        }
        let done = esper_presence::power::ask_for(action);
        let (how, worked) = match &done {
            Ok(program) => {
                println!("      {}     with {program}", action.name());
                ((*program).to_owned(), true)
            }
            Err(why) => {
                println!("      {}     could not: {why}", action.name());
                (why.clone(), false)
            }
        };
        if let Err(e) = log.append(&Entry::now(
            &device,
            Event::Powered {
                what: action.name().to_owned(),
                how,
                worked,
            },
        )) {
            tracing::warn!(error = %e, "could not record a power request");
        }
        // **Closed before the machine goes.** A restart takes this process
        // with it, so the close frame is the last thing that can reach the
        // far end — and a controller that heard nothing cannot tell a machine
        // that restarted from a link that broke.
        media.close(if worked {
            esper_proto::Reason::SESSION_COMPLETE
        } else {
            esper_proto::Reason::NO_SCREEN_LOCKER
        });
        return match done {
            Ok(program) => Ok((format!("{} with {program}", action.name()), parked)),
            Err(why) => Err((format!("could not {}: {why}", action.name()), parked)),
        };
    }

    if kind == esper_proto::SessionKind::Files {
        if !crate::settings::files_enabled() {
            println!("      files      off — set files.enabled to serve them");
            media.close(esper_proto::Reason::PERMISSION_DENIED);
            return Ok(("file transfer is not switched on here".to_owned(), parked));
        }
        let files = match crate::paths::files_root().and_then(|root| {
            crate::files_task::Files::open(
                &root,
                crate::settings::file_limits(),
                &granted,
                Some(log.clone()),
                device,
                "      ",
            )
        }) {
            Ok(files) => std::sync::Arc::new(files),
            Err(why) => {
                println!("      files      unavailable ({why})");
                media.close(esper_proto::Reason::PERMISSION_DENIED);
                return Ok((format!("no file directory: {why}"), parked));
            }
        };
        println!("      files      {}", files.described());

        // **In the background, like a forward.** A ten gigabyte copy would
        // otherwise hold this loop — and with it the screen, the shell and
        // every other device — for as long as it takes. Nothing in it touches
        // the capture backend or a parked session, so it is one of the two
        // kinds that can simply be let go of.
        let closer = media.clone();
        let from = session.remote_address();
        let live = live.clone();
        let log = log.clone();
        tokio::spawn(async move {
            // Held for the life of the session: dropping it would reset a
            // stream the peer has no reason to think is finished.
            let _agreed = control;
            let watchdog = tokio::spawn(close_when_revoked(
                closer.clone(),
                live,
                // Either file permission going means the session goes; which
                // one it was is in the log line the watchdog writes.
                Permission::FilesOut,
                log,
                device,
            ));
            let accepting = session.control_acceptor();
            let mut children = Children(Vec::new());
            loop {
                let Ok(mut stream) = accepting.accept().await else {
                    break;
                };
                match tokio::time::timeout(
                    SAY_WHAT_YOU_ARE,
                    stream.recv::<esper_proto::StreamPurpose>(),
                )
                .await
                {
                    Ok(Ok(esper_proto::StreamPurpose::Files)) => {
                        children.0.push(tokio::spawn(crate::files_task::serve(
                            stream,
                            std::sync::Arc::clone(&files),
                        )));
                    }
                    Ok(Ok(other)) => eprintln!(
                        "      · a {} stream on a file session, which carries files only",
                        other.name()
                    ),
                    Ok(Err(_)) | Err(_) => continue,
                }
            }
            watchdog.abort();
            println!("  · {from} finished moving files");
            closer.linger(GOODBYE).await;
            drop((recorded, spent));
        });
        return Ok(("moving files, in the background".to_owned(), parked));
    }

    // **A terminal session is not a screen session.** No capture, no encoder,
    // no presenter — which is the whole claim of terminal mode, and it would
    // not be true if the host set a screen up first and answered the shell
    // request on the side.
    if kind == esper_proto::SessionKind::Terminal {
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
            log.clone(),
            device,
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

        // The exit status is stream data and the peer is waiting for it —
        // covered by the `linger` in `serve_one`, which every ending here
        // goes through.
        return match outcome {
            Ok(f) => Ok((f.what, parked)),
            Err(e) => Err((e, parked)),
        };
    }
    // A returning controller resumes what it left; anybody else gets a new
    // session and the parked one is not disturbed by their arrival.
    let (mut link, parked) = match parked {
        Some(p) if p.may_resume(&device) => {
            let (w, h) = p.encode_size();
            let waited = p.parked_for(Instant::now());
            match HostLink::resume(&media, control, p, &device, machine_seen) {
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
        other => match fresh(&media, control, o, machine_seen) {
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

    // Where the captured screens are on the desktop, so a pointer position can
    // be expressed as a fraction of the one it is on. Taken from the config
    // that was just announced, so both ends are working from the same list.

    // **Whether this device may drive the machine comes from the grant**, and
    // the grant came from the stored record clamped by the transport — never
    // from anything the peer said about itself.
    let may_control = granted.contains(&Permission::Control);

    // **The monitor table the injector maps pointer positions through, as it
    // changes.**
    //
    // A `watch` because only the latest layout matters and an input stream
    // opened later must get the current one rather than a queue of old ones.
    //
    // The outgoing pointer path has done this since multi-monitor shipped --
    // "a switch would leave every pointer position expressed as a fraction of
    // the screen that is no longer on show" -- and the incoming path had the
    // same problem with nothing to fix it. `Dispatcher::set_monitors` was
    // written for exactly this, with the comment "after the host's screens
    // changed", and had no caller.
    let (layout, layout_now) = tokio::sync::watch::channel(monitors_from(&config));

    // **What the panic key stops, held where the panic can reach it.** One
    // switch per session: the dispatcher a session opens with and one somebody
    // allows halfway through are the same session's keyboard, and a press has
    // to stop whichever of them exists.
    let stop_injecting = esper_input::Stopped::new();
    // Set when the grant said control and the machine cannot deliver it.
    let mut cannot_inject = false;
    let input = if may_control {
        let chosen = crate::injector::open();
        for line in &chosen.attempts {
            println!("      {line}");
        }
        if chosen.real {
            Some(esper_input::Dispatcher::new(
                chosen.sink,
                layout_now.borrow().clone(),
                true,
                stop_injecting.clone(),
            ))
        } else {
            // **Said out loud, and said to the end that needs to hear it.**
            //
            // A host that took keystrokes and dropped them would look like a
            // working session where nothing happens, which is the hardest
            // fault to diagnose from the other end -- and the other end is
            // exactly where it has to be diagnosed, because that is where
            // somebody is typing. Printing it here told the wrong person.
            //
            // So the session is narrowed to what it can actually do. It is not
            // a revocation: the grant stands and the next session gets it
            // again, on a host where the display may well have come back.
            // Checked per session rather than once at startup for that reason
            // -- an availability read at startup is a snapshot, which is the
            // shape of several bugs already in this tree.
            println!("      view only: nothing on this machine can inject input");
            cannot_inject = true;
            None
        }
    } else {
        // "not granted" rather than "not held": a controller that did not ask
        // for control and one that asked and was refused both land here, and
        // only the grant is a fact this side knows.
        println!("      view only: control is not granted for this session");
        None
    };
    // **One slot, two readers.** The stream that arrives takes it; the
    // session's own loop fills it if somebody here allows control while the
    // session is running.
    let input = Arc::new(std::sync::Mutex::new(input));

    // **The clipboard, if this device was granted one.** Opened before the
    // stream that carries it, so a machine with no clipboard says so here
    // rather than at the first copy — a session silently without one looks,
    // from the far end, exactly like a clipboard that is empty.
    let clipboard_grants = esper_clipboard::Grants::from_permissions(&granted);
    let clipboard = if clipboard_grants.read || clipboard_grants.write {
        match crate::clipboard_task::open(clipboard_grants, "      ") {
            Ok(attached) => {
                println!(
                    "      clipboard  {} — offered, never pushed",
                    attached.backend
                );
                Some(attached)
            }
            Err(why) => {
                println!("      clipboard  {why}");
                None
            }
        }
    } else {
        // **Said with the way out of it.** A permission that exists, is
        // checked, and is never mentioned is the same as a feature that does
        // not work — the person at the host has no reason to guess that a
        // clipboard is a thing they could grant.
        println!(
            "      clipboard  not granted — {} grant {} clipboard-out clipboard-in",
            crate::net::me(),
            session.peer_fingerprint().hex()
        );
        None
    };

    // **Files, if this device was granted them and this machine serves them.**
    // Said out loud either way, and with the way out of it: a permission that
    // exists, is checked, and is never mentioned is the same as a feature that
    // does not work.
    let files = if !crate::settings::files_enabled() {
        // **Off, and said.** A device holding the permission and finding
        // nothing there would look like a broken host rather than a
        // deliberately closed door.
        println!("      files      off — set files.enabled to serve them");
        None
    } else if !granted.contains(&Permission::FilesOut) && !granted.contains(&Permission::FilesIn) {
        println!(
            "      files      not granted — {} grant {} files-out files-in",
            crate::net::me(),
            session.peer_fingerprint().hex()
        );
        None
    } else {
        match crate::paths::files_root().and_then(|root| {
            crate::files_task::Files::open(
                &root,
                crate::settings::file_limits(),
                &granted,
                Some(log.clone()),
                device,
                "      ",
            )
        }) {
            Ok(files) => {
                println!("      files      {}", files.described());
                Some(std::sync::Arc::new(files))
            }
            // A file directory that will not open is not a reason to refuse a
            // screen. Said, and the session carries on without it.
            Err(why) => {
                println!("      files      unavailable ({why})");
                None
            }
        }
    };

    // Noted before the three are handed to the task that serves them, because
    // what the controller has to be told is *whether* each arrived, not the
    // thing itself.
    let clipboard_missing = clipboard.is_none();
    let files_missing = files.is_none();

    // **Always, even with nothing to serve yet.** It used to be spawned only
    // when the session opened with something a stream could carry, which was
    // right until a session could be widened while it ran: a controller
    // allowed control at half past three opens an input stream, and nothing
    // would have been listening for it.
    //
    // Accepted in a task because the controller opens these lazily — a session
    // that sends no input never opens one — and waiting here would hold up the
    // video.
    let waiting_for_streams = {
        let accepting = session.control_acceptor();
        let stopping = stopping.clone();
        let live = live.clone();
        tokio::spawn(serve_extra_streams(ExtraStreams {
            accepting,
            input: Arc::clone(&input),
            layout: layout_now,
            clipboard,
            files,
            stopping,
            live,
        }))
    };

    // **The indicator goes up before a single frame goes out.** A person
    // whose screen starts being watched has to be told at the moment it
    // starts, not once the far end has a picture.
    if let Some(indicator) = indicator {
        indicator.show(esper_presence::Watching {
            peer: session.peer_hello().name.clone(),
            controlling: may_control,
            recording: false,
            paused: false,
        });

        // **And the screen is asked whether it is really there, before a
        // picture leaves.**
        //
        // Asking for the frame and having it are different facts, and until
        // this the host only knew the first: a window that never mapped, one
        // destroyed by a compositor restarting, or a program drawn over the
        // top all looked exactly like success. So the session waits for the
        // screen's own answer, and does not start without it.
        match crate::presence_task::confirmed(indicator, INDICATOR_PATIENCE).await {
            Ok(looks) => {
                println!("      indicator  on screen, confirmed by reading it ({looks} look(s))");
            }
            Err(why) => {
                println!("      indicator  NOT on screen: {why}");
                println!("                 No picture is sent when this machine cannot show that");
                println!("                 it is being watched.");
                if let Err(w) = log.append(&Entry::now(
                    &device,
                    Event::Refused {
                        reason: format!("the session indicator could not be confirmed: {why}"),
                    },
                )) {
                    eprintln!("      · could not write the audit log: {w}");
                }
                media.close(esper_proto::Reason::INDICATOR_UNCONFIRMED);
                return Ok((
                    format!("the session indicator could not be confirmed: {why}"),
                    parked,
                ));
            }
        }
    }

    // **The privacy screen, if this machine's owner asked for one.**
    //
    // The order is the whole safety story and it is not negotiable: ask what
    // blanking would take, start something that will outlive this process,
    // give it the undo, and only then turn anything off. Any other order
    // leaves a window where a crash leaves somebody's monitor dark with
    // nobody holding the way back.
    let mut privacy: Option<crate::presence_task::Privacy> = None;
    if crate::settings::flag("host.privacy_screen", false) {
        match indicator {
            Some(indicator) => match crate::presence_task::blank_for_the_session(indicator) {
                Ok(held) => {
                    println!(
                        "      privacy    {} dark for this session, and a keeper is holding the \
                         way back",
                        held.what
                    );
                    privacy = Some(held);
                }
                Err(why) => {
                    // **Not fatal.** A session that refused to start because
                    // the host could not go dark would be a worse product than
                    // one that says so: the far end still gets its picture,
                    // and the person who set this can see the line.
                    println!("      privacy    NOT blanked: {why}");
                }
            },
            None => println!("      privacy    nothing on this machine can blank a screen"),
        }
    }

    // **And the cheapest of the three, if this machine's owner asked for it.**
    //
    // Not a privacy measure at all: a plain background is bandwidth. A
    // desktop is windows over a background, the windows are what the far end
    // came for, and a photograph behind them is 63% of a keyframe at
    // 1280x720 with a tenth of the screen covered — measured in
    // `esper-encode/tests/background_cost.rs`. It lands on the *first*
    // picture and on every keyframe after a change of size, because capture
    // is damage-driven and a still background is sent once.
    let mut backdrop: Option<crate::presence_task::PlainBackdrop> = None;
    if crate::settings::flag("host.plain_background", false) {
        match crate::presence_task::plain_backdrop() {
            Ok(plain) => {
                if plain.moved == 0 {
                    // **Nothing changed, so nothing is kept.** Either the
                    // background was plain already or something covers every
                    // place looked at — and a wallpaper hidden for no
                    // measurable gain is a change nobody asked for. Dropping
                    // it here puts it back and lets the keeper go.
                    println!(
                        "      backdrop   left alone: making it plain changed nothing that \
                         could be seen"
                    );
                    drop(plain);
                } else {
                    println!(
                        "      backdrop   {} is plain for this session ({} of {} places \
                         changed), and a keeper is holding the way back",
                        plain.what, plain.moved, plain.of
                    );
                    backdrop = Some(plain);
                }
            }
            // **Not fatal**, for the reason the privacy screen is not: a
            // session refused because a wallpaper could not be hidden would
            // be a worse product than one that says so.
            Err(why) => println!("      backdrop   left alone: {why}"),
        }
    }

    // **The other half of it, if this machine's owner asked for that too.**
    // Same order and the same reason: nothing is detached until somebody
    // other than this process can put it back. A dark monitor still leaves
    // whoever is there able to type blind; a floating keyboard leaves them
    // with no way to say anything at all, so this one is the more careful of
    // the two.
    let mut local_input: Option<crate::presence_task::LocalInput> = None;
    if crate::settings::flag("host.lock_local_input", false) {
        match crate::presence_task::lock_local_input() {
            Ok(held) => {
                println!(
                    "      local      {} out of the picture for this session, and a keeper is \
                     holding the way back",
                    held.what
                );
                // **What it costs, said every time.** The panic key is a key
                // combination and a keyboard with no master sends its keys
                // nowhere, so the one escape this machine has while a session
                // runs is gone for as long as the session does. Somebody who
                // set this asked for exactly that; somebody who inherited the
                // setting from a policy did not, and this line is the only
                // place they will find out.
                println!(
                    "      local      the panic key goes with it: this session ends from the \
                     far end, on its own clock, or when this host stops"
                );
                local_input = Some(held);
            }
            // **Not fatal**, for the reason the privacy screen is not: a
            // session refused because the host could not silence its own
            // keyboard is a worse product than one that says so.
            Err(why) => println!("      local      input NOT taken out of the picture: {why}"),
        }
    }

    // **The pointer, read on its own schedule.** A cursor sampled at the frame
    // rate is a cursor sampled at five hertz on a quiet desktop, which is why
    // this is not part of the sending loop.
    //
    // A host with no cursor source — a synthetic screen has no pointer — says
    // so once and sends nothing, rather than inventing a pointer in the middle
    // of the picture.
    let (watch, pointer) = match crate::cursor_task::start(served_geometry(&config)) {
        Ok((watch, messages)) => {
            println!("      pointer    {} — sent out of band", watch.backend());
            (Some(watch), Some(messages))
        }
        Err(e) => {
            println!("      pointer    not sent ({e})");
            (None, None)
        }
    };

    // **What this session can actually deliver, which is not always what it
    // was granted — and the far end has to be told which.**
    //
    // Three capabilities can be granted and then turn out to be missing: a
    // machine with nothing to inject input through, one with no clipboard, one
    // serving no files. Each of those already said so, carefully, on the
    // *host's* own output — and each comment explaining why says the fault is
    // hardest to diagnose from the other end, which is exactly the end that
    // was not told. A controller offering a cursor that does nothing, or a
    // paste that goes nowhere, looks like a broken session rather than a
    // machine that cannot do it.
    //
    // So the session is narrowed to what it can do, in one message carrying
    // the complete set. It is **not** a revocation: the grant stands, the
    // device keeps it, and the next session gets it again on a machine where
    // the display may well have come back.
    // **The sound, if this session may have it and this machine can produce
    // it.** Started before the narrowing below, because whether it started is
    // one of the things the narrowing has to say.
    //
    // A monitor of the output, never an input: see `esper-audio-pulse`. The
    // permission is what a person granted; the device is what the machine has.
    let sound = if granted.contains(&Permission::Audio) {
        match crate::audio_task::start(&media) {
            Ok(sending) => {
                println!(
                    "      sending sound: {} at {} Hz, {} channel(s), {} ms frames, ssrc {:#010x}",
                    sending.backend(),
                    sending.config().sample_rate_hz,
                    sending.config().channels,
                    sending.config().frame_ms,
                    sending.config().ssrc
                );
                // Announced on the control stream, because a controller cannot
                // build a decoder for a stream it was not told about — and the
                // packets are already on their way.
                if let Err(e) = link.announce_sound(sending.config()).await {
                    println!("      · could not announce the sound ({e})");
                }
                Some(sending)
            }
            Err(e) => {
                // Said once, and the session carries on. A host with no sound
                // server is the ordinary case on a server.
                println!("      no sound: {e}");
                None
            }
        }
    } else {
        None
    };

    let mut deliverable = granted.clone();
    if sound.is_none() {
        deliverable.retain(|p| *p != Permission::Audio);
    }
    if cannot_inject {
        deliverable.retain(|p| *p != Permission::Control);
    }
    if clipboard_missing {
        deliverable.retain(|p| !matches!(p, Permission::ClipboardOut | Permission::ClipboardIn));
    }
    if files_missing {
        deliverable.retain(|p| !matches!(p, Permission::FilesOut | Permission::FilesIn));
    }
    if deliverable != granted {
        let lost: Vec<&str> = granted
            .iter()
            .filter(|p| !deliverable.contains(p))
            .map(|p| p.name())
            .collect();
        println!(
            "      telling the controller this session cannot do: {}",
            lost.join(", ")
        );
        if let Err(e) = link.narrowed_to(deliverable).await {
            // Not fatal to the session. A controller that was not told offers
            // something that does not work, which is worse than this line and
            // better than no session at all.
            println!("      · could not say so ({e})");
        }
    }

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
            stop_injecting: stop_injecting.clone(),
            input: Arc::clone(&input),
            watching: esper_presence::Watching {
                recording: false,
                peer: session.peer_hello().name.clone(),
                controlling: may_control,
                paused: false,
            },
            layout,
            log,
            capacity,
            policy,
            recorded: &recorded,
        },
    )
    .await;
    // **Dropped here, with the session.** The thread stops, the capture
    // subprocess is killed and the sound stops — a session that ended and left
    // a capture running would be recording somebody's machine for nothing.
    drop(sound);
    drop(watch);
    // **The screens come back before the indicator goes down**, so a person
    // watching their own machine sees it light up rather than sitting dark for
    // a moment with nothing on it saying why.
    if let Some(held) = privacy.take() {
        held.give_back();
    }
    // **And the keyboard comes back with them.** Dropped rather than asked,
    // because the `Drop` is the thing that must happen on every path out of
    // this function — including the ones that return early — and a `give_back`
    // that had to be remembered would eventually not be.
    drop(local_input.take());
    // And the wallpaper, for the same reason and by the same means.
    drop(backdrop.take());
    if let Some(indicator) = indicator {
        indicator.hide();
    }
    // Aborted rather than awaited: it is waiting on a stream that will never
    // arrive now. The `Drop` of what it holds is what releases a key still
    // down and wipes what the clipboard was carrying.
    waiting_for_streams.abort();
    served
}

/// Where each screen the session is sending sits on the host's desktop.
///
/// A pointer position is a fraction of the screen it is *on*, so the pointer
/// reader needs all of them: with several screens in one picture, a reader
/// given only the first reports the pointer as gone half way across a picture
/// it is plainly still in.
fn served_geometry(config: &esper_proto::StreamConfig) -> Vec<(u32, i32, i32, u32, u32)> {
    config
        .serving
        .iter()
        .filter_map(|id| config.monitors.iter().find(|m| m.id == *id))
        .map(|m| (m.id, m.x, m.y, m.width, m.height))
        .collect()
}

/// Open a capture backend and build a sender around it.
fn fresh(
    media: &esper_transport::MediaChannel,
    control: esper_transport::Control,
    o: &HostOptions,
    seen: esper_proto::Seen,
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

    HostLink::new(media, control, pipeline, seen).map_err(|e| e.to_string())
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
/// What a host is prepared to serve on the streams a controller opens.
struct ExtraStreams {
    accepting: esper_transport::ControlAcceptor,
    /// Taken by the first input stream. There is only one keyboard.
    ///
    /// **Shared with the session's own loop, and that is the point.** A
    /// session that opened without `control` has nothing here — opening an
    /// injector for a session that will never type would print two lines and
    /// take a device for nothing — and if somebody at this machine allows
    /// control later, the loop that heard them puts one in.
    input: Arc<std::sync::Mutex<Option<esper_input::Dispatcher>>>,
    /// The host's screens, as they change, for the injector to map through.
    layout: tokio::sync::watch::Receiver<Vec<esper_input::Monitor>>,
    /// Taken by the first clipboard stream. There is only one clipboard.
    clipboard: Option<crate::clipboard_task::Attached>,
    /// **Shared, not taken.** A file stream is one file, so a session has as
    /// many of them as it has files in flight — and how many that may be is
    /// the service's own decision rather than this loop's.
    files: Option<std::sync::Arc<crate::files_task::Files>>,
    stopping: tokio::sync::watch::Receiver<bool>,
    live: crate::permits::Live,
}

/// Take the injector out of the shared slot, if there is one there.
///
/// A poisoned lock is nothing to inject with: a panic somewhere holding this
/// is not a reason to hand a keyboard to a peer.
fn take_the_keyboard(
    slot: &std::sync::Mutex<Option<esper_input::Dispatcher>>,
) -> Option<esper_input::Dispatcher> {
    slot.lock().ok()?.take()
}

/// Tasks that must not outlive whatever spawned them.
struct Children(Vec<tokio::task::JoinHandle<()>>);

impl Drop for Children {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

/// How long a stream has to say what it is.
///
/// A peer that opens one and then says nothing must not park this loop: the
/// next stream would wait behind it, and a session would lose its clipboard
/// because something opened a stream and stopped.
const SAY_WHAT_YOU_ARE: Duration = Duration::from_secs(10);

/// Accept the streams a controller opens, and give each to what serves it.
///
/// **Every stream says what it is before it says anything else.** There used
/// to be one kind, so the one that arrived was the one it had to be — which is
/// an assumption rather than a fact the moment there are two, and a clipboard
/// stream mistaken for an input stream would be a session where neither works
/// and nothing says why.
async fn serve_extra_streams(mut extras: ExtraStreams) {
    // **What is spawned here dies with this.** The session ends by aborting
    // this task, and a spawned child is not aborted by its parent being. The
    // input dispatcher's `Drop` is what releases a key still held down on
    // somebody else's machine, and the clipboard's is what wipes what it was
    // carrying — neither is a thing to leave until a stream happens to error.
    let mut children = Children(Vec::new());
    // Whether each kind has been served, so a stream arriving with nothing to
    // serve it is told which of the two things happened.
    let mut served_input = false;
    let mut served_clipboard = false;
    loop {
        let Ok(mut control) = extras.accepting.accept().await else {
            return;
        };
        let said = tokio::time::timeout(
            SAY_WHAT_YOU_ARE,
            control.recv::<esper_proto::StreamPurpose>(),
        )
        .await;
        let purpose = match said {
            Ok(Ok(purpose)) => purpose,
            Ok(Err(e)) => {
                tracing::debug!(error = %e, "a stream did not say what it was");
                continue;
            }
            Err(_) => {
                eprintln!("      · a stream was opened and never said what it was for");
                continue;
            }
        };
        // **Checked against the grant as it is now, not as the session
        // opened.** A stream is not permission to do the thing it names — and
        // the set moves in both directions while a session runs: a permission
        // taken away at the machine must not admit a stream a moment later,
        // and one a person has just allowed for this session must.
        let granted = esper_transport::Grants::granted(&extras.live);
        if !purpose.permitted_by(&granted) {
            eprintln!(
                "      · refused a {} stream: not granted for this session",
                purpose.name()
            );
            continue;
        }
        match purpose {
            esper_proto::StreamPurpose::Input => match take_the_keyboard(&extras.input) {
                Some(dispatcher) => {
                    served_input = true;
                    let stopping = extras.stopping.clone();
                    let live = extras.live.clone();
                    let layout = extras.layout.clone();
                    children.0.push(tokio::spawn(serve_input(
                        control, dispatcher, stopping, live, layout,
                    )));
                }
                // **Two different refusals, said differently.** One keyboard
                // means a second stream is a peer confused about what it
                // already has; nothing to inject with means the far end is
                // waiting on something this machine already said it does not
                // have.
                None if served_input => eprintln!("      · refused a second input stream"),
                None => {
                    eprintln!("      · an input stream, with nothing on this machine to inject");
                }
            },
            esper_proto::StreamPurpose::Clipboard => match extras.clipboard.take() {
                Some(attached) => {
                    served_clipboard = true;
                    let stopping = extras.stopping.clone();
                    children.0.push(tokio::spawn(crate::clipboard_task::serve(
                        control, attached, stopping,
                    )));
                }
                None if served_clipboard => {
                    eprintln!("      · refused a second clipboard stream");
                }
                None => {
                    eprintln!("      · a clipboard stream, with no clipboard on this machine");
                }
            },
            esper_proto::StreamPurpose::Files => match extras.files.clone() {
                Some(files) => {
                    children
                        .0
                        .push(tokio::spawn(crate::files_task::serve(control, files)));
                }
                None => eprintln!("      · a file stream, with no file directory on this machine"),
            },
        }
    }
}

async fn serve_input(
    session_input: esper_transport::Control,
    mut dispatcher: esper_input::Dispatcher,
    mut stopping: tokio::sync::watch::Receiver<bool>,
    mut live: crate::permits::Live,
    mut layout: tokio::sync::watch::Receiver<Vec<esper_input::Monitor>>,
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
                // **The screens moved, so the table this maps through has to.**
                // A pointer position is a fraction of a monitor, so a stale
                // table puts a click at the right fraction of the wrong
                // screen -- or names a monitor that is no longer there, which
                // is refused and counted and looks to the person clicking
                // like a session that stopped taking input.
                Ok(()) = layout.changed() => {
                    dispatcher.set_monitors(layout.borrow_and_update().clone());
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
                Ok(()) = layout.changed() => {
                    dispatcher.set_monitors(layout.borrow_and_update().clone());
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
///
/// **Only the screens being sent.** A position naming a screen this session
/// cannot see is refused by the injector rather than acted on: a controller
/// cannot produce one from a picture it does not have, so one arriving is
/// either a stale message from before a switch — which would land at the right
/// fraction of the wrong screen — or a peer clicking somewhere it cannot see.
/// The picture and the input surface are the same set, and that is a rule
/// worth being able to state.
fn monitors_from(config: &esper_proto::StreamConfig) -> Vec<esper_input::Monitor> {
    config
        .monitors
        .iter()
        .filter(|m| config.serving.contains(&m.id))
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
    /// Who to ask before a session opens, and what it remembers.
    ///
    /// One for the host rather than one per session: it owns the terminal
    /// reader, and its record of who has just been accepted is what stops a
    /// reconnect asking the same question twice.
    policy: &'a Arc<crate::consent::AskAtTheHost>,
    /// Where to put what the controller measures the link at, for this
    /// connection's own congestion controller to pace from.
    ///
    /// `None` for a connection this endpoint did not accept — which does not
    /// happen for a host, and is not something to fail a session over.
    capacity: Option<esper_transport::Capacity>,
    /// What somebody at this machine would see of a session, for the
    /// controller to be told.
    seen: esper_proto::Seen,
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
    /// What stops injection, for the panic key to throw.
    ///
    /// **Held here because the press arrives here.** The dispatcher itself
    /// lives in the task that injects, and by the time a press is read there
    /// is nothing in this loop holding it — so what the loop holds is the
    /// switch.
    stop_injecting: esper_input::Stopped,
    /// Where an injector goes if somebody here allows control mid-session.
    ///
    /// The other end of [`ExtraStreams::input`]. Empty for a session that
    /// opened with control — the stream took it — and empty for one that
    /// cannot inject at all.
    input: Arc<std::sync::Mutex<Option<esper_input::Dispatcher>>>,
    /// What the session indicator is saying, so pausing can change one field
    /// of it rather than rebuilding what the rest should be.
    watching: esper_presence::Watching,
    /// The host's screens, published whenever they change, so the injector
    /// maps pointer positions through the layout that is on show.
    layout: tokio::sync::watch::Sender<Vec<esper_input::Monitor>>,
    /// What happened, for somebody reading it afterwards.
    log: &'a Log,
    /// Where what the controller measures the link at goes, for this
    /// connection's own congestion controller to pace from.
    capacity: Option<esper_transport::Capacity>,
    /// The host's policy, for the one question it answers *during* a session:
    /// whether the screen has been locked since the device walked in.
    policy: &'a Arc<crate::consent::AskAtTheHost>,
    /// The trail's record of this session, so whatever ends it can say why.
    recorded: &'a Recorded,
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
    // **Whether the picture is held, read by the thread that sends it.**
    //
    // A `watch` because only the latest answer matters: a queue of pauses and
    // resumes would be a queue the sending thread worked through one frame at
    // a time, so a press and an unpress in quick succession would hold the
    // picture for as long as it took to drain rather than not at all.
    let (hold, held) = tokio::sync::watch::channel(false);
    let (to_pipeline, mut from_control) = tokio::sync::mpsc::unbounded_channel::<MediaMessage>();
    let (to_controller, mut from_pipeline) = tokio::sync::mpsc::unbounded_channel::<MediaMessage>();
    let (mut pump, mut control) = link.split();

    let sending = std::thread::spawn(move || {
        let interval = Duration::from_millis(200);
        let mut frames = 0u64;
        // **What the controller was last told**, whole rather than just its
        // size. Two overlapping conditions used to decide whether to send a
        // config — the picture's size moved, or the desktop changed — and a
        // monitor switch satisfied both, so it went out twice. One rule: send
        // it when it is different from what was sent.
        //
        // Read from the pump rather than passed in, so there is one source
        // for it.
        let mut announced: Option<esper_proto::StreamConfig> = Some(pump.config());
        // **What the viewer said it was doing, said once when it says it.**
        //
        // The one thing this side cannot work out for itself, and worth
        // telling whoever owns the machine: a session that is deliberately
        // soft because somebody is watching video looks, in every other line
        // of output, exactly like a session that is soft because this machine
        // is struggling.
        let mut said = pump.profile();
        loop {
            // **Held: nothing captured, nothing encoded, nothing sent.**
            //
            // Not a frame dropped on the way out — the capture never happens,
            // so a paused session costs the host nothing, which is half the
            // reason to pause rather than end. The controller was told, so its
            // last picture staying on screen is a picture it can explain.
            //
            // Messages are still read below: a keyframe request arriving
            // during a pause is answered by the first frame after it, and a
            // controller whose request was silently dropped would wait a pace
            // and ask again for as long as the pause lasted.
            if *held.borrow() {
                std::thread::sleep(HELD_TICK);
                while let Ok(message) = from_control.try_recv() {
                    if let Err(e) = pump.handle(&message) {
                        eprintln!("      · refused a message from the controller: {e}");
                    }
                }
                if !pump.is_open() {
                    break;
                }
                continue;
            }
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
                            "      now sending {}: {}x{}, ssrc {:#010x}",
                            esper_stream::screens_named(&config.serving),
                            config.width,
                            config.height,
                            config.ssrc
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
                if let Some(config) = answer {
                    // Recorded as well as sent, so the reconfiguration this
                    // switch is about to cause does not announce the same
                    // thing again a moment later.
                    announced = Some(config.clone());
                    if to_controller.send(MediaMessage::Config(config)).is_err() {
                        break;
                    }
                }
            }
            match pump.pump(interval) {
                Ok(HostStep::Sent { .. }) => frames = frames.saturating_add(1),
                Ok(HostStep::Finished) => break,
                // **The desktop changed, and the controller has to be told —
                // even when the picture did not.** A monitor plugged in, or a
                // desktop rearranged, leaves the screen being watched exactly
                // as it was: the size check below sees nothing and says
                // nothing, and the controller's monitor list is wrong for the
                // rest of the session. A viewer offering a picker would list
                // screens that are gone and not the one that arrived.
                Ok(HostStep::Reconfigured { switched, .. }) => {
                    let now = pump.config();
                    // **Only when it is news.** A monitor switch already
                    // announced itself from the `Select` handler, and the
                    // reconfiguration it causes arrives here a moment later —
                    // the same config twice, the second reported as "the
                    // desktop changed" when nothing had.
                    if announced.as_ref() == Some(&now) {
                        continue;
                    }
                    if switched {
                        // **A switch nobody asked for.** The screen being
                        // watched went away and this machine fell back to
                        // another one. It is the single thing a controller
                        // cannot work out for itself — the picture simply
                        // becomes somewhere else, possibly at the same size —
                        // so it is said here and carried in the config.
                        println!(
                            "      the monitor being watched went away; now sending {}: {}x{}",
                            esper_stream::screens_named(&now.serving),
                            now.width,
                            now.height
                        );
                    } else {
                        println!(
                            "      the desktop changed: {} monitor(s), still sending {}",
                            now.monitors.len(),
                            esper_stream::screens_named(&now.serving)
                        );
                    }
                    announced = Some(now.clone());
                    if to_controller.send(MediaMessage::Config(now)).is_err() {
                        break;
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    eprintln!("      · the pipeline stopped: {e}");
                    break;
                }
            }

            // **What the viewer said it was doing, once, when it says it.**
            //
            // The one thing this side cannot work out for itself — a slideshow
            // and a video look the same to a damage tracker — and worth
            // telling whoever owns the machine, because a session that is
            // deliberately soft because somebody is watching video looks, in
            // every other line of output here, exactly like a session that is
            // soft because this machine is struggling.
            if pump.profile() != said {
                said = pump.profile();
                println!(
                    "      the viewer says it is {}",
                    match said {
                        esper_proto::Profile::Text => "reading: sharpness over motion",
                        esper_proto::Profile::Balanced => "not minding either way",
                        esper_proto::Profile::Motion => "watching motion: a softer picture will do",
                    }
                );
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
            let size_moved = announced.as_ref().is_none_or(|was| {
                (was.width, was.height) != (now_sending.width, now_sending.height)
            });
            if size_moved {
                announced = Some(now_sending.clone());
                // **Why, not just what.** Two different things lower a
                // picture — this machine running out of CPU, and the
                // controller asking for smaller frames because the link is
                // losing them — and an operator told "the CPU budget moved"
                // when it was the network goes looking in the wrong place.
                let why = match pump.asked_ceiling() {
                    Some(edge) if edge >= now_sending.width.max(now_sending.height) => {
                        "the controller asked for smaller frames: it is losing them"
                    }
                    _ => "the CPU budget moved",
                };
                println!(
                    "      now sending {}x{} ({why})",
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
        /// The person at the host hit one of the hotkeys.
        Pressed(esper_presence::HostEvent),
        /// What this device may do changed while the session was running.
        Permissions,
        /// The screen stopped showing the session indicator, or started again.
        Indicator,
        /// The indicator has been off the screen for too long.
        IndicatorGaveUp,
        /// The screen at this host was locked while the session ran.
        Locked(String),
        /// Somebody at this machine answered a question about widening this
        /// session — or the rule answered it without asking anybody.
        Answered(TheyAnswered),
    }
    // Which reason this side ended it with, if it did. A session the host
    // closed is not one to park — parking is for a controller that has not
    // chosen to leave — and the reason is worth naming in the log rather than
    // printing one fixed sentence for two different endings.
    let mut ours: Option<esper_proto::Reason> = None;
    // What the transport has been told, so the same number is not announced
    // every half second.
    let mut told_capacity: Option<u32> = None;
    // What the controller has been told this session may do, so one decision
    // produces one message. `None` until something is taken away: the opening
    // grant was already agreed when the session was set up.
    let mut told_permissions: Option<Vec<Permission>> = None;
    // Whether the picture is held. The session owns this, and the key and the
    // signal are two ways of asking it to change — which is why neither of
    // them carries a state.
    let mut paused = false;
    // **What the far end says it is doing with this session**, so one notice
    // produces one line and one log entry: a controller that repeated itself
    // would otherwise fill the trail with the same fact.
    let mut recording = false;
    // **What the screen says about the indicator, and since when.**
    //
    // Separate from `paused` because they are different reasons for the same
    // effect and both can be true: a person can hold the picture during a
    // compositor restart, and letting go must not start it again while the
    // frame is still missing. The picture flows only when neither says stop.
    let mut sightings = attending
        .indicator
        .map(crate::presence_task::Indicator::sightings);
    let mut hidden_since: Option<std::time::Instant> = None;
    // One per session, dropped with it. `signal` may be called more than once
    // and every stream receives, so this does not have to be threaded down
    // from the host's own start.
    let mut usr1 =
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1()) {
            Ok(s) => Some(s),
            Err(e) => {
                // Said, not swallowed: a host whose pause signal is unavailable
                // still works, and somebody who tried it would otherwise be left
                // wondering which end was broken.
                eprintln!("      · `kill -USR1` will not hold this session: {e}");
                None
            }
        };
    // **The bound on `unattended` applies to a session already running, not
    // only to one arriving.**
    //
    // Without this, a device that walked in at five o'clock keeps watching all
    // night once the operator locks up and leaves — which is the hour the
    // bound was switched on for. Only when the policy is on: a host without it
    // spawns nothing and asks nobody.
    let mut locked = watch_for_a_lock(Arc::clone(attending.policy), stopping.clone());
    // **How often this session has put a question to whoever is here.** Per
    // session rather than per host: the limit exists so one device cannot
    // fill somebody's screen with prompts, and a limit shared between devices
    // would let one of them spend another's.
    let knocking = Arc::new(std::sync::Mutex::new(crate::consent::Knocking::new()));
    // The question that is out, if one is. One at a time: a second dialog
    // over the first would be two questions with one answer between them.
    let mut asking: Option<Asking> = None;
    // How many times this session has asked anything at all.
    let mut asks = 0usize;

    loop {
        let turn = tokio::select! {
            _ = stopping.changed() => Turn::Stop,
            event = wait_for_press(attending.presses.as_deref_mut()) => Turn::Pressed(event),
            // **And the same thing from a signal**, because a host with no
            // display has no hotkey and still has a reason to hold a session:
            // `kill -USR1` is the trigger a script and a machine without a
            // screen both have. One `Turn`, so there is one place that decides
            // what pausing means.
            () = wait_for_usr1(usr1.as_mut()) => {
                Turn::Pressed(esper_presence::HostEvent::Pause)
            }
            () = attending.live.changed() => Turn::Permissions,
            () = wait_for_the_screen(sightings.as_mut()) => Turn::Indicator,
            () = out_of_grace(hidden_since) => Turn::IndicatorGaveUp,
            said = wait_for_a_lock(locked.as_mut()) => Turn::Locked(said),
            answered = wait_for_an_answer(asking.as_mut()) => {
                // Taken out here rather than in the arm: the handle has
                // already produced its answer, and a session that left it in
                // place would refuse the next question for ever.
                asking = None;
                Turn::Answered(answered)
            }
            incoming = control.recv::<MediaMessage>() => Turn::FromController(incoming),
            outgoing = from_pipeline.recv() => Turn::ToController(outgoing),
            cursor = wait_for_cursor(attending.pointer.as_mut()) => Turn::ToController(cursor),
        };
        match turn {
            // **The reason travels.** A controller whose host simply stopped
            // sending cannot tell a shutdown from a network that broke, and
            // there is a code for this one.
            // **A screen that locked mid-session ends it, where somebody asked
            // for that bound.** The same reason code an arriving device gets,
            // because it is the same rule and the same thing fixes it.
            Turn::Locked(how) => {
                println!("      · ending this session: the screen here is {how}");
                attending
                    .recorded
                    .ending(&format!("the screen at this host is {how}"));
                media.close(esper_proto::Reason::HOST_IS_LOCKED);
                ours = Some(esper_proto::Reason::HOST_IS_LOCKED);
                break;
            }
            // **What somebody at this machine said about widening it.**
            //
            // The order is the same discipline as everywhere else here: apply
            // it at the host first, then say so, then tell the far end. A
            // controller told it may drive before the injector would accept a
            // keystroke is a controller whose first keystroke is refused.
            Turn::Answered(answered) => {
                use crate::consent::Widening;
                let TheyAnswered {
                    wanted,
                    verdict: answer,
                    keyboard,
                } = answered;
                let (allowed, how) = match &answer {
                    Widening::Allowed { asked: true } => (true, "somebody here said yes"),
                    Widening::Allowed { asked: false } => {
                        (true, "the device already holds it here")
                    }
                    Widening::AlreadyHeld => (true, "this session already had it"),
                    Widening::Refused(_, _) => (false, "refused"),
                };
                if allowed && !matches!(answer, Widening::AlreadyHeld) {
                    // **The keyboard first, then the permission.** In the
                    // other order there is a moment when the session may
                    // drive and the stream that carries the driving would be
                    // refused for having nothing to inject with.
                    if let Some(dispatcher) = keyboard
                        && let Ok(mut slot) = attending.input.lock()
                    {
                        *slot = Some(dispatcher);
                    }
                    attending.live.allow_here(wanted);
                }
                if let Err(e) = attending.log.append(&Entry::now(
                    &device,
                    Event::SessionAsked {
                        wanted,
                        allowed,
                        how: match &answer {
                            Widening::Refused(_, detail) => detail.clone(),
                            _ => how.to_owned(),
                        },
                    },
                )) {
                    eprintln!("      · could not write the audit log: {e}");
                }
                match &answer {
                    Widening::Refused(reason, detail) => {
                        println!("      · not allowed {}: {detail}", wanted.name());
                        if control
                            .send(&MediaMessage::Answer(
                                esper_proto::PermissionAnswer::Refused {
                                    reason: *reason,
                                    detail: detail.clone(),
                                },
                            ))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Widening::AlreadyHeld | Widening::Allowed { .. } => {
                        if matches!(answer, Widening::AlreadyHeld) {
                            // Answered rather than ignored: a controller that
                            // lost track of what it holds is still owed a
                            // reply, and the set below is what puts it right.
                            println!(
                                "      · the controller asked for {} and already had it",
                                wanted.name()
                            );
                        } else {
                            println!(
                                "      · this session may now {} — {how}, for this session only",
                                wanted.name()
                            );
                        }
                        // **The indicator, because the border is what somebody
                        // at this machine reads.** A session that has just
                        // gained control has to look like one, and nothing
                        // else would change the colour: the store did not
                        // move, so the branch that watches it says nothing.
                        if wanted == Permission::Control {
                            attending.watching.controlling = true;
                            if let Some(i) = attending.indicator {
                                i.show(esper_presence::Watching {
                                    recording,
                                    paused,
                                    ..attending.watching.clone()
                                });
                            }
                        }
                        // **The set, then the answer.** `Permissions` is the
                        // one statement of what a session may do, so it goes
                        // first and the answer is the acknowledgement — a
                        // controller that saw "allowed" and no set would have
                        // to guess what it now holds.
                        let granted = esper_transport::Grants::granted(&attending.live);
                        told_permissions = Some(granted.clone());
                        if control
                            .send(&MediaMessage::Permissions(esper_proto::PermissionChange {
                                granted,
                            }))
                            .await
                            .is_err()
                        {
                            break;
                        }
                        if control
                            .send(&MediaMessage::Answer(
                                esper_proto::PermissionAnswer::Allowed,
                            ))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                }
                continue;
            }
            Turn::Stop => {
                attending.recorded.ending("the host was shutting down");
                media.close(esper_proto::Reason::HOST_SHUTTING_DOWN);
                ours = Some(esper_proto::Reason::HOST_SHUTTING_DOWN);
                break;
            }
            // **Its own reason.** A controller told "the host is shutting
            // down" would wait for it to come back, and told "you were
            // disconnected" would reconnect. Neither happened: a person
            // standing at the machine decided they had seen enough.
            Turn::Pressed(esper_presence::HostEvent::Pause) => {
                paused = !paused;
                // **The thread first, then the far end.** Holding the picture
                // is the thing being asked for; telling the controller is how
                // it is explained. In the other order a frame captured between
                // the two would arrive after the pause was announced.
                let _ = hold.send(picture_is_held(paused, hidden_since.is_some()));
                if let Some(i) = attending.indicator {
                    i.show(esper_presence::Watching {
                        paused,
                        ..attending.watching.clone()
                    });
                }
                println!(
                    "      · {} the picture at the host",
                    if paused { "holding" } else { "released" }
                );
                if control
                    .send(&MediaMessage::Paused(esper_proto::Paused { paused }))
                    .await
                    .is_err()
                {
                    break;
                }
                continue;
            }
            // **The picture stops when the screen stops showing that it is
            // being watched.**
            //
            // Two rules, because the two failures are different: something
            // that can be put back — a compositor restarting destroys
            // override-redirect windows — is worth waiting a few seconds for,
            // and something drawn *over* the frame cannot be fixed from here
            // at all. Either way the picture stops **at once**; what differs
            // is whether the session survives.
            Turn::Indicator => {
                let now = sightings
                    .as_ref()
                    .map_or(crate::presence_task::Sighting::OnScreen, |s| {
                        s.borrow().clone()
                    });
                match &now {
                    crate::presence_task::Sighting::OnScreen => {
                        if hidden_since.take().is_some() {
                            let still_held = picture_is_held(paused, false);
                            println!(
                                "      · the indicator is back on screen; the picture {}",
                                if still_held { "stays held" } else { "resumes" }
                            );
                            let _ = hold.send(still_held);
                            if control
                                .send(&MediaMessage::Paused(esper_proto::Paused {
                                    paused: still_held,
                                }))
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                    crate::presence_task::Sighting::Down => {}
                    crate::presence_task::Sighting::Missing(missing) => {
                        if hidden_since.is_none() {
                            hidden_since = Some(std::time::Instant::now());
                            println!("      · {}", missing.describe());
                            println!("        the picture is held until it is back");
                            let _ = hold.send(picture_is_held(paused, true));
                            // The controller is told in the vocabulary it
                            // already has for a picture that stopped, so a
                            // viewer shows "held" rather than a frozen frame
                            // it cannot explain.
                            if control
                                .send(&MediaMessage::Paused(esper_proto::Paused { paused: true }))
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                }
                continue;
            }
            // **A session whose indicator never came back does not go on.**
            //
            // The picture already stopped, so what this ends is a session
            // sitting there holding a screen nobody can see is being watched —
            // with a clipboard, a file transfer and an input dispatcher still
            // live. The grace is for the recoverable case; this is the answer
            // when recovery did not happen.
            Turn::IndicatorGaveUp => {
                let why = sightings
                    .as_ref()
                    .map(|s| s.borrow().clone())
                    .and_then(|seen| match seen {
                        crate::presence_task::Sighting::Missing(m) => Some(m.describe()),
                        _ => None,
                    })
                    .unwrap_or_else(|| "the indicator is not on screen".to_owned());
                println!("      · the indicator has not come back: {why}");
                println!(
                    "        ending the session, because a screen being watched has to say so"
                );
                attending
                    .recorded
                    .ending(&format!("the session indicator did not come back: {why}"));
                media.close(esper_proto::Reason::INDICATOR_UNCONFIRMED);
                ours = Some(esper_proto::Reason::INDICATOR_UNCONFIRMED);
                break;
            }
            Turn::Pressed(esper_presence::HostEvent::Panic) => {
                // **First, before the session is even closed.** Everything
                // else here takes a moment — a message to the far end, a
                // locker to spawn, a task to unwind — and injection is the one
                // thing that must not happen during any of them.
                attending.stop_injecting.stop_now();
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
            //
            // Ended only when what the session *is* has gone: a media session
            // is a picture, so losing `view` ends it and losing anything else
            // narrows it. That is the same rule the other session kinds
            // follow, and the reason is that ending a working session over
            // something it was not doing is a worse answer than either.
            //
            // Narrowing is not the controller's decision to accept. Injection
            // has already stopped at the host — in the code doing the
            // injecting, on the same signal — so this message is how the
            // person at the far end finds out, not how it is enforced.
            Turn::Permissions => match attending.live.standing() {
                crate::permits::Standing::Unchanged => continue,
                crate::permits::Standing::Forgotten => {
                    println!("      · ending this session: that device is no longer paired");
                    attending.recorded.ending("that device is no longer paired");
                    record_revocation(attending.log, &device);
                    media.close(esper_proto::Reason::REVOKED);
                    ours = Some(esper_proto::Reason::REVOKED);
                    break;
                }
                crate::permits::Standing::Narrowed(lost) => {
                    if lost.contains(&Permission::View) {
                        let taken = lost.iter().map(|p| p.name()).collect::<Vec<_>>().join(", ");
                        println!("      · ending this session: {taken} was taken away");
                        attending
                            .recorded
                            .ending(&format!("{taken} was taken away"));
                        record_revocation(attending.log, &device);
                        media.close(esper_proto::Reason::REVOKED);
                        ours = Some(esper_proto::Reason::REVOKED);
                        break;
                    }
                    let granted = esper_transport::Grants::granted(&attending.live);
                    // **Only when it has actually moved.** This arm runs on
                    // every change to the store, including ones about other
                    // devices, and a controller handed the same set again
                    // would be told twice about one decision.
                    if told_permissions.as_ref() == Some(&granted) {
                        continue;
                    }
                    println!(
                        "      · narrowing this session: {} was taken away, the picture keeps \
                         flowing",
                        lost.iter().map(|p| p.name()).collect::<Vec<_>>().join(", ")
                    );
                    record_narrowing(attending.log, &device, &granted);
                    told_permissions = Some(granted.clone());
                    if control
                        .send(&MediaMessage::Permissions(esper_proto::PermissionChange {
                            granted,
                        }))
                        .await
                        .is_err()
                    {
                        break;
                    }
                    continue;
                }
            },
            Turn::FromController(Ok(message)) => {
                // **Who may send this, and whether it is within bounds,
                // before anything acts on it.**
                //
                // This loop is a boundary and did not behave like one. Three
                // of these messages are acted on *here* — a link report, a
                // recording notice, a request for one more permission — and
                // the check that refuses the rest lives in the pipeline
                // thread, on the other side of a channel. So a controller's
                // capacity report reached the transport's pacer before
                // anything asked whether it was in range: the pipeline
                // refused the message a moment later and printed a line, and
                // by then the host was pacing to whatever number had arrived.
                //
                // Found by attacking it rather than by reading it, which is
                // the point of the pass this comes from.
                if !message.may_be_sent_by(esper_proto::Role::Controller)
                    || !message.is_well_formed()
                {
                    eprintln!(
                        "      · refused a message from the controller: {} is not one a \
                         controller may send, or is out of bounds",
                        message.name()
                    );
                    continue;
                }
                // **The one message this loop acts on itself.** What the
                // controller measured about the link is for the *transport* —
                // its congestion controller is the only place that can pace
                // without putting a second serialiser in front of the wire —
                // and the transport is here rather than on the pipeline's
                // thread. It is forwarded as well, so the session can say what
                // it was told.
                // **And the other: what the far end says it is doing with
                // this session.** A recording outlives the session, so the
                // person whose screen it is has to be told while it is
                // happening — which means a screen and a log, and both are
                // here rather than on the pipeline's thread.
                if let MediaMessage::Recording(notice) = &message
                    && notice.recording != recording
                {
                    {
                        recording = notice.recording;
                        if let Some(i) = attending.indicator {
                            i.show(esper_presence::Watching {
                                recording,
                                paused,
                                ..attending.watching.clone()
                            });
                        }
                        if recording {
                            println!(
                                "      · THE CONTROLLER SAYS IT IS RECORDING THIS SESSION TO A \
                                 FILE"
                            );
                        } else {
                            println!("      · the controller says it stopped recording");
                        }
                        if let Err(e) = attending
                            .log
                            .append(&Entry::now(&device, Event::Recording { on: recording }))
                        {
                            eprintln!("      · could not write the audit log: {e}");
                        }
                    }
                }
                if let MediaMessage::Report(report) = &message
                    && let Some(cell) = attending.capacity.as_ref()
                    && crate::settings::flag("host.pace_to_measured", true)
                {
                    match report.capacity_kbit {
                        Some(kbit) => {
                            cell.told(kbit);
                            if told_capacity != Some(kbit) {
                                told_capacity = Some(kbit);
                                println!(
                                    "      the viewer measures this link at {kbit} \
                                     kbit/s; pacing to it"
                                );
                            }
                        }
                        // **Withdrawn, not remembered.** A host pacing on a
                        // number nothing stands behind any more would hold a
                        // link that got faster at the old rate for the rest of
                        // the session, with nothing saying why.
                        None => {
                            cell.forget();
                            if told_capacity.is_some() {
                                told_capacity = None;
                                println!("      the viewer can no longer measure this link");
                            }
                        }
                    }
                }
                // **And the third: a session asking to be allowed one more
                // thing.** It is answered by a person at this machine, so it
                // is put to them on a thread of its own — see
                // `wait_for_an_answer` — and the picture keeps flowing while
                // they decide.
                if let MediaMessage::Ask(request) = &message {
                    let wanted = request.wanted;
                    // **A bound on the asks themselves, before any of them
                    // costs anything.**
                    //
                    // Separate from the three questions a *person* may be put
                    // — see `consent::Knocking` — because they bound different
                    // things: that one is somebody's patience, this one is the
                    // work an ask costs whether or not it reaches anybody. And
                    // it costs some: the device record is read from disk, and
                    // a request for `control` on a session that has none opens
                    // an injector to find out whether a yes could be honoured,
                    // which on Linux creates a `uinput` device. Neither is
                    // much; unbounded, both are a lever.
                    //
                    // Found by a controller that asked in a loop. Four times
                    // what a person will ever be asked, so nothing legitimate
                    // meets it.
                    const MOST_ASKS: usize = 12;
                    if asks >= MOST_ASKS {
                        if control
                            .send(&MediaMessage::Answer(
                                esper_proto::PermissionAnswer::Refused {
                                    reason: esper_proto::Reason::ASKED_ENOUGH,
                                    detail: format!(
                                        "this session has asked {MOST_ASKS} times and will \
                                         not be asked about again"
                                    ),
                                },
                            ))
                            .await
                            .is_err()
                        {
                            break;
                        }
                        continue;
                    }
                    asks = asks.saturating_add(1);
                    if asking.is_some() {
                        // **One question at a time.** A second dialog over the
                        // first would be two questions with one answer between
                        // them, and whichever was on top would collect it.
                        if control
                            .send(&MediaMessage::Answer(
                                esper_proto::PermissionAnswer::Refused {
                                    reason: esper_proto::Reason::ASKED_ENOUGH,
                                    detail: "a question is already in front of \
                                             somebody at this machine"
                                        .to_owned(),
                                },
                            ))
                            .await
                            .is_err()
                        {
                            break;
                        }
                        continue;
                    }
                    println!(
                        "      · the controller asks to be allowed {}",
                        wanted.name()
                    );
                    // **What the store says, read now rather than at the
                    // start of the session.** Somebody may have granted this
                    // permission in the meantime, and then there is nobody to
                    // ask.
                    let holds = esper_transport::Grants::granted(&attending.live);
                    let policy = Arc::clone(attending.policy);
                    let counted = Arc::clone(&knocking);
                    // Whether saying yes would need an injector this session
                    // has never opened. `controlling` is what the session
                    // opened with, so this is false for one that already
                    // drives and for anything that is not `control`.
                    let needs_a_keyboard =
                        wanted == Permission::Control && !attending.watching.controlling;
                    let screens = attending.layout.borrow().clone();
                    let stop = attending.stop_injecting.clone();
                    asking = Some(tokio::task::spawn_blocking(move || {
                        // The device record, read here because the answer
                        // depends on what the store says now and a session's
                        // copy of it is as old as the session.
                        let named = crate::paths::devices()
                            .ok()
                            .and_then(|(store, _)| store.get(&device).cloned());
                        let refuse = |reason, detail: &str| TheyAnswered {
                            wanted,
                            verdict: crate::consent::Widening::Refused(reason, detail.to_owned()),
                            keyboard: None,
                        };
                        let Some(record) = named else {
                            // Not paired any more. The session is about to end
                            // on its own — `Turn::Permissions` reads the same
                            // store — and this is not the branch that should
                            // end it.
                            return refuse(
                                esper_proto::Reason::REVOKED,
                                "this device is no longer paired",
                            );
                        };
                        // **Before anybody is asked.** A machine that cannot
                        // inject cannot honour a yes, and a question somebody
                        // answered for nothing is worse than a refusal.
                        let keyboard = if needs_a_keyboard {
                            let chosen = crate::injector::open();
                            for line in &chosen.attempts {
                                println!("      {line}");
                            }
                            if !chosen.real {
                                return refuse(
                                    esper_proto::Reason::NO_INPUT_BACKEND,
                                    "this host has nothing it can inject input with",
                                );
                            }
                            Some(esper_input::Dispatcher::new(
                                chosen.sink,
                                screens,
                                true,
                                stop.clone(),
                            ))
                        } else {
                            None
                        };
                        let Ok(mut counting) = counted.lock() else {
                            // A panic somewhere holding the count. Refusing is
                            // the only safe answer to a question about
                            // permissions.
                            return refuse(
                                esper_proto::Reason::PROMPT_TIMEOUT,
                                "the question could not be put",
                            );
                        };
                        let verdict = policy.widen(
                            &record,
                            wanted,
                            &holds,
                            &mut counting,
                            std::time::Instant::now(),
                        );
                        // **Dropped on a no**, which releases whatever the
                        // injector took — a `uinput` device, or a connection
                        // to the display server.
                        let keyboard = match verdict {
                            crate::consent::Widening::Allowed { .. } => keyboard,
                            _ => None,
                        };
                        TheyAnswered {
                            wanted,
                            verdict,
                            keyboard,
                        }
                    }));
                    continue;
                }
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
                if let MediaMessage::Config(config) = &message {
                    if let Some(watch) = attending.watch {
                        watch.watch(served_geometry(config));
                    }
                    // **And a new monitor to inject through**, which is the
                    // same event and the same reasoning read the other way: a
                    // pointer position arriving from the controller is a
                    // fraction of a monitor, so an injector working from the
                    // table this session opened with would put a click at the
                    // right fraction of the wrong screen.
                    //
                    // Sent even with no input stream open: the channel holds
                    // the latest layout, so a stream opened afterwards starts
                    // from the current one rather than from the opening one.
                    attending.layout.send_replace(monitors_from(config));
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
    let mut stats = format!(
        "{frames} frames, {} datagrams, {} KiB",
        sent.datagrams,
        sent.bytes / 1024
    );
    // **And anything the transport would not take, because silence here cost
    // a great deal once.** A refused datagram is not an error a session dies
    // of, so the sink counts it and carries on — which is right, and meant
    // that a packetizer sizing for a path that had narrowed underneath it
    // destroyed every keyframe for a whole session with no layer saying a
    // word. Said out loud, a recurrence is one line in a log rather than an
    // afternoon with a packet capture.
    if sent.refused > 0 {
        stats.push_str(&format!(
            " — {} datagram(s) the transport would not carry",
            sent.refused
        ));
    }
    Ok((stats, survivor))
}

/// What `connect` takes, for somebody who has not memorised it.
///
/// **A person cannot use a flag they cannot find out about.** Every flag here
/// was documented only in a doc comment in this file — which is to say, to
/// nobody who was not reading the source. `--profile` in particular decides
/// what a host gives up first when it runs short, and had no way of being
/// discovered at all.
///
/// The program is named as it was invoked rather than as it was built: a
/// person who typed `esper` is told to type `esper`.
///
/// Kept honest by `every_flag_the_parser_takes_is_in_its_usage`, which reads
/// the parser's own arms out of this file rather than trusting anybody to
/// remember. Help that has drifted from the program is worse than none: it
/// is a promise the program does not keep.
/// Open a stream to the host and say what it is for.
///
/// **The purpose goes first, before anything else on the stream.** The host
/// serves whatever a stream says it is, so a stream that said nothing would be
/// one the host holds open waiting to be told.
async fn announce(
    session: &Session,
    purpose: esper_proto::StreamPurpose,
) -> Result<esper_transport::Control, String> {
    let mut control = session
        .open_control()
        .await
        .map_err(|e| format!("could not open a {} stream: {e}", purpose.name()))?;
    control
        .send(&purpose)
        .await
        .map_err(|e| format!("could not say what the stream was for: {e}"))?;
    Ok(control)
}

pub fn connect_usage() -> String {
    let me = crate::net::me();
    format!(
        "usage: {me} connect [<address>|<device>] [options]\n\
         \n\
         Show a paired host's screen. An address, a device name, or a\n\
         fingerprint prefix — or nothing, when exactly one host is paired.\n\
         \n\
         \x20 --present            open a window and show the pictures\n\
         \x20 --control            send this window's keys and pointer to the host\n\
         \x20 --monitor <n[,n]>   ask for these screens rather than the one\n\
         \x20                      announced. Several arrive as one picture, in the\n\
         \x20                      arrangement the host's desktop has\n\
         \x20 --all-monitors       ask for every screen the host has\n\
         \x20 --no-audio           do not ask for the host's sound, so none is sent\n\
         \x20 --record <file.mkv>  write the session to a file. The host says so on its\n\
         \x20                      own screen for as long as it lasts\n\
         \x20 --profile <what>     text, balanced or motion: what you are doing,\n\
         \x20                      which decides what the host gives up first\n\
         \x20 --clipboard <what>   both, from-host, to-host or none. Nothing is ever\n\
         \x20                      pushed: a copy offers, the other end asks (default both)\n\
         \x20 --seconds <n>        stop after this long, for measuring\n\
         \x20 --via <broker>       be introduced by a broker when nothing direct works\n\
         \x20 --broker-key <key>   the broker's key, when it is not remembered\n\
         \x20 --no-mdns            do not ask the local network where the host is\n\
         \n\
         for example: {me} connect elythion --present --control"
    )
}

/// And what `host` takes.
pub fn host_usage() -> String {
    let me = crate::net::me();
    format!(
        "usage: {me} host [options]\n\
         \n\
         Serve this machine's screen to a paired device.\n\
         \n\
         \x20 --listen <addr>          address, or a bare port, to listen on\n\
         \x20 --monitor <n>            serve this monitor rather than the primary\n\
         \x20 --max-resolution <n>     never send a picture wider than this, or `none`\n\
         \x20 --fps <n>                never send more frames a second than this\n\
         \x20 --budget <n>             how many cores' worth of CPU to spend\n\
         \x20 --backend <name>         which capture backend to use\n\
         \x20 --forward-allow <rule>   allow a forwarded target, repeatable\n\
         \x20 --once                   serve one session and exit\n\
         \x20 --via <broker>           be reachable through a broker\n\
         \x20 --broker-key <key>       the broker's key, when it is not remembered\n\
         \x20 --no-mdns                do not advertise on the local network\n\
         \x20 --watched-by <socket>    answer a supervisor's questions here\n\
         \n\
         for example: {me} host --listen 7645 --max-resolution 1280"
    )
}

/// The share of frames going missing that stops meaning "a lossy link".
///
/// A quarter. Measured, the regimes separate either side of it: a link whose
/// short buffer is overflowing on bursts loses about half its frames, while a
/// link with five per cent packet loss — a genuinely lossy one — loses about a
/// tenth.
const LOSS_THAT_IS_NOT_A_SLOW_LINK: f64 = 0.25;

/// Queueing delay above the path's own floor that counts as a filling queue.
///
/// The same hundred milliseconds the controller's own ladder steps on, and
/// used here for the opposite purpose: to establish that the queue is *not*
/// what went wrong, which is what makes the loss mean something else.
const QUEUE_THAT_IS_GROWING: Duration = Duration::from_millis(100);

/// Whether what went wrong was a buffer too short to hold one frame.
///
/// **Two conditions, and the second is what makes it mean anything.** Frames
/// going missing is ordinary; frames going missing on a path whose round trip
/// never moved is not. A link that is genuinely slow queues before it drops,
/// so its round trip grows; a link whose buffer cannot hold one frame drops
/// the tail of every large one and stays as quick as it ever was.
///
/// Which is the case the rest of the system reads wrongly: it looks exactly
/// like packet loss, so the ladder spends frame size on it, and frame size is
/// not the problem — the *shape* of a frame's departure is.
fn looks_like_a_short_buffer(lost: f64, rtt: Duration, floor: Duration) -> bool {
    lost >= LOSS_THAT_IS_NOT_A_SLOW_LINK && rtt.saturating_sub(floor) < QUEUE_THAT_IS_GROWING
}

/// What share of the frames a host sent never arrived whole.
///
/// Pictures and skipped frames both arrived; a lost one did not. Zero when
/// nothing happened at all, because a session with no frames has no fraction
/// rather than a fraction of nothing.
fn lost_fraction(pictures: u64, skipped: u64, lost: u64) -> f64 {
    let sent = pictures.saturating_add(skipped).saturating_add(lost);
    if sent == 0 {
        return 0.0;
    }
    lost as f64 / sent as f64
}

/// Where to look for a device, and what may be told in the looking.
///
/// Shared by every command that has to *get* to a host — a screen, a shell, a
/// forward. Written once because the ladder is the same ladder: a command that
/// grew its own copy would be the one that still dials the address in the
/// store after the machine has moved.
#[derive(Debug, Clone, Copy)]
pub struct Reaching<'a> {
    /// What to print before dialling anything, like `"📺 esper connect"`.
    pub label: &'a str,
    /// Where the host is, when somebody knows.
    pub addr: Option<SocketAddr>,
    /// Which paired host, by name or fingerprint prefix.
    pub named: Option<&'a str>,
    /// A broker to be introduced through, when nothing direct works.
    pub via: Option<esper_rendezvous::Via>,
    /// Ask the segment where it is.
    pub mdns: bool,
    /// Say all of it on stderr rather than stdout.
    ///
    /// For `forward --stdio`, where **stdout is somebody's ssh connection**.
    /// A banner printed there is not untidy, it is protocol corruption — and
    /// the one thing that would not have caught it is a test that greps the
    /// output for what it expected to find.
    pub aside: bool,
}

/// A host reached, and how.
pub struct Reached {
    /// Held by the caller: dropping it closes the socket under the session.
    pub endpoint: Endpoint,
    pub session: Session,
    pub tier: crate::ladder::Tier,
}

/// Reach a paired host, by whichever rung answers first.
///
/// Prints what it is doing as it goes, because every one of these steps can
/// take a moment and a program that says nothing while it tries three
/// addresses looks like a program that has hung.
pub async fn reach(identity: &esper_crypto::Identity, o: &Reaching<'_>) -> Result<Reached, String> {
    let (store, keys) = paired()?;
    // Everything this function says goes through here, so `aside` cannot be
    // honoured in four places and forgotten in the fifth.
    let say = |line: String| {
        if o.aside {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    };

    // **Which device somebody meant.** A name, a fingerprint prefix, or — when
    // exactly one is paired — the obvious one. An address on the command line
    // says where without saying which, and the key check is what settles that
    // either way.
    let device = match (o.named, keys.as_slice()) {
        (Some(named), _) => Some(crate::ladder::device_named(store.iter(), named)?),
        (None, [_]) => store.iter().next(),
        (None, _) if o.addr.is_none() => {
            return Err(format!(
                "{} devices are paired, so say which: `{} <name>` — \
                 `{} devices` lists them",
                keys.len(),
                o.label.split_whitespace().last().unwrap_or("connect"),
                net::me(),
            ));
        }
        (None, _) => None,
    };

    // **A pairing that is over is said so here, before anything is dialled.**
    // The host would refuse it anyway, and "the host does not accept this
    // device" is the wrong sentence for a support pairing that did exactly
    // what it was meant to do.
    if let Some(d) = device
        && !d.admits(paths::now_secs())
    {
        let said = format!(
            "the pairing with {} was for one session, and that session has happened. \
             Ask them for a new invitation",
            d.name
        );
        // Tidied away as it is refused, so the list does not fill up with
        // people somebody helped once. The message above is printed first
        // because it is the only time it can be: from here on the honest
        // answer is that there is no such device.
        forget(&d.public_key);
        return Err(said);
    }

    match (o.addr, device) {
        (Some(addr), _) => say(format!("{} {addr}", o.label)),
        (None, Some(d)) => say(format!("{} {}", o.label, d.name)),
        (None, None) => return Err("there is no address and no device to reach".to_owned()),
    }
    say(String::new());

    // **Whatever this dials, and nothing else.** With a broker in the picture
    // that is the broker as well as the host — one socket does both, because
    // the address a router hands out belongs to the socket and a rendezvous on
    // its own would open a path for a port nothing was going to listen on.
    let mut dialable = keys.clone();
    if let Some(via) = o.via {
        dialable.push(via.key);
    }
    let dial: Arc<dyn esper_transport::PeerAuthority> = if let [only] = dialable.as_slice() {
        Arc::new(OnlyPeer::new(*only))
    } else {
        Arc::new(AnyOf::new(dialable))
    };

    let device_key = device.map(|d| d.public_key);
    let rungs_from_the_store = device.map(|d| crate::ladder::direct_rungs(d, &[]));

    // Bound to match whatever is being dialled first; quinn needs the runtime
    // that is already under us.
    let toward = o
        .addr
        .or(o.via.map(|v| v.addr))
        .unwrap_or(net::any_port(SocketAddr::from(([127, 0, 0, 1], 0))));
    let endpoint = Endpoint::bind(identity, net::any_port(toward), Arc::new(NoPeers), dial)
        .map_err(|e| format!("binding a local port: {e}"))?;
    let hello = Hello::new(Role::Controller, identity.public_key(), &net::this_device());

    // Everywhere worth trying without telling anybody: the address given, or
    // what the segment says plus where it was last time.
    let mut rungs: Vec<crate::ladder::Rung> = match o.addr {
        Some(addr) => vec![crate::ladder::Rung {
            tier: crate::ladder::Tier::Given,
            addr,
        }],
        None => {
            let mut rungs = rungs_from_the_store.unwrap_or_default();
            if o.mdns
                && let Some(key) = device_key
            {
                for addr in on_the_segment(&key).await {
                    // Ahead of what was remembered: a device that has just
                    // moved is right here and wrong in the store.
                    if !rungs.iter().any(|r| r.addr == addr) {
                        rungs.insert(
                            0,
                            crate::ladder::Rung {
                                tier: crate::ladder::Tier::Lan,
                                addr,
                            },
                        );
                    }
                }
            }
            rungs
        }
    };
    rungs.dedup_by_key(|r| r.addr);

    // **Which of these are already private.** A fact about this machine's
    // routing table rather than about where the address came from, so it is
    // applied to every rung at once, after they are collected.
    let overlays = crate::settings::overlays();
    crate::ladder::label_private(&mut rungs, &overlays);

    let (session, tier) = match reach_directly(&endpoint, &rungs, hello.clone()).await {
        Some(Ok(found)) => found,
        // Nothing direct worked, or there was nothing to try. A broker is
        // asked last on purpose: every rung above it tells nobody anything,
        // and this one tells the broker who is meeting whom.
        other => {
            let direct = match other {
                Some(Err(why)) => why,
                _ => Vec::new(),
            };
            let Some(via) = o.via else {
                return Err(no_way_in(&direct, &rungs));
            };
            let peer = device_key.ok_or("no device to ask the broker for")?;
            let bound = endpoint
                .local_addr()
                .map_err(|e| format!("reading the bound address: {e}"))?;
            let session = crate::broker_cmd::reach(
                &endpoint,
                identity,
                via,
                peer,
                crate::broker_cmd::candidates(bound, via.addr),
                hello,
            )
            .await?;
            (session, crate::ladder::Tier::Broker)
        }
    };
    say(format!(
        "  reached    {} — {}",
        session.remote_address(),
        tier.describe()
    ));

    // **Where it answered, remembered.** A hint for next time, and the reason
    // a machine that has been reached once is reached instantly afterwards
    // even with no discovery and no broker.
    // Asked about the address that actually answered, rather than read off
    // the rung it came from: a broker introduction that lands on a private
    // path is still a private path, and it is the address that gets stored.
    let privately = overlays.path_to(session.remote_address().ip()).is_some();
    remember_where(session.peer_key(), session.remote_address(), privately);

    let known = store
        .iter()
        .find(|d| d.public_key == *session.peer_key())
        .map(|d| d.name.clone());
    say(format!(
        "  host       {}",
        known.unwrap_or_else(|| "unknown".to_owned())
    ));
    for line in session.peer_fingerprint().display().lines() {
        say(format!("      {line}"));
    }

    // **Spent on this side too.** The host is what enforces one session, but a
    // controller that kept dialling a pairing which is over would produce a
    // refusal from the far end instead of an explanation from this one.
    spend_pairing(*session.peer_key());

    Ok(Reached {
        endpoint,
        session,
        tier,
    })
}

/// Connect to a paired host and show what it sends.
pub fn run_connect(o: &ConnectOptions) -> Result<(), String> {
    net::install_crypto();
    let identity = paths::identity()?;

    net::runtime()?.block_on(async {
        let Reached {
            endpoint,
            mut session,
            // Recorded in the trail, so "this was relayed" is visible
            // afterwards rather than only in the moment it was printed.
            tier,
        } = reach(
            &identity,
            &Reaching {
                label: "📺 esper connect",
                addr: o.addr,
                named: o.named.as_deref(),
                via: o.via,
                mdns: o.mdns,
                aside: false,
            },
        )
        .await?;

        let (media, mut control) = parts(&mut session).map_err(|e| e.to_string())?;

        // **Say what this session is for, and hear what it may actually do.**
        // The grant may be smaller than the request and the controller renders
        // what it holds: asking for control and assuming it was given is how a
        // client offers a cursor that does nothing.
        // **A wait a person can see.** A host that has to ask somebody takes as
        // long as they take to walk over, and a controller showing nothing for
        // three quarters of a minute is one somebody kills before it works.
        // Said after a beat rather than up front, because the ordinary case is
        // an answer inside a round trip and a note about waiting would be a
        // note about nothing.
        let request = esper_proto::SessionRequest {
            kind: esper_proto::SessionKind::Screen,
            want: {
                // Asked for, not assumed: the host grants a subset, and
                // this side renders what it holds.
                let mut want = vec![Permission::View];
                if o.control {
                    want.push(Permission::Control);
                }
                // **Asked for unless declined**, because the grant is the
                // intersection of what the device holds and what the session
                // asked for: a session that never asked would be granted no
                // sound however the host is configured, and the person who
                // granted the permission would be left wondering where their
                // sound went.
                if !o.no_audio {
                    want.push(Permission::Audio);
                }
                want.extend(o.clipboard.wanted());
                want
            },
            codecs: vec![config_codec_name().to_owned()],
            reason: "a screen".to_owned(),
        };
        let asking = esper_transport::request_session(&mut control, &request);
        let grant = {
            let mut asking = std::pin::pin!(asking);
            let said = tokio::time::timeout(SAY_WE_ARE_WAITING, &mut asking).await;
            match said {
                Ok(answer) => answer,
                Err(_) => {
                    println!("  waiting    for somebody at the host to allow this session");
                    asking.await
                }
            }
        }
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
        // **Written down at this end too.** The host has recorded who
        // connected to it since Phase 2; this is the other half of the same
        // question — what this machine reached, and what it was allowed to do
        // there. One line per session, and never a byte of what crossed.
        let far_end = crate::trail::peer_of(&session);
        crate::trail::went_out(
            &far_end,
            esper_audit::Event::SessionOpened {
                granted: grant.granted.clone(),
                tier: tier.describe().to_owned(),
                // This end asked and was given a session; it was never told
                // which of the host's reasons applied.
                allowed: None,
            },
        );

        let may_drive = grant.allows(Permission::Control);
        // **Asked for and not granted, so the host is asked.** Said here
        // rather than discovered later by a keystroke that had no effect —
        // and asked rather than only said, because the alternative is a phone
        // call in which somebody is talked through `esper grant`, and the
        // fastest way through that call is granting more than the moment
        // needs, for ever.
        //
        // The question is put once the session is up: it is a question about
        // this session, and the answer lasts exactly as long as it does.
        let ask_to_drive = o.control && !may_drive;
        if ask_to_drive {
            println!("  control    not granted — asking somebody at the host for this session");
        }

        let link = ControllerLink::open(media, control)
            .await
            .map_err(|e| format!("the host did not announce a stream: {e}"))?;
        let config = link.config().clone();
        println!(
            "  stream     {}x{} {} at up to {} fps{}",
            config.width,
            config.height,
            config.codec.name(),
            config.max_fps,
            if config.serving.len() > 1 {
                format!(" — {} screens as one picture", config.serving.len())
            } else {
                String::new()
            }
        );
        // **What somebody at the host can see of this**, said here because a
        // person watching a screen is entitled to know whether the person
        // whose screen it is has any way of knowing. The host refuses to serve
        // when it has a display and cannot show an indicator, so `NotShown`
        // only arrives from a host whose owner deliberately allowed it — and
        // it is still the loudest line in this output.
        if let Some(warning) = config.seen.warning() {
            match config.seen {
                esper_proto::Seen::NotShown => println!("  presence   ⚠ {warning}"),
                _ => println!("  presence   {warning}"),
            }
        }
        for m in &config.monitors {
            println!(
                "      {} {}  {}x{} at {},{}{}",
                if config.serving.contains(&m.id) {
                    "→"
                } else {
                    " "
                },
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
        // **Say what this viewer is doing, before anything else is asked
        // for.** The host cannot work it out, and what it decides to give up
        // when it runs short depends on the answer.
        //
        // `Text` asks for a sharp picture and `Motion` for a soft one, which
        // is the same statement said twice — once as content and once as a
        // consequence — because the host acts on the second and a person
        // reads the first. `Balanced` states no preference at all and leaves
        // the host's own judgement alone.
        let wanted = match o.profile {
            Profile::Text => Some(Sharpness::Sharp),
            Profile::Balanced => None,
            Profile::Motion => Some(Sharpness::Soft),
        };
        link.hint(None, None, o.profile)
            .await
            .map_err(|e| format!("saying what this viewer is doing: {e}"))?;
        link.want_sharpness(wanted)
            .await
            .map_err(|e| format!("saying what this viewer will settle for: {e}"))?;
        if ask_to_drive {
            // **Not fatal, and said.** A host that cannot be asked is a
            // session that watches, which is what it was going to be anyway.
            if let Err(e) = link.ask_for(Permission::Control).await {
                println!("  control    could not ask the host ({e}); watching only");
            }
        }

        let mut asked_for: Option<Vec<u32>> = None;
        // Which screens the picture is of, so a layout change can be told
        // apart from a switch — the host sends one message for both.
        let mut showing: Option<Vec<u32>> = Some(config.serving.clone());
        // **`--all-monitors` becomes a list here and not before**: which
        // screens a host has is something only the host knows, and it has just
        // said so.
        let wanted = if o.all_monitors {
            Some(config.monitors.iter().map(|m| m.id).collect::<Vec<u32>>())
        } else {
            o.monitors.clone()
        };
        if let Some(wanted) = wanted {
            let named = esper_stream::screens_named(&wanted);
            if wanted == config.serving {
                println!("  monitor    the host is already sending {named}");
            } else {
                println!("  monitor    asking for {named}");
                link.select_monitors(wanted.clone());
                link.flush_outbound()
                    .await
                    .map_err(|e| format!("asking for {named}: {e}"))?;
                asked_for = Some(wanted);
            }
        }

        if o.no_audio {
            println!("  sound      not asked for (--no-audio), so none is sent");
        }
        // The speaker, once the host announces sound. `None` until then, and
        // for the whole of most sessions.
        let mut playing: Option<crate::playback_task::Playing> = None;
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
        // What this session holds, as the host last said. Starts from the
        // grant it opened with, so the first narrowing can say what went.
        let mut held_permissions: Vec<Permission> = grant.granted.clone();
        if o.control && may_drive {
            match presenter.as_mut() {
                Some(p) => match announce(&session, esper_proto::StreamPurpose::Input).await {
                    Ok(control) => {
                        p.forward_input(true);
                        println!("  control    keys and pointer go to the host");
                        driving = Some((
                            crate::input_source::InputSource::new(
                                esper_stream::tiles_of(&config),
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

        // **The clipboard, from the same grant the host is working from.**
        // The permissions are written from the host's side, so this end reads
        // them the other way round: a host that will let this controller read
        // its clipboard needs this controller willing to put what comes back
        // onto its own. One negotiated grant, both ends, no second set of
        // switches to disagree with the first.
        let (clipboard_stop, clipboard_stopping) = tokio::sync::watch::channel(false);
        let clipboard = match crate::clipboard_task::open(
            esper_clipboard::Grants::mirrored(&grant.granted),
            "  ",
        ) {
            Ok(attached) => {
                let backend = attached.backend;
                match announce(&session, esper_proto::StreamPurpose::Clipboard).await {
                    Ok(control) => {
                        println!("  clipboard  {backend} — offered, never pushed");
                        Some(tokio::spawn(crate::clipboard_task::serve(
                            control,
                            attached,
                            clipboard_stopping,
                        )))
                    }
                    Err(e) => {
                        println!("  clipboard  unavailable ({e})");
                        None
                    }
                }
            }
            Err(why) => {
                println!("  clipboard  {why}");
                None
            }
        };

        let deadline =
            (o.seconds > 0).then(|| tokio::time::Instant::now() + Duration::from_secs(o.seconds));
        let started = std::time::Instant::now();
        // **The recording, if one was asked for.**
        //
        // Set up before the first frame can arrive, and the host is told
        // *before* anything is written: the person whose screen it is should
        // learn about a recording as it starts, not after it has some of their
        // screen in it. A host that never hears it is a host whose controller
        // is not this program.
        let mut recorder = o.record.as_deref().map(|path| {
            let audio = link.sound_shape().map(|(rate, channels)| esper_mkv::Audio {
                sample_rate: rate,
                channels,
            });
            crate::record_task::Recorder::waiting(path, (config.width, config.height), audio)
        });
        if recorder.is_some() {
            link.keep_for_recording(true);
            match link.announce_recording(true).await {
                Ok(()) => println!("  recording  the host has been told, and will show it"),
                // **Not fatal, and said.** A host that cannot be told is a
                // host whose screen is about to be recorded without it saying
                // so — which the person running this should know about, and
                // which is their decision rather than this program's.
                Err(e) => {
                    eprintln!("  · the host could not be told about the recording: {e}");
                    eprintln!("    it will not show one. Recording anyway, because you asked.");
                }
            }
            // A keyframe now rather than at the host's own pace, because the
            // file cannot begin without one — a session opens with one, so
            // this matters for a recording started later rather than at
            // connect time.
            if let Err(e) = link.ask_for_keyframe().await {
                eprintln!("  · could not ask for a keyframe to start the recording: {e}");
            }
        }
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
                        // **Four different things arrive as one message**, and
                        // a viewer that said the same sentence for all of them
                        // would tell somebody their picture had changed when
                        // only the host's monitor list had.
                        let was = showing.replace(announced.serving.clone());
                        let layout_only = was.as_ref() == Some(&announced.serving);
                        match asked_for.take_if(|m| *m != announced.serving) {
                            Some(refused) => println!(
                                "  · the host would not send {}; it is still sending {}",
                                esper_stream::screens_named(&refused),
                                esper_stream::screens_named(&announced.serving)
                            ),
                            // Nothing was asked for and the picture is still
                            // the same screen: the host's desktop changed
                            // around it. Worth saying, because a picker built
                            // from the old list is now wrong.
                            None if layout_only && asked_for.is_none() => {
                                println!(
                                    "  · the host's desktop changed: {} monitor(s)",
                                    announced.monitors.len()
                                );
                                for m in &announced.monitors {
                                    println!(
                                        "      monitor {} {}  {}x{} at {},{}{}",
                                        m.id,
                                        m.name,
                                        m.width,
                                        m.height,
                                        m.x,
                                        m.y,
                                        if announced.serving.contains(&m.id) {
                                            " — showing"
                                        } else {
                                            ""
                                        }
                                    );
                                }
                            }
                            None => {
                                // A switch this end did not ask for is the
                                // screen it was watching having gone away.
                                if asked_for.is_none()
                                    && let Some(gone) = was.as_ref()
                                {
                                    // What is no longer being sent, which with
                                    // one screen is the screen that went and
                                    // with several is whichever of them did.
                                    let lost: Vec<u32> = gone
                                        .iter()
                                        .copied()
                                        .filter(|id| !announced.serving.contains(id))
                                        .collect();
                                    if !lost.is_empty() {
                                        println!(
                                            "  · the host's {} went away",
                                            esper_stream::screens_named(&lost)
                                        );
                                    }
                                }
                                asked_for = None;
                                println!(
                                    "  · now showing {}: {}x{}",
                                    esper_stream::screens_named(&announced.serving),
                                    announced.width,
                                    announced.height
                                );
                                // The same window position is now a fraction
                                // of a different screen.
                                if let Some((source, _)) = driving.as_mut() {
                                    source.showing(
                                        esper_stream::tiles_of(&announced),
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
                    // **And the round trip.** The link samples it on every
                    // turn of its own accord; this covers the case where
                    // nothing is arriving at all, which is a quiet link rather
                    // than a fast one and is also when it is worth knowing.
                    link.observe_link();
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
            // **The recording, before anything is shown.** What goes in the
            // file is what the host *sent*, taken from the stream before this
            // machine decides whether it managed to decode or draw it: a
            // recording with holes where the viewer stuttered would be a worse
            // record of the session than the session was.
            if let Some(writing) = recorder.as_mut() {
                writing.take_from(&mut link, started.elapsed());
            }
            for d in out.drain(..) {
                match d {
                    Decoded::Picture { picture, .. } => {
                        pictures = pictures.saturating_add(1);
                        // **The first one, said once.** On a good link this is
                        // a few tens of milliseconds and nobody needs telling;
                        // on a link that has to ratchet 1280x720 down to
                        // something a 200 kbit/s pipe carries it is several
                        // seconds, and the difference between "working" and
                        // "hung" is exactly this line. It is also what a test
                        // can wait for instead of guessing how long to allow.
                        if pictures == 1 {
                            println!(
                                "  first      picture after {} ms",
                                started.elapsed().as_millis()
                            );
                        }
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
                            // **Which screen it is on, and where in the
                            // picture that screen is.** A position on the wire
                            // is a fraction of a named screen, so with two
                            // screens tiled the same fraction is two different
                            // places — and drawing it against the whole
                            // picture would put the pointer on the wrong
                            // screen, at the wrong scale.
                            //
                            // A pointer on a screen this session is not being
                            // shown is not in the picture: nothing to draw,
                            // and nothing to wear either — a worn pointer
                            // would be showing the person a cursor for a
                            // screen they cannot see.
                            let on_screen = esper_stream::tiles_of(link.config())
                                .into_iter()
                                .find(|(id, _)| *id == at.monitor);
                            let target = on_screen.and_then(|(_, tile)| {
                                draw.then(|| {
                                    let (x, y) = at.to_pixels(tile.width, tile.height);
                                    (
                                        x.saturating_add(tile.x.unsigned_abs()),
                                        y.saturating_add(tile.y.unsigned_abs()),
                                    )
                                })
                            });
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
            // **A held picture, said rather than shown as a frozen one.**
            //
            // A viewer whose frames stopped shows the last one it had, which
            // is exactly what a dead link looks like — and somebody watching
            // that would reconnect, which is the thing pausing exists to
            // avoid.
            if let Some(held) = link.take_paused() {
                println!(
                    "  · the host {} the picture{}",
                    if held { "is holding" } else { "released" },
                    if held {
                        " — the session, its permissions and anything in flight are kept"
                    } else {
                        ""
                    }
                );
            }
            // **What the host says this session may now do.**
            //
            // Acted on before anything else in this turn, so a keystroke the
            // window produced a moment ago is not sent under a permission that
            // has just gone. It would be refused at the host either way -- the
            // enforcement is there, not here -- but sending it anyway would
            // mean this end knew and carried on.
            // **The sound, once the host says there is any.** It is announced
            // rather than assumed: audio is permissioned on its own, a host
            // may have no sound device, and the announcement carries the SSRC
            // that tells audio datagrams from the picture's.
            //
            // The decoder goes in a jail of its own — a peer's packets reach a
            // C library, and a failure in it must not take the picture down —
            // and the speaker goes on its own thread, because playing blocks.
            //
            // There is no "declined" branch here: `--no-audio` means the
            // session never asked for the permission, so a host has nothing
            // to announce — which is better than declining, because the sound
            // is never sent and never paid for.
            if let Some(announced) = link.take_audio() {
                match crate::playback_task::start(esper_audio::Format::of(&announced)) {
                    Ok(speaker) => match esper_decode::SandboxedAudio::spawn_self(&announced) {
                        Ok(worker) => {
                            let jail = worker.report().describe();
                            match link.with_sound(&announced, Box::new(worker)) {
                                Ok(()) => {
                                    println!(
                                        "  sound      {} Hz, {} channel(s) — decoder sandboxed \
                                         ({jail}), playing through {}",
                                        announced.sample_rate_hz,
                                        announced.channels,
                                        speaker.backend()
                                    );
                                    playing = Some(speaker);
                                }
                                Err(e) => println!("  sound      unavailable ({e})"),
                            }
                        }
                        // **No fallback to decoding in process.** A fallback
                        // is how a security property becomes a preference, and
                        // the one time it would trigger is the one time it
                        // matters.
                        Err(e) => println!("  sound      refused: no sandbox for it ({e})"),
                    },
                    Err(e) => println!("  sound      nowhere to play it ({e})"),
                }
            }
            // Handed to the speaker without waiting: see `playback_task`.
            if let Some(speaker) = playing.as_mut() {
                for frame in link.take_sound() {
                    if !speaker.play(frame) {
                        println!("  · the sound stopped");
                        playing = None;
                        break;
                    }
                }
            }
            if let Some(now_granted) = link.take_permissions() {
                let lost: Vec<&str> = held_permissions
                    .iter()
                    .filter(|p| !now_granted.contains(p))
                    .map(|p| p.name())
                    .collect();
                held_permissions = now_granted;
                if !lost.is_empty() {
                    println!("  · the host took away: {}", lost.join(", "));
                }
                // **And the other direction, which only somebody at the host
                // can cause.** The store cannot widen a live session, so a
                // set that has *gained* something is a person there having
                // answered a question about this session.
                if held_permissions.contains(&Permission::Control) && driving.is_none() && o.control
                {
                    match presenter.as_mut() {
                        Some(p) => {
                            match announce(&session, esper_proto::StreamPurpose::Input).await {
                                Ok(control) => {
                                    p.forward_input(true);
                                    println!(
                                        "  · allowed at the host: keys and pointer go there now"
                                    );
                                    driving = Some((
                                        crate::input_source::InputSource::new(
                                            esper_stream::tiles_of(&config),
                                            (config.width, config.height),
                                        ),
                                        control,
                                    ));
                                }
                                Err(e) => println!("  · control was allowed, but ({e})"),
                            }
                        }
                        // Nothing to drive from, which is the same answer as
                        // at the start of a session: input comes from the
                        // viewer's own window.
                        None => println!("  · control was allowed, but this viewer has no window"),
                    }
                }
                if !held_permissions.contains(&Permission::Control)
                    && let Some((source, control)) = driving.as_mut()
                {
                    // **Let go of what is held, then stop driving.** A
                    // modifier held at the moment control went away is one the
                    // host is about to release on its own timer; saying so
                    // takes one message and gets there at once.
                    if source.is_holding() {
                        let _ = control.send(&esper_proto::InputMessage::ReleaseAll).await;
                    }
                    println!("  · no longer driving this session; watching only");
                    driving = None;
                }
            }
            // **What somebody at the host said about a request for more.**
            // The set itself arrives in its own message, handled above; this
            // is the answer to the question, and a refusal is the only thing
            // that would otherwise leave somebody watching a screen wondering
            // whether anybody saw it.
            if let Some(answer) = link.take_answer() {
                match answer {
                    esper_proto::PermissionAnswer::Allowed => {}
                    esper_proto::PermissionAnswer::Refused { reason, detail } => {
                        println!("  · the host did not allow it: {detail} [{reason}]");
                    }
                }
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
        let link_capped = link.link_ceiling();
        let link_fps = link.link_fps();
        let link_baseline = link.baseline_round_trip();
        let capacity = link.link_capacity();
        println!();
        // **Why it ended, in the vocabulary the protocol has for it.** Every
        // reason code used to be a failure, so a session ending normally
        // looked like one; the 0xx class exists so "the person closed the
        // window" and "the host refused you" are different sentences.
        match &closed_by_peer {
            Some((code, text)) => {
                let described = code.describe().unwrap_or(text);
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
        // **Stopped before the summary, so what it was holding is gone by the
        // time this process says anything else.** Dropping the sender ends the
        // task, which drops what it holds, which wipes it.
        drop(clipboard_stop);
        if let Some(task) = clipboard {
            task.abort();
        }
        println!();

        let path = link.transport();
        let jitter = link.jitter();
        let seconds = started.elapsed().as_secs_f64().max(0.001);
        // **The other end of the outbound line**, so the trail holds how long
        // this machine was connected rather than only that it was. The reason
        // is the sentence the far end or this one gave, which is what makes a
        // line about a session that *ended badly* distinguishable afterwards.
        crate::trail::went_out(
            &far_end,
            esper_audit::Event::SessionClosed {
                duration_secs: started.elapsed().as_secs(),
                reason: closed_by_peer.as_ref().map_or_else(
                    || "closed at this end".to_owned(),
                    |(code, text)| {
                        if text.is_empty() {
                            format!("the host closed it ({})", code.0)
                        } else {
                            format!("the host closed it: {text}")
                        }
                    },
                ),
            },
        );
        // **The recording is closed before the summary**, so its own line —
        // how many frames, how long, how big — sits with the rest of what the
        // session came to rather than after it.
        if let Some(recorder) = recorder.take() {
            // Told first, because the host's screen should stop saying
            // "recording" as soon as it stops being true, and a file that
            // takes a moment to close is not a moment of recording.
            if let Err(e) = link.announce_recording(false).await {
                eprintln!("  · the host could not be told the recording stopped: {e}");
            }
            recorder.finish();
        }
        println!("  pictures   {pictures} shown, {skipped} not shown");
        // **Said only when there was sound**, and said in the terms that
        // matter: how much was heard, how much was reconstructed because a
        // packet was lost, and how much was thrown away because it arrived too
        // late to play in order. The last number is the A/V policy, counted —
        // sound is never delayed to line up with the picture.
        if let Some(sound) = link.sound_stats() {
            println!(
                "  sound      {} frame(s) played, {} concealed, {} too late{}",
                sound.frames,
                sound.concealed,
                sound.late,
                match playing.as_ref().map(crate::playback_task::Playing::dropped) {
                    Some(n) if n > 0 => format!(", {n} dropped at the speaker"),
                    _ => String::new(),
                }
            );
        }
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
        // **And how many of those had nothing to prompt them.** A keyframe
        // asked for because nothing was arriving is the diagnostic for a state
        // that used to be unrecoverable, so it is worth naming when it
        // happens: one or two is a lost keyframe recovered, and a number that
        // climbs is a host that is not answering.
        let silent = if stats.keyframes_asked_into_silence > 0 {
            format!(
                ", {} of them because nothing was arriving",
                stats.keyframes_asked_into_silence
            )
        } else {
            String::new()
        };
        println!(
            "  frames     {} lost, {} keyframes asked for{silent}",
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
        // **Why the picture was smaller than the screen.** A session that
        // stepped down and never said so leaves somebody comparing a blurry
        // window against a machine that was working perfectly well.
        if let Some(edge) = link_capped {
            println!(
                "  link       asked for {edge} pixels on the long edge: frames were being lost"
            );
        }
        // **And why it was less smooth.** A queue filling is invisible from
        // this end except as latency, and a viewer told nothing would put a
        // stuttering session down to the host being slow. The baseline is
        // printed with it because the number that matters is the *difference*:
        // a 200 ms path is fine, and 200 ms of queue on top of it is not.
        if let Some(fps) = link_fps {
            match link_baseline {
                Some(base) => println!(
                    "  queue      settled for {fps} fps: the round trip grew from {:.0} ms, \
                     which is this session's bytes waiting in a buffer",
                    base.as_secs_f64() * 1000.0
                ),
                None => println!("  queue      settled for {fps} fps: the round trip was growing"),
            }
        }
        // **What the link can actually carry**, which is not what it carried:
        // the second is demand and the first is capacity, and only a frame's
        // own arrival measures the difference. A number worth showing on its
        // own — somebody asking why a session is soft is better served by
        // "this link carries about 800 kbit/s" than by any amount of advice.
        //
        // **And it says which of the two measurements it is.** One times a
        // frame's own datagrams arriving together; the other counts bytes
        // while frames are failing, cannot tell a frame from a fragment, and
        // is a lower bound. Both used to print the first sentence, which is
        // the sentence somebody reads to decide whether to believe the
        // number — measured, an 800 kbit/s link with a buffer too short for
        // one frame reported "about 398 kbit/s, from how fast a frame's
        // datagrams arrived together", and only the last clause was wrong.
        if let Some(carried) = capacity {
            match carried.how {
                esper_stream::Measured::ByABurst => println!(
                    "  capacity   about {:.0} kbit/s, from how fast a frame's \
                     datagrams arrived together",
                    carried.kbit
                ),
                esper_stream::Measured::AtLeast => println!(
                    "  capacity   at least {:.0} kbit/s, counted while frames were \
                     failing — nothing here could time one",
                    carried.kbit
                ),
            }
        }
        // **A short buffer, which looks exactly like a lossy link and is not.**
        //
        // A frame's datagrams leave the host together, and a buffer that
        // cannot hold one frame drops the tail of every large one. What
        // reaches this end is frames going missing on a path whose round trip
        // never moves — the signature of a *lossy* link, which is why the
        // ladder above spends frame size on it, and frame size is not the
        // problem.
        //
        // Measured: two links both carrying 800 kbit/s, one with 40 ms of
        // buffer and one with 200, gave 9-31 pictures and 144-148. Neither
        // was short of capacity. Modern routers keep buffers deliberately
        // short, so this is the *good* equipment — and nothing in a session's
        // own numbers said so until this line.
        if let Some(base) = link_baseline
            && looks_like_a_short_buffer(
                lost_fraction(pictures, skipped, stats.frames_lost),
                path.rtt,
                base,
            )
        {
            println!(
                "  buffer     frames arrived in pieces on a path that never slowed \
                 (round trip {:.0} ms against a floor of {:.0} ms): the link's buffer is \
                 most likely too short for one frame, which no resolution can fix",
                path.rtt.as_secs_f64() * 1000.0,
                base.as_secs_f64() * 1000.0
            );
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {

    /// **Two reasons to stop the picture, and neither cancels the other.**
    ///
    /// The case worth pinning is the third row: a person pressed pause while
    /// the indicator was missing, and the indicator coming back must not
    /// release a pause they chose. The first version sent the person's own
    /// state on recovery and `true` on loss, which got that exactly backwards
    /// — a screen somebody had deliberately hidden came back on its own.
    #[test]
    fn the_picture_flows_only_when_neither_reason_says_stop() {
        assert!(!super::picture_is_held(false, false), "nothing is wrong");
        assert!(
            super::picture_is_held(true, false),
            "the person pressed pause"
        );
        assert!(
            super::picture_is_held(true, true),
            "paused, and the indicator is missing: still held"
        );
        assert!(
            super::picture_is_held(false, true),
            "nobody paused, and nobody at the host can see this session"
        );
    }

    /// **The input surface is the picture, and nothing else.**
    ///
    /// A position naming a screen the session is not sending is refused by the
    /// injector, and this is where that is decided: the table it works from is
    /// built from `serving`. A controller cannot produce such a position from a
    /// picture it does not have, so one arriving is a stale message from before
    /// a switch — which would land at the right fraction of the wrong screen —
    /// or a peer acting on a screen it cannot see.
    #[test]
    fn the_injector_is_told_only_about_the_screens_being_sent() {
        use super::monitors_from;
        let screen = |id: u32, x: i32| esper_proto::MonitorDescription {
            id,
            name: format!("screen-{id}"),
            x,
            y: 0,
            width: 1920,
            height: 1080,
            scale_percent: 100,
            primary: id == 0,
        };
        let config = esper_proto::StreamConfig {
            monitors: vec![screen(0, 0), screen(1, 1920), screen(2, 3840)],
            selected: 0,
            serving: vec![0, 2],
            codec: esper_proto::Codec::H264,
            width: 5760,
            height: 1080,
            max_fps: 30,
            clock_hz: 90_000,
            ssrc: 1,
            seen: esper_proto::Seen::Shown,
        };
        let told: Vec<u32> = monitors_from(&config).iter().map(|m| m.id).collect();
        assert_eq!(
            told,
            vec![0, 2],
            "the injector was told about a screen this session is not sending"
        );
    }

    /// **An ordinary ending is worth locking for, and the host already knows
    /// when.**
    ///
    /// A device that connects to an unattended machine, does something and
    /// disconnects used to leave it logged in, unlocked and showing whatever
    /// it was left showing. The panic key locked; nothing else did.
    #[test]
    fn a_session_nobody_was_asked_about_locks_the_screen_after_it() {
        use crate::consent::Allowed;
        for how in [Allowed::Unattended, Allowed::Invited] {
            assert!(
                lock_now(LockAfter::Unattended, Some(how), true),
                "{how:?}: nobody here was asked, so nobody here is watching"
            );
        }
    }

    /// And a session somebody answered for does not, because they are still
    /// at the machine and locking it under them is rude and useless.
    #[test]
    fn a_session_somebody_answered_for_leaves_the_screen_alone() {
        use crate::consent::Allowed;
        for how in [Allowed::Accepted, Allowed::StillAccepted] {
            assert!(
                !lock_now(LockAfter::Unattended, Some(how), true),
                "{how:?}: somebody answered a prompt here a moment ago"
            );
        }
    }

    /// **Not knowing counts as nobody.** A session this host cannot say how it
    /// allowed is one it cannot vouch for, and the safe reading of an unknown
    /// is the one that locks.
    #[test]
    fn a_session_of_unknown_provenance_locks() {
        assert!(lock_now(LockAfter::Unattended, None, true));
    }

    /// **A machine with no screen is not warned about a door it has not got.**
    ///
    /// A server reached over ssh cannot lock and has nothing to leave
    /// readable, and an alarming line after every session on such a host is
    /// how people learn to ignore alarming lines.
    #[test]
    fn a_host_with_no_display_says_nothing_about_locking() {
        use crate::consent::Allowed;
        for after in [LockAfter::Unattended, LockAfter::Always, LockAfter::Never] {
            for how in [None, Some(Allowed::Unattended), Some(Allowed::Accepted)] {
                assert!(
                    !lock_now(after, how, false),
                    "{after:?} with {how:?} tried to lock a machine with no screen"
                );
            }
        }
    }

    /// **A device's own answer wins, and the host is told which decided.**
    ///
    /// The whole of the per-device setting is this precedence. A rule that
    /// silently went the other way would look exactly like a machine whose
    /// setting nobody had noticed — which is why the answer carries where it
    /// came from and the host says it.
    #[test]
    fn a_devices_own_answer_beats_the_hosts_rule() {
        for host in LockAfter::ALL {
            assert_eq!(
                lock_rule(None, host),
                (host, Decided::ByTheHost),
                "a device with no answer of its own must follow the host"
            );
            for theirs in LockAfter::ALL {
                assert_eq!(
                    lock_rule(Some(theirs), host),
                    (theirs, Decided::ByTheDevice),
                    "the device said {theirs:?} and the host said {host:?}"
                );
            }
        }
    }

    /// The two overrides do what they say, whatever happened.
    #[test]
    fn always_and_never_do_not_consult_anything() {
        use crate::consent::Allowed;
        for how in [
            None,
            Some(Allowed::Accepted),
            Some(Allowed::StillAccepted),
            Some(Allowed::Unattended),
            Some(Allowed::Invited),
        ] {
            assert!(lock_now(LockAfter::Always, how, true), "{how:?}");
            assert!(!lock_now(LockAfter::Never, how, true), "{how:?}");
        }
    }

    use super::*;

    /// **The two failures that look identical from here.**
    ///
    /// Frames going missing on a link that never slowed is a buffer too short
    /// to hold one frame; frames going missing on a link that did slow is a
    /// queue. Measured, two links both carrying 800 kbit/s and differing only
    /// in buffer gave 9-31 pictures and 144-148, and every other number a
    /// session reports was the same shape.
    ///
    /// Told them apart wrongly, the advice is actively harmful: the ladder
    /// spends resolution on a problem no resolution can fix.
    #[test]
    fn a_short_buffer_is_told_from_a_slow_link() {
        let ms = Duration::from_millis;
        // The measured short-buffer case: half the frames gone, a round trip
        // that never left its floor.
        assert!(looks_like_a_short_buffer(0.5, ms(15), ms(8)));
        // The measured bloated case: the same loss, and a round trip half a
        // second above where it started. That is a queue, and the ladder that
        // watches the round trip already answers it.
        assert!(!looks_like_a_short_buffer(0.5, ms(520), ms(20)));
        // A genuinely lossy link: five per cent of packets, a tenth of the
        // frames, and nothing about the buffer to report.
        assert!(!looks_like_a_short_buffer(0.1, ms(3), ms(2)));
        // And a session where nothing went wrong at all.
        assert!(!looks_like_a_short_buffer(0.0, ms(2), ms(2)));
    }

    /// The fraction is over what the host *sent*, which is everything that
    /// arrived plus everything that did not.
    #[test]
    fn the_lost_fraction_counts_frames_that_arrived_and_were_discarded() {
        // Skipped frames arrived: they cost the link everything they cost,
        // and counting them as losses would read a session recovering from
        // one lost keyframe as a session losing most of its frames.
        assert!((lost_fraction(10, 10, 0) - 0.0).abs() < f64::EPSILON);
        assert!((lost_fraction(0, 0, 10) - 1.0).abs() < f64::EPSILON);
        assert!((lost_fraction(40, 40, 20) - 0.2).abs() < f64::EPSILON);
        // A session with nothing in it has no fraction rather than a division
        // by zero.
        assert!((lost_fraction(0, 0, 0) - 0.0).abs() < f64::EPSILON);
    }

    /// **Help that has drifted from the program is worse than none.**
    ///
    /// It is a promise the program does not keep, and the person who believes
    /// it spends their time on a flag that was renamed. Writing the first
    /// draft of `host_usage` from memory produced exactly that: `--forward`,
    /// which has been `--forward-allow` since the day it existed.
    ///
    /// So this reads the parser's own match arms out of this file and checks
    /// both directions. Reading the source is unusual and it is the only way
    /// to be *exact*: any list kept alongside the parser is a second thing to
    /// remember, and the whole failure being guarded against is somebody not
    /// remembering.
    #[test]
    fn every_flag_the_parser_takes_is_in_its_usage() {
        // **Each parser says which file it is in.** The check read only this
        // one, so a command whose parser lived elsewhere was outside it — and
        // `screenshot`'s does. Searching every source for the parser's name
        // was the first fix and it was wrong in a way worth recording: this
        // file *contains* the name of every parser, because the table below
        // names them, so the search found this file and scanned its own test.
        let flags_of = |source: &str, parser: &str| -> Vec<String> {
            let from = source
                .find(parser)
                .unwrap_or_else(|| panic!("{parser} is gone"));
            let rest = &source[from + parser.len()..];
            let to = rest.find("\npub fn ").unwrap_or(rest.len());
            let mut found: Vec<String> = Vec::new();
            for line in rest[..to].lines() {
                let line = line.trim();
                // `"--flag" => {`, which is how every arm here is written.
                if let Some(after) = line.strip_prefix("\"--")
                    && let Some(name) = after.split('"').next()
                    && line.contains("=>")
                {
                    found.push(format!("--{name}"));
                }
            }
            found.sort();
            found.dedup();
            assert!(
                !found.is_empty(),
                "{parser} has no flags, which cannot be right"
            );
            found
        };

        let here = include_str!("session_cmd.rs");
        let shot = include_str!("screenshot_cmd.rs");
        for (source, parser, usage, command) in [
            (here, "pub fn parse_connect", connect_usage(), "connect"),
            (here, "pub fn parse_host", host_usage(), "host"),
            (
                shot,
                "pub fn parse",
                crate::screenshot_cmd::usage(),
                "screenshot",
            ),
        ] {
            for flag in flags_of(source, parser) {
                assert!(
                    usage.contains(&flag),
                    "{command} takes {flag} and its usage does not mention it: \
                     a flag nobody can find out about"
                );
            }
            // And the other way, which is the one that lies to a person: a
            // usage naming a flag the parser would refuse.
            for word in usage.split_whitespace() {
                let word = word.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-');
                if word.starts_with("--") && word.len() > 2 {
                    assert!(
                        flags_of(source, parser).iter().any(|f| f == word),
                        "{command}'s usage names {word}, which its parser does not take"
                    );
                }
            }
        }
    }

    /// **What a person types, and the three answers there are.**
    ///
    /// A profile is the one input a host cannot work out for itself — a
    /// slideshow and a video look the same to a damage tracker — so getting
    /// it from the command line wrong, or silently, would leave the host
    /// guessing while a person believed they had said.
    #[test]
    fn a_profile_is_what_the_person_says_they_are_doing() {
        let parse = |args: &[&str]| {
            let owned: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
            parse_connect(&owned)
        };
        assert_eq!(
            parse(&["127.0.0.1:7645"]).expect("no profile").profile,
            Profile::Balanced,
            "the default is the one to pick when unsure"
        );
        for (typed, wanted) in [
            ("text", Profile::Text),
            ("balanced", Profile::Balanced),
            ("motion", Profile::Motion),
        ] {
            let o = parse(&["127.0.0.1:7645", "--profile", typed]).expect("a profile");
            assert_eq!(o.profile, wanted, "--profile {typed}");
        }
        // The clipboard is asked for in the terms a person thinks in, not in
        // the terms the permissions are stored in.
        for (typed, wanted, permissions) in [
            ("both", ClipboardWish::Both, 2),
            ("from-host", ClipboardWish::FromHost, 1),
            ("to-host", ClipboardWish::ToHost, 1),
            ("none", ClipboardWish::None, 0),
        ] {
            let o = parse(&["127.0.0.1:7645", "--clipboard", typed]).expect("a clipboard");
            assert_eq!(o.clipboard, wanted, "--clipboard {typed}");
            assert_eq!(
                o.clipboard.wanted().len(),
                permissions,
                "--clipboard {typed}"
            );
        }
        // The two named directions ask for opposite permissions, or the option
        // is one setting with two spellings.
        assert_ne!(
            ClipboardWish::FromHost.wanted(),
            ClipboardWish::ToHost.wanted()
        );
        assert_eq!(
            ClipboardWish::FromHost.wanted(),
            vec![Permission::ClipboardOut]
        );
        assert_eq!(
            ClipboardWish::ToHost.wanted(),
            vec![Permission::ClipboardIn]
        );
        // Asked for by default: a remote desktop where copy and paste do
        // nothing until a flag is found is a remote desktop people work around.
        assert_eq!(
            parse(&["127.0.0.1:7645"]).expect("no options").clipboard,
            ClipboardWish::Both
        );
        assert!(parse(&["127.0.0.1:7645", "--clipboard", "sideways"]).is_err());
        assert!(parse(&["127.0.0.1:7645", "--clipboard"]).is_err());

        // **Refused rather than defaulted.** A typo silently becoming
        // `balanced` is how somebody watching video spends the session
        // wondering why it stutters.
        let err = parse(&["127.0.0.1:7645", "--profile", "cinema"]).expect_err("a typo");
        assert!(err.contains("cinema") && err.contains("motion"), "{err}");
        assert!(
            parse(&["127.0.0.1:7645", "--profile"]).is_err(),
            "a profile with no answer is not an answer"
        );
    }

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
