//! The control channel to the Go control plane.
//!
//! Length-prefixed CBOR over stdin and stdout — the same framing as the wire
//! protocol, so there is one implementation of it to get right rather than two.
//!
//! **Media never crosses this channel.** It carries commands and state at
//! roughly ten messages a second. Everything touching a frame stays inside this
//! process, which is the entire reason the seam is affordable.

use std::io::{BufReader, BufWriter, Read, Write};

use esper_proto::{Capability, Command, Hello, Report, Role, Status, read_frame, write_frame};

/// Run the control channel until the peer closes it.
///
/// The exchange opens with `Hello` in both directions. A version mismatch is
/// reported and the process exits rather than continuing: the two binaries ship
/// together, so a mismatch means a partial install, and every later failure
/// would be a confusing symptom of this one.
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    // **Before anything else: put back a resolution a previous run changed.**
    //
    // A session that changes the remote's resolution owes it back, and it
    // gives it back when the session ends. A host that is *killed* ends no
    // session — and leaves the person sitting at that machine with a screen
    // they did not choose and nothing to tell them why. The record on disk is
    // the promise; this is where it is kept.
    //
    // Here rather than in `main` because this is the command that hosts. A
    // `pair` or a `devices` has no business touching somebody's display.
    match crate::paths::home()
        .map_err(esper_display_mode::ModeError::Record)
        .and_then(|home| esper_display_mode::restore_anything_left_behind(&home))
    {
        Ok(esper_display_mode::Restored::Nothing) => {}
        Ok(esper_display_mode::Restored::PutBack) => {
            eprintln!(
                "{}: a previous session left this screen's resolution changed; it is back",
                crate::net::me()
            );
        }
        // Said, not swallowed. This is a screen somebody is looking at.
        Ok(esper_display_mode::Restored::CouldNotReach(why)) => {
            eprintln!(
                "{}: a previous session changed this screen's resolution and it cannot be \
                 put back: {why}",
                crate::net::me()
            );
        }
        Err(why) => {
            eprintln!(
                "{}: could not check for a changed resolution: {why}",
                crate::net::me()
            );
        }
    }

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = BufWriter::new(stdout.lock());

    handshake(&mut reader, &mut writer)?;

    // The command loop. It ends when the control plane closes the pipe, which
    // is how a clean shutdown reaches us.
    loop {
        match read_frame::<Command, _>(&mut reader) {
            Ok(command) => {
                let report = answer(&command);
                write_frame(&mut writer, &report)?;
                writer.flush()?;
            }
            Err(e) => {
                // A truncated read is the control plane closing the pipe, which
                // is the normal way this process ends.
                if matches!(e, esper_proto::FrameError::Truncated { got: 0, .. }) {
                    return Ok(());
                }
                return Err(Box::new(e));
            }
        }
    }
}

/// Answer one command.
///
/// Every failure is a [`Report::Failed`] rather than an error out of the loop:
/// a control plane that asked something this machine cannot do should hear
/// why, not watch its engine exit.
fn answer(command: &Command) -> Report {
    match command {
        Command::Status => Report::Status(Box::new(status())),
        Command::Sessions { most } => sessions(*most),
        // **A `serve` engine is not hosting anything**, so there is nothing
        // for it to be alive *about*: it has no listening address, no session
        // and no count. Answering with zeroes would put a healthy-looking
        // report on a supervisor's screen for a process that is not the one
        // being watched. The question belongs to `host`, over the channel in
        // `crate::supervised`.
        Command::Alive => Report::Failed {
            detail: "this engine is answering a command channel rather than hosting, so \
                     it has nothing to report being alive about — ask the host that is serving"
                .to_owned(),
        },
    }
}

/// The most lines of history the engine will read out, whatever was asked for.
///
/// A control plane asking for everything must not be able to make this read a
/// year of log into memory — and nobody scrolls past a few hundred lines of
/// anything.
const MOST_HAPPENINGS: usize = 500;

/// What has happened on this machine lately, most recent last.
fn sessions(most: u32) -> Report {
    let path = match crate::paths::audit_log() {
        Ok(p) => p,
        Err(e) => return Report::Failed { detail: e },
    };
    let entries = match esper_audit::Log::at(path).read() {
        Ok(entries) => entries,
        // A log that cannot be read is worth saying rather than an empty list
        // that reads as "nothing has ever happened here".
        Err(e) => {
            return Report::Failed {
                detail: e.to_string(),
            };
        }
    };

    // The names are looked up once for the whole list rather than per line:
    // the store is small, and reading it fourteen times to render fourteen
    // lines is the kind of thing that is fine until somebody has a thousand.
    let store = crate::paths::devices().map(|(store, _)| store).ok();

    let most = (most as usize).min(MOST_HAPPENINGS);
    let from = entries.len().saturating_sub(most);
    Report::Sessions(
        entries
            .get(from..)
            .unwrap_or(&[])
            .iter()
            .map(|entry| esper_proto::Happening {
                at: entry.at,
                peer: entry.peer.clone(),
                key: entry.peer_key.clone(),
                event: entry.event.tag().to_owned(),
                detail: entry.event.detail(),
                // The word the log stores, not this crate's own spelling of
                // it: one vocabulary, so a line somebody greps for and a
                // field a program branches on are the same string.
                direction: if entry.direction.is_inbound() {
                    "in".to_owned()
                } else {
                    "out".to_owned()
                },
                device: store
                    .as_ref()
                    .and_then(|store| named(store, &entry.peer_key))
                    .unwrap_or_default(),
            })
            .collect(),
    )
}

/// What `serve` takes, which is nothing.
///
/// A usage even so: it is in the engine's own help, and that help says running
/// this binary directly is for diagnostics — so somebody reading the list will
/// ask what this one does, and answering with the list they just read is not
/// an answer.
#[must_use]
pub fn usage() -> String {
    let me = crate::net::me();
    format!(
        "usage: {me} serve\n\
         \n\
         Speak the control protocol on standard input and output. This is how\n\
         `esper` talks to the engine it supervises: it is not a command to\n\
         type, and run by hand it will wait for a handshake that never comes.\n\
         \n\
         It carries commands and state — roughly ten messages a second — and\n\
         never a frame. Media does not cross this seam.\n\
         \n\
         Takes nothing."
    )
}

/// What this machine can actually do, established by trying it.
///
/// **Probed, not declared.** An encoder that is compiled in and cannot start,
/// a capture backend that is present and refuses the display — those are what
/// a person runs a diagnostic to find, and a report assembled from build flags
/// would say everything is fine.
fn status() -> Status {
    Status {
        engine: esper_buildinfo::VERSION.to_owned(),
        capture: capture(),
        encoder: encoder(),
        input: input(),
        overlays: crate::settings::overlays()
            .iter()
            .map(|o| o.name.clone())
            .collect(),
        mdns: crate::settings::flag("discovery.mdns", true),
        broker: crate::settings::broker()
            .ok()
            .flatten()
            .map(|via| via.addr.to_string()),
        paired: crate::paths::devices()
            .map(|(store, _)| u32::try_from(store.iter().count()).unwrap_or(u32::MAX))
            .unwrap_or(0),
        platform: platform(),
    }
}

/// What kind of machine this is, and what it is showing on.
///
/// Said so a screen can name a *remedy*: "no capture backend" is a different
/// sentence on a Mac, where somebody has to allow screen recording, from on
/// Wayland, where a portal has to be installed. The display is read from the
/// environment the same way everything else in this product reads it — see
/// `esper_desktop`, which exists so the answer is in one place.
fn platform() -> esper_proto::Platform {
    let display = if cfg!(target_os = "linux") {
        match esper_desktop::session_from(
            std::env::var("DISPLAY").ok().as_deref(),
            std::env::var("WAYLAND_DISPLAY").ok().as_deref(),
            std::env::var("XDG_SESSION_TYPE").ok().as_deref(),
        ) {
            esper_desktop::Session::X11 => "x11",
            // Both are there and the X11 path works, which is the thing a
            // remedy turns on: a machine with XWayland needs none of the
            // portal advice.
            esper_desktop::Session::WaylandWithX11 => "x11",
            esper_desktop::Session::Wayland => "wayland",
            esper_desktop::Session::Headless => "headless",
        }
    } else {
        ""
    };
    esper_proto::Platform {
        os: std::env::consts::OS.to_owned(),
        display: display.to_owned(),
    }
}

/// What the device with this key is called now, if it is still paired.
fn named(store: &esper_crypto::DeviceStore, key_hex: &str) -> Option<String> {
    store
        .iter()
        .find(|d| {
            d.public_key
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
                == key_hex
        })
        .map(|d| d.name.clone())
}

/// Open a capturer and say what it turned out to be.
fn capture() -> Capability {
    match crate::backend::open(crate::backend::Choice::Auto, None, (320, 180)) {
        Ok(selected) => {
            let name = selected.capturer.backend();
            // A synthetic screen is what this machine falls back to when there
            // is no real one. It is a working capturer and it is not a
            // screen, and a diagnostic that called it available would be the
            // reason somebody spent an afternoon wondering why their session
            // showed a test pattern.
            if name == "synthetic" {
                Capability::missing("no real screen").after(selected.attempts)
            } else {
                Capability::working(name).after(selected.attempts)
            }
        }
        Err(e) => Capability::missing(e.to_string()),
    }
}

/// Build an encoder and encode one frame with it.
fn encoder() -> Capability {
    use esper_encode::encoder::{EncoderConfig, VideoEncoder as _};

    let config = EncoderConfig::for_size(320, 180);
    let mut encoder = match esper_encode::openh264::OpenH264::new(&config) {
        Ok(e) => e,
        Err(e) => return Capability::missing(e.to_string()),
    };
    // A blank frame, which is the cheapest one there is and still exercises
    // everything between here and the bitstream.
    let picture = match esper_color::I420::new(320, 180) {
        Ok(p) => p,
        Err(e) => return Capability::missing(e.to_string()),
    };
    match encoder.encode(&picture, true) {
        Ok(encoded) => Capability::working(format!(
            "openh264, software, {} assembly, {} bytes for one keyframe",
            if esper_encode::has_assembly() {
                "with"
            } else {
                "without"
            },
            encoded.len()
        )),
        Err(e) => Capability::missing(format!("openh264 starts and cannot encode: {e}")),
    }
}

/// Open an input sink and say whether it can actually inject.
fn input() -> Capability {
    let chosen = crate::injector::open();
    if chosen.real {
        Capability::working(chosen.sink.backend()).after(chosen.attempts)
    } else {
        Capability::missing("nothing — this machine is view only").after(chosen.attempts)
    }
}

/// Exchange `Hello` and validate the peer's.
fn handshake<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
) -> Result<(), Box<dyn std::error::Error>> {
    let peer: Hello = read_frame(reader)?;

    if !peer.version_matches() {
        return Err(format!(
            "protocol version mismatch: engine speaks {}, control plane speaks {} — \
             the two ship together, so one is from a different install",
            esper_proto::PROTOCOL_VERSION,
            peer.proto
        )
        .into());
    }
    if !peer.name_is_acceptable() {
        return Err("control plane sent an unacceptable name".into());
    }

    // The engine identifies as the host role: it is the side that captures and
    // is controlled. The device key is zero until identity lands in Phase 1.
    let mut ours = Hello::new(Role::Host, [0u8; 32], "esper-engine");
    ours.engine = esper_buildinfo::VERSION.to_owned();
    write_frame(writer, &ours)?;
    writer.flush()?;
    Ok(())
}
