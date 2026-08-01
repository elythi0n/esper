//! The Esper engine.
//!
//! Owns the realtime path end to end: capture, encode, transport, decode,
//! present, and input injection. It is a separate process from the Go control
//! plane, and media never crosses between them — the socket carries commands
//! and state, roughly ten messages a second, never a frame.
//!
//! At this stage it reports its own identity and exits. Everything else is
//! built on top of that.

mod audio_task;
mod backend;
mod broker_cmd;
mod clipboard_task;
mod consent;
mod cp_cmd;
mod cursor_task;
mod devices_cmd;
mod discover_cmd;
mod files_task;
mod forward_cmd;
mod injector;
mod input_source;
mod invite_cmd;
mod ladder;
mod loopback;
mod net;
mod pairing;
mod paths;
mod permits;
mod playback_task;
mod presence_task;
mod screenshot_cmd;
mod serve;
mod session_cmd;
mod settings;
mod terminal_cmd;
mod trail;

use std::process::ExitCode;

use esper_buildinfo::{BUILD_TIME, VERSION};

fn main() -> ExitCode {
    // **Settings first, and a bad one stops here.** A file that is there and
    // cannot be understood is somebody having meant something; running with
    // the defaults instead would be running with settings they did not choose.
    if let Err(e) = settings::load() {
        eprintln!("{}: {e}", net::me());
        return ExitCode::FAILURE;
    }

    let args: Vec<String> = std::env::args().skip(1).collect();

    // **Asking for help is never a failure.**
    //
    // Handled before the command runs, because `--help` is a question rather
    // than an attempt: a person who asks gets an answer on stdout and an exit
    // status of nought, not a usage line on stderr after an error.
    //
    // **And every command answers**, not only the ones with a list of their
    // own. Four of them used to reply "unexpected argument" or "unknown flag
    // --help" — which is a program telling somebody that asking was a
    // mistake. Where there is nothing specific to say, the general help is
    // the honest answer, and it is still an answer.
    if let Some(command) = args.first().map(String::as_str)
        && args.iter().skip(1).any(|a| a == "--help" || a == "-h")
    {
        match usage_for(command) {
            Some(usage) => println!("{usage}"),
            None => print_help(),
        }
        return ExitCode::SUCCESS;
    }

    match args.first().map(String::as_str) {
        None | Some("version" | "--version" | "-v") => {
            print_version();
            ExitCode::SUCCESS
        }
        Some("identity") => match print_identity() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{}: {e}", net::me());
                ExitCode::FAILURE
            }
        },
        Some("loopback") => {
            let rest: Vec<String> = std::env::args().skip(2).collect();
            match loopback::parse(&rest).and_then(|o| loopback::run(&o)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("loopback: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        // **Re-executed as the sandboxed decoder.** The worker is this same
        // binary, so the two halves of the parent-child protocol cannot be at
        // different versions — which for a protocol between them is worth more
        // than the tidiness of a second executable.
        //
        // Checked first, and by *both* the argument and the environment
        // variable, so an operator cannot reach it by accident and a stray
        // `decode-worker` on a command line does nothing.
        // **And the sandboxed *audio* decoder**, which is its own process for
        // the same reason plus one: a malformed packet of sound must not take
        // the picture down. The shape it decodes arrives on the command line,
        // because the child has to build its decoder before the jail goes on.
        Some(esper_decode::AUDIO_WORKER_ARG)
            if std::env::var_os(esper_decode::WORKER_ENV).is_some() =>
        {
            let rest: Vec<String> = std::env::args().skip(2).collect();
            match audio_worker_config(&rest)
                .ok_or_else(|| {
                    "decode-audio-worker wants a rate, a channel count and a frame length"
                        .to_owned()
                })
                .and_then(|config| {
                    esper_decode::audio::run_on_stdio(&config).map_err(|e| e.to_string())
                }) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("decode-audio-worker: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        // **Re-executed as the keeper.** It holds no session and decodes
        // nothing: it waits for this process to die and puts the display
        // server back if it does. Gated by the argument *and* the environment
        // variable, like the decode workers, so a stray `keeper` on a command
        // line does nothing.
        Some(esper_keeper::KEEPER_ARG) if std::env::var_os(esper_keeper::KEEPER_ENV).is_some() => {
            let home = crate::paths::home().unwrap_or_default();
            let stdin = std::io::stdin();
            let stdout = std::io::stdout();
            #[cfg(target_os = "linux")]
            let applied = esper_keeper::run_keeper(
                stdin.lock(),
                stdout.lock(),
                &home,
                esper_presence_x11::put_back,
            );
            #[cfg(not(target_os = "linux"))]
            let applied = esper_keeper::run_keeper(stdin.lock(), stdout.lock(), &home, |_| {
                Err("no backend on this platform knows how to put that back".to_owned())
            });
            // Nothing to put back is the ordinary ending, and it is not a
            // failure: it means the host shut down properly.
            let _ = applied;
            ExitCode::SUCCESS
        }
        Some(esper_decode::WORKER_ARG) if std::env::var_os(esper_decode::WORKER_ENV).is_some() => {
            match esper_decode::child::run_on_stdio(&esper_decode::decoder::DecoderConfig::default())
            {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("decode-worker: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("discover") => {
            let rest: Vec<String> = std::env::args().skip(2).collect();
            match discover_cmd::parse(&rest).and_then(|o| discover_cmd::run(&o)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("discover: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("pair") => {
            let rest: Vec<String> = std::env::args().skip(2).collect();
            match pairing::parse(&rest).and_then(|o| pairing::run(&o)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("pair: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("host") => {
            let rest: Vec<String> = std::env::args().skip(2).collect();
            match session_cmd::parse_host(&rest).and_then(|o| session_cmd::run_host(&o)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("host: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("connect") => {
            let rest: Vec<String> = std::env::args().skip(2).collect();
            match session_cmd::parse_connect(&rest).and_then(|o| session_cmd::run_connect(&o)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("connect: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("screenshot") => {
            let rest: Vec<String> = std::env::args().skip(2).collect();
            match screenshot_cmd::parse(&rest).and_then(|o| screenshot_cmd::run(&o)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("screenshot: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("cp") => {
            let rest: Vec<String> = std::env::args().skip(2).collect();
            match cp_cmd::parse(&rest).and_then(|o| cp_cmd::run(&o)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("cp: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("config") => {
            let rest: Vec<String> = std::env::args().skip(2).collect();
            let outcome = match rest.first().map(String::as_str) {
                None | Some("show") => settings::show(),
                Some("example") => {
                    settings::print_example();
                    Ok(())
                }
                Some("path") => match paths::config_file() {
                    Ok(path) => {
                        println!("{}", path.display());
                        Ok(())
                    }
                    Err(e) => Err(e),
                },
                Some(other) => Err(format!(
                    "unknown: `config {other}` — try `show`, `example` or `path`"
                )),
            };
            match outcome {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("{} config: {e}", net::me());
                    ExitCode::FAILURE
                }
            }
        }
        Some("devices") => match devices_cmd::list() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{} devices: {e}", net::me());
                ExitCode::FAILURE
            }
        },
        Some("grant") => {
            let rest: Vec<String> = std::env::args().skip(2).collect();
            match devices_cmd::change(&rest, devices_cmd::Change::Grant) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("{} grant: {e}", net::me());
                    ExitCode::FAILURE
                }
            }
        }
        Some("revoke") => {
            let rest: Vec<String> = std::env::args().skip(2).collect();
            match devices_cmd::change(&rest, devices_cmd::Change::Revoke) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("{} revoke: {e}", net::me());
                    ExitCode::FAILURE
                }
            }
        }
        Some("invite") => {
            let rest: Vec<String> = std::env::args().skip(2).collect();
            match invite_cmd::parse_invite(&rest).and_then(|o| invite_cmd::run_invite(&o)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("{} invite: {e}", net::me());
                    ExitCode::FAILURE
                }
            }
        }
        Some("join") => {
            let rest: Vec<String> = std::env::args().skip(2).collect();
            match invite_cmd::run_join(&rest) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("{} join: {e}", net::me());
                    ExitCode::FAILURE
                }
            }
        }
        Some("schedule") => {
            let rest: Vec<String> = std::env::args().skip(2).collect();
            match devices_cmd::schedule(&rest) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("{} schedule: {e}", net::me());
                    ExitCode::FAILURE
                }
            }
        }
        Some("keep") => {
            let rest: Vec<String> = std::env::args().skip(2).collect();
            match devices_cmd::keep(&rest) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("{} keep: {e}", net::me());
                    ExitCode::FAILURE
                }
            }
        }
        Some("forget") => {
            let rest: Vec<String> = std::env::args().skip(2).collect();
            match devices_cmd::forget(&rest) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("{} forget: {e}", net::me());
                    ExitCode::FAILURE
                }
            }
        }
        Some("terminal") => {
            let rest: Vec<String> = std::env::args().skip(2).collect();
            match terminal_cmd::parse(&rest).and_then(|o| terminal_cmd::run(&o)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("{} terminal: {e}", net::me());
                    ExitCode::FAILURE
                }
            }
        }
        Some("forward") => {
            let rest: Vec<String> = std::env::args().skip(2).collect();
            match forward_cmd::parse(&rest).and_then(|o| forward_cmd::run(&o)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("{} forward: {e}", net::me());
                    ExitCode::FAILURE
                }
            }
        }
        Some("serve") => match serve::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{}: {e}", net::me());
                ExitCode::FAILURE
            }
        },
        Some("help" | "--help" | "-h") => {
            print_help();
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("{}: unknown command {other:?}", net::me());
            print_help();
            ExitCode::FAILURE
        }
    }
}

/// Load or create this device's identity and print its fingerprint.
///
/// The fingerprint is what a person reads aloud to confirm two machines are
/// looking at the same device, so it is printed as words rather than hex.
fn print_identity() -> Result<(), Box<dyn std::error::Error>> {
    let path = identity_path()?;
    let id = esper_crypto::Identity::load_or_create(&path)?;

    println!("device fingerprint");
    println!();
    for line in id.fingerprint().display().lines() {
        println!("    {line}");
    }
    println!();
    println!("  key   {}", id.fingerprint().hex());
    println!("  file  {}", path.display());
    Ok(())
}

/// Where the identity key lives. `$ESPER_HOME` overrides, which is what makes
/// the two-process pairing test possible on one machine.
fn identity_path() -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    if let Ok(home) = std::env::var("ESPER_HOME") {
        return Ok(std::path::PathBuf::from(home).join("identity.key"));
    }
    let home = std::env::var("HOME").map_err(|_| "neither ESPER_HOME nor HOME is set")?;
    Ok(std::path::PathBuf::from(home)
        .join(".esper")
        .join("identity.key"))
}

/// What a command takes, for the commands that take anything.
///
/// `None` for the ones that take no options at all — `devices`, `identity` —
/// where a person asking for help should get the general help rather than an
/// empty list, which is what falling through to the dispatcher gives them.
fn usage_for(command: &str) -> Option<String> {
    match command {
        "connect" => Some(session_cmd::connect_usage()),
        "cp" => Some(cp_cmd::usage()),
        "host" => Some(session_cmd::host_usage()),
        "screenshot" => Some(screenshot_cmd::usage()),
        "schedule" => Some(devices_cmd::schedule_usage()),
        _ => None,
    }
}

fn print_version() {
    println!("esper-engine {VERSION}");
    println!("  built     {BUILD_TIME}");
    println!("  protocol  {}", esper_proto::PROTOCOL_VERSION);
}

fn print_help() {
    println!(
        "esper-engine — the Esper realtime engine\n\
         \n\
         This binary is normally launched and supervised by `esper`.\n\
         Running it directly is for diagnostics.\n\
         \n\
         Commands:\n  \
           identity   print this device's fingerprint\n  \
           discover   list the Esper devices on this network\n  \
           pair       pair with another device, in either direction\n  \
           config     show every setting and where its value came from\n  \
           devices    list paired devices and what they may do\n  \
           screenshot one picture of a paired host's screen, as a PNG\n  \
           grant      give a paired device a permission (--yes to confirm a\n  \
           \x20          dangerous one without being asked)\n  \
           revoke     take a permission away\n  \
           schedule   when a device may connect unattended, in local time\n  \
           keep       make a one-session pairing last\n  \
           forget     unpair a device entirely\n  \
           invite     offer somebody a way in: one link, through your broker\n  \
           join       take somebody up on an invitation\n  \
           host       serve this machine's screen to a paired device\n  \
           connect    show a paired host's screen\n  \
           terminal   open a shell on a paired host\n  \
           forward    reach a port on a paired host, ssh -L style\n  \
           cp         copy one file to or from a paired host\n  \
           serve      speak the control protocol on stdin/stdout\n  \
           version    print version, build time and protocol version\n  \
           help       this text\n\
         \n\
         Both `host` and `connect` take `--via <broker> --broker-key <key>` to be\n\
         introduced through a broker you run. There is no default broker address\n\
         anywhere in Esper: without one, those tiers do not exist."
    );
}

/// The audio worker's shape, from its own command line.
///
/// Three integers and nothing else: a config a decoder cannot be built for is
/// refused here rather than becoming a worker that starts and then fails on
/// every packet.
fn audio_worker_config(args: &[String]) -> Option<esper_proto::AudioConfig> {
    let config = esper_proto::AudioConfig {
        codec: esper_proto::AudioCodec::Opus,
        sample_rate_hz: args.first()?.parse().ok()?,
        channels: args.get(1)?.parse().ok()?,
        frame_ms: args.get(2)?.parse().ok()?,
        // The worker decodes; it never sends, so it has no stream identity.
        ssrc: 0,
    };
    config.is_well_formed().then_some(config)
}
