//! The Esper engine.
//!
//! Owns the realtime path end to end: capture, encode, transport, decode,
//! present, and input injection. It is a separate process from the Go control
//! plane, and media never crosses between them — the socket carries commands
//! and state, roughly ten messages a second, never a frame.
//!
//! At this stage it reports its own identity and exits. Everything else is
//! built on top of that.

mod ask_screen;
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
// The signal that holds a session's picture, which only Unix has. Selected
// here so the rest of the crate carries no `cfg` about it — the same shape
// `esper-pty` uses for pseudo-terminals, and for the same reason.
#[cfg(unix)]
#[path = "hold/unix.rs"]
mod hold;
#[cfg(not(unix))]
#[path = "hold/unsupported.rs"]
mod hold;
mod injector;
mod input_source;
mod invite_cmd;
mod ladder;
mod lock_cmd;
mod loopback;
mod ls_cmd;
mod net;
mod pairing;
mod paths;
mod permits;
mod playback_task;
/// The portal grant this machine holds, which is what unattended access on a
/// Wayland desktop rests on.
#[cfg(target_os = "linux")]
mod portal_grant;
mod presence_task;
mod put_back;
mod record_task;
mod screenshot_cmd;
mod serve;
mod session_cmd;
mod settings;
mod viewer_task;
// The channel a supervisor watches a host through.
mod supervised;
mod terminal_cmd;
mod trail;
mod wake_cmd;

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
    // own. Replying "unexpected argument" to `--help` is a program telling
    // somebody that asking was a mistake. Where there is nothing specific to
    // say, the general help is the honest answer, and it is still an answer.
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
            /*
                **An argument this does not know is refused, not ignored.**

                `version --json` used to print the three human lines and exit
                zero, while the front door answered the same flag with an
                object and the broker refused it — three binaries, three
                answers to one question. Somebody scripting a deployment got
                a table from one of them and no reason to look further until
                something downstream broke on it.
            */
            let rest: Vec<String> = std::env::args().skip(2).collect();
            let mut as_json = false;
            for arg in &rest {
                match arg.as_str() {
                    "--json" => as_json = true,
                    other => {
                        eprintln!(
                            "{} version: unknown option {other:?}\n\n{}",
                            net::me(),
                            version_usage()
                        );
                        return ExitCode::FAILURE;
                    }
                }
            }
            if as_json {
                print_version_as_json();
            } else {
                print_version();
            }
            ExitCode::SUCCESS
        }
        Some("sandbox") => {
            let rest: Vec<String> = std::env::args().skip(2).collect();
            let mut as_json = false;
            for arg in &rest {
                match arg.as_str() {
                    "--json" => as_json = true,
                    unknown => {
                        eprintln!(
                            "{} sandbox: unknown option {unknown:?}\n\n{}",
                            net::me(),
                            sandbox_usage()
                        );
                        return ExitCode::FAILURE;
                    }
                }
            }
            match report_the_sandbox(as_json) {
                Ok(enforcing) => {
                    if enforcing {
                        ExitCode::SUCCESS
                    } else {
                        // Non-zero, because a decoder that is not in a sandbox
                        // is the finding this command exists to report and a
                        // script watching it should see one.
                        ExitCode::FAILURE
                    }
                }
                Err(e) => {
                    eprintln!("{} sandbox: {e}", net::me());
                    ExitCode::FAILURE
                }
            }
        }
        Some("input") => {
            // `--help` never reaches here: the dispatcher answers it from
            // `usage_for` before any command runs, which is what keeps one
            // usage for one command.
            let rest: Vec<String> = std::env::args().skip(2).collect();
            let mut as_json = false;
            for arg in &rest {
                match arg.as_str() {
                    "--json" => as_json = true,
                    unknown => {
                        eprintln!(
                            "{} input: unknown option {unknown:?}\n\n{}",
                            net::me(),
                            input_usage()
                        );
                        return ExitCode::FAILURE;
                    }
                }
            }
            if as_json {
                print_input_as_json();
            } else {
                print_input();
            }
            ExitCode::SUCCESS
        }
        Some("identity") => {
            // An argument this does not know is refused, not ignored: a flag
            // accepted and disregarded is a script that reads a table and has
            // no reason to look further.
            //
            // `--help` never reaches here: the dispatcher answers it from
            // `usage_for` before any command runs, which is what keeps one
            // usage for one command.
            let rest: Vec<String> = std::env::args().skip(2).collect();
            let mut as_json = false;
            for arg in &rest {
                match arg.as_str() {
                    "--json" => as_json = true,
                    other => {
                        eprintln!(
                            "{} identity: unknown option {other:?}\n\n{}",
                            net::me(),
                            identity_usage()
                        );
                        return ExitCode::FAILURE;
                    }
                }
            }
            let printed = if as_json {
                print_identity_as_json()
            } else {
                print_identity()
            };
            match printed {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("{}: {e}", net::me());
                    ExitCode::FAILURE
                }
            }
        }
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
            let applied = esper_keeper::run_keeper(
                stdin.lock(),
                stdout.lock(),
                &home,
                crate::put_back::put_back,
            );
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
                    eprintln!("{} discover: {e}", net::me());
                    ExitCode::FAILURE
                }
            }
        }
        Some("pair") => {
            let rest: Vec<String> = std::env::args().skip(2).collect();
            match pairing::parse(&rest).and_then(|o| pairing::run(&o)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    // Named, like every other refusal: the parser says what is
                    // wrong and the dispatcher says which program and which
                    // command.
                    eprintln!("{} pair: {e}", net::me());
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
        Some("wake") => {
            let rest: Vec<String> = std::env::args().skip(2).collect();
            match wake_cmd::parse(&rest).and_then(|o| wake_cmd::run(&o)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("{} wake: {e}", net::me());
                    ExitCode::FAILURE
                }
            }
        }
        Some("group") => {
            let rest: Vec<String> = std::env::args().skip(2).collect();
            match devices_cmd::group(&rest) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("{} group: {e}", net::me());
                    ExitCode::FAILURE
                }
            }
        }
        // **One arm for three verbs**, because the difference between them is
        // the permission and the session kind and nothing else.
        Some(verb @ ("lock" | "logoff" | "reboot")) => {
            let Some(what) = lock_cmd::What::parse(verb) else {
                eprintln!("{}: unknown command {verb:?}", net::me());
                return ExitCode::FAILURE;
            };
            let rest: Vec<String> = std::env::args().skip(2).collect();
            match lock_cmd::parse(what, &rest).and_then(|o| lock_cmd::run(&o)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("{} {verb}: {e}", net::me());
                    ExitCode::FAILURE
                }
            }
        }
        Some("screenshot") => {
            let rest: Vec<String> = std::env::args().skip(2).collect();
            match screenshot_cmd::parse(&rest).and_then(|o| screenshot_cmd::run(&o)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("{} screenshot: {e}", net::me());
                    ExitCode::FAILURE
                }
            }
        }
        Some("ls") => {
            let rest: Vec<String> = std::env::args().skip(2).collect();
            match ls_cmd::parse(&rest).and_then(|o| ls_cmd::run(&o)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("{} ls: {e}", net::me());
                    ExitCode::FAILURE
                }
            }
        }
        Some("cp") => {
            let rest: Vec<String> = std::env::args().skip(2).collect();
            match cp_cmd::parse(&rest).and_then(|o| cp_cmd::run(&o)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("{} cp: {e}", net::me());
                    ExitCode::FAILURE
                }
            }
        }
        Some("config") => {
            let rest: Vec<String> = std::env::args().skip(2).collect();
            // `--json` anywhere, including as the only word: it is a way of
            // *showing*, not a subcommand, so `config --json` and
            // `config show --json` are the same thing.
            //
            // **And nothing else is.** Any other option was taken for a
            // subcommand that is not there, skipped, and the table printed —
            // so `config --porcelain` answered with the listing laid out for a
            // person and exited zero. `set` and `unset` take values that may
            // begin with a dash, so only the words before them are examined.
            let as_json = rest.iter().any(|a| a == "--json");
            let verb = rest
                .iter()
                .map(String::as_str)
                .find(|a| !a.starts_with("--"));
            let options_before_the_verb = rest
                .iter()
                .take_while(|a| a.starts_with("--"))
                .filter(|a| *a != "--json");
            if let Some(unknown) = options_before_the_verb.clone().next() {
                eprintln!(
                    "{} config: unknown option {unknown:?}\n\n\
                     usage: {} config [show] [--json] | set <key> <value> | unset <key> | example | path",
                    net::me(),
                    net::me()
                );
                return ExitCode::FAILURE;
            }
            let first = verb;
            let outcome = match first {
                None | Some("show") => {
                    // For a program, the table for a person. The control plane
                    // reads this, so the registry has exactly one owner.
                    if as_json {
                        settings::show_as_json()
                    } else {
                        settings::show()
                    }
                }
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
                Some("set") => match (rest.get(1), rest.get(2)) {
                    (Some(key), Some(value)) => settings::set(key, value),
                    _ => Err(format!(
                        "usage: {} config set <key> <value>\n\n\
                         `{} config` lists every setting and what it is now.",
                        net::me(),
                        net::me()
                    )),
                },
                Some("unset") => match rest.get(1) {
                    Some(key) => settings::unset(key),
                    None => Err(format!(
                        "usage: {} config unset <key>\n\n\
                         Takes a setting out of the file, so the default decides again.",
                        net::me()
                    )),
                },
                Some(other) => Err(format!(
                    "unknown: `config {other}` — try `show`, `set`, `unset`, `example` or `path`"
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
        Some("devices") => {
            /*
                **An argument this does not understand is refused.**

                It used to take none and ignore every one: `devices --json` was
                accepted, answered with the table laid out for a person, and
                exited zero. A script asking for JSON got a listing and no
                indication that anything was wrong, which is worse than the
                flag not existing.
            */
            let rest: Vec<String> = std::env::args().skip(2).collect();
            let mut as_json = false;
            let mut refused = None;
            for arg in &rest {
                match arg.as_str() {
                    "--json" => as_json = true,
                    // `--help` never reaches here: the dispatcher answers it
                    // from `usage_for` before any command runs, which is what
                    // keeps one usage for one command.
                    other => refused = Some(other.to_owned()),
                }
            }
            if let Some(other) = refused {
                eprintln!(
                    "{} devices: unknown option {other:?}\n\n{}",
                    net::me(),
                    devices_cmd::list_usage()
                );
                return ExitCode::FAILURE;
            }
            let done = if as_json {
                devices_cmd::list_as_json()
            } else {
                devices_cmd::list()
            };
            match done {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("{} devices: {e}", net::me());
                    ExitCode::FAILURE
                }
            }
        }
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
        Some("lock-after") => {
            let rest: Vec<String> = std::env::args().skip(2).collect();
            match devices_cmd::lock_after(&rest) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("{} lock-after: {e}", net::me());
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

/// What the input ladder chooses here, and everything it tried.
///
/// The ladder runs when a *session* starts rather than when the host does, so
/// a host's own output never shows it — and "I can see the screen and nothing
/// I type arrives" is the fault this answers. It is also the only way to check
/// the ladder's order against a real compositor, which is how the wlroots
/// backend was held to being reachable rather than merely written.
fn print_input() {
    let chosen = injector::open();
    println!("input backend");
    println!();
    if chosen.real {
        println!("  in use     {}", chosen.sink.backend());
    } else {
        println!("  in use     nothing — this machine is view only");
    }
    println!();
    println!("  tried");
    for attempt in &chosen.attempts {
        println!("      {attempt}");
    }
}

fn input_usage() -> String {
    let me = net::me();
    format!(
        "usage: {me} input [--json]\n\
     \n\
     Print which backend this machine would inject keys and pointer moves\n\
     with, and what was tried before it. A host that can be seen and not\n\
     controlled is view only, and this says why.\n\
     \n\
     Nothing is injected and no session is started.\n\
     \n\
     options:\n  \
     \x20 --json   one object, for a script rather than a person"
    )
}

/// The same answer, in the shape `esper doctor --json` reports it.
///
/// The same two keys the local API uses for this capability — `detail` for
/// the one line, `attempts` for what was tried — so a reader that knows one
/// knows the other.
fn sandbox_usage() -> String {
    let me = net::me();
    format!(
        "usage: {me} sandbox [--json]\n\
         \n\
         Start the decoder the way a session does and report the jail it ran\n\
         in — which layers applied, and what a compromised decoder could\n\
         reach if any of them did not.\n\
         \n\
         Nothing decodes a peer's bitstream here and no session is started.\n\
         \n\
         With ESPER_SANDBOX_AUDIT=1 the syscall filter logs instead of\n\
         killing, which is how a syscall missing from it is found rather than\n\
         guessed at. That is a diagnostic: a session still refuses a decoder\n\
         that is not enforcing, whatever is set here.\n\
         \n\
         options:\n  \
         \x20 --json   one object, for a script rather than a person"
    )
}

/// Report the jail the decoder runs in, and what is reachable if it is not on.
///
/// **The escape hatch, made reachable.** `ESPER_SANDBOX_AUDIT=1` exists so a
/// syscall missing from the filter can be found rather than guessed at — and
/// with a session refusing any decoder that is not enforcing, turning it on
/// taught nobody anything. This is where it is allowed to mean something,
/// because there is no peer here: the worker is started, it reports, and it
/// is stopped.
///
/// Returns whether the sandbox is actually in force.
fn report_the_sandbox(as_json: bool) -> Result<bool, String> {
    let config = esper_decode::decoder::DecoderConfig::default();
    let worker = esper_decode::SandboxedDecoder::spawn_self_for_diagnosis(&config)
        .map_err(|e| e.to_string())?;
    let report = *worker.report();
    drop(worker);

    let audit_set = std::env::var_os(esper_decode::sandbox::AUDIT_ENV).is_some();
    if as_json {
        let out = serde_json::json!({
            "enforcing": report.is_enforcing(),
            "layers": {
                "address_space": report.address_space,
                "open_files": report.open_files,
                "no_new_privs": report.no_new_privs,
                "landlock": report.landlock,
                "seccomp": report.seccomp,
                "killing": report.enforcing,
            },
            "detail": report.describe(),
            "reachable": report.what_is_reachable(),
            "audit_requested": audit_set,
        });
        println!("{out}");
        return Ok(report.is_enforcing());
    }

    println!("decoder sandbox");
    println!();
    println!("  {}", report.headline());
    println!();
    println!("  what a compromised decoder could reach");
    for line in report.what_is_reachable() {
        println!("      {line}");
    }
    if audit_set {
        println!();
        println!(
            "  {} is set, so the filter logs instead of killing.",
            esper_decode::sandbox::AUDIT_ENV
        );
        println!("  A session refuses a decoder in this state; this command does not,");
        println!("  because the only bitstream here is this machine's own.");
    }
    Ok(report.is_enforcing())
}

fn print_input_as_json() {
    let chosen = injector::open();
    let out = serde_json::json!({
        "available": chosen.real,
        "detail": if chosen.real {
            chosen.sink.backend().to_owned()
        } else {
            "nothing — this machine is view only".to_owned()
        },
        "attempts": chosen.attempts,
    });
    println!("{out}");
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
    // **Both, and each called what it is.** The fingerprint is sixty-four
    // bits, for a person to read aloud and for naming a device on a command
    // line; the key is what a peer actually pins, and labelling the short one
    // "key" is how somebody comes to compare sixty-four bits and believe they
    // have checked a machine. `esper-broker identity` prints the whole key
    // under the same word, so the word has to mean one thing.
    println!("  fingerprint  {}", id.fingerprint().hex());
    println!(
        "  key          {}",
        id.public_key().iter().fold(String::new(), |mut acc, b| {
            use std::fmt::Write as _;
            let _ = write!(acc, "{b:02x}");
            acc
        })
    );
    println!("  file         {}", path.display());
    Ok(())
}

/// This device, as one object.
///
/// The same field names `devices --json` uses for the same three facts — the
/// key a peer pins, and the fingerprint derived from it in both the form a
/// script compares and the form a person reads aloud. One vocabulary, so a
/// reader that knows the device list knows this too.
fn print_identity_as_json() -> Result<(), Box<dyn std::error::Error>> {
    use std::fmt::Write as _;

    let path = identity_path()?;
    let id = esper_crypto::Identity::load_or_create(&path)?;
    let key = id.public_key().iter().fold(String::new(), |mut acc, b| {
        let _ = write!(acc, "{b:02x}");
        acc
    });
    let out = serde_json::json!({
        "public_key": key,
        "fingerprint": {
            "hex": id.fingerprint().hex(),
            "words": id.fingerprint().words(),
        },
        "file": path.display().to_string(),
    });
    println!("{out}");
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
/// `None` only where there is genuinely nothing to say about a command on its
/// own; the general help is then the honest answer, and it is still an answer.
/// A command that takes *anything* — a flag, a subcommand, or nothing at all
/// where that is worth stating — belongs here. Without an entry, `--help`
/// answers with a list of thirty-two commands, none of which says what the one
/// that was asked about takes.
fn usage_for(command: &str) -> Option<String> {
    match command {
        "connect" => Some(session_cmd::connect_usage()),
        "cp" => Some(cp_cmd::usage()),
        "ls" => Some(ls_cmd::usage()),
        "host" => Some(session_cmd::host_usage()),
        "lock" => Some(lock_cmd::usage(lock_cmd::What::Lock)),
        "logoff" => Some(lock_cmd::usage(lock_cmd::What::Logoff)),
        "reboot" => Some(lock_cmd::usage(lock_cmd::What::Reboot)),
        "group" => Some(devices_cmd::group_usage()),
        "wake" => Some(wake_cmd::usage()),
        "screenshot" => Some(screenshot_cmd::usage()),
        "discover" => Some(discover_cmd::usage()),
        "terminal" => Some(terminal_cmd::usage()),
        "forward" => Some(forward_cmd::usage()),
        "invite" => Some(invite_cmd::usage()),
        "grant" => Some(devices_cmd::change_usage(devices_cmd::Change::Grant)),
        "revoke" => Some(devices_cmd::change_usage(devices_cmd::Change::Revoke)),
        "schedule" => Some(devices_cmd::schedule_usage()),
        "lock-after" => Some(devices_cmd::lock_after_usage()),
        "devices" => Some(devices_cmd::list_usage()),
        "version" => Some(version_usage()),
        "config" => Some(settings::usage()),
        "identity" => Some(identity_usage()),
        "input" => Some(input_usage()),
        "sandbox" => Some(sandbox_usage()),
        "pair" => Some(pairing::usage()),
        "join" => Some(invite_cmd::join_usage()),
        "keep" => Some(devices_cmd::keep_usage()),
        "forget" => Some(devices_cmd::forget_usage()),
        "serve" => Some(serve::usage()),
        _ => None,
    }
}

/// What `identity` takes.
fn identity_usage() -> String {
    let me = net::me();
    format!(
        "usage: {me} identity [--json]\n\
         \n\
         This machine's own fingerprint, in hex and in the words somebody\n\
         reads out loud to confirm it.\n\
         \n\
         options:\n  \
         \x20 --json   one object, for a script rather than a person\n"
    )
}

fn print_version() {
    println!("esper-engine {VERSION}");
    println!("  built     {BUILD_TIME}");
    println!("  protocol  {}", esper_proto::PROTOCOL_VERSION);
}

/// The same three facts, in the shape the other two binaries answer with.
///
/// One object with the same three keys, so a deployment asking all three what
/// they are gets three answers it can compare rather than three formats.
fn print_version_as_json() {
    let out = serde_json::json!({
        "version": VERSION,
        "buildTime": BUILD_TIME,
        "protocol": esper_proto::PROTOCOL_VERSION,
    });
    println!("{out}");
}

/// What `version` takes.
#[must_use]
pub fn version_usage() -> String {
    let me = crate::net::me();
    format!(
        "usage: {me} version [--json]\n\
         \n\
         The release, when it was built, and the protocol version two machines\n\
         have to agree on.\n\
         \n\
         options:\n  \
         \x20 --json   one object, for a script rather than a person\n"
    )
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
           input      which injection backend this machine can use\n  \
           sandbox    the jail a peer's video is decoded in, and what it stops\n  \
           discover   list the Esper devices on this network\n  \
           pair       pair with another device, in either direction\n  \
           config     show every setting, change one, or take one back out\n  \
           devices    list paired devices and what they may do\n  \
           group      put devices in a group, for applying a policy at once\n  \
           wake       send a magic packet to a paired device that is asleep\n  \
           screenshot one picture of a paired host's screen, as a PNG\n  \
           grant      give a paired device a permission (--yes to confirm a\n  \
           \x20          dangerous one without being asked)\n  \
           revoke     take a permission away\n  \
           schedule   when a device may connect unattended, in local time\n  \
           lock-after whether a session with a device leaves this screen locked\n  \
           keep       make a one-session pairing last\n  \
           forget     unpair a device entirely\n  \
           invite     offer somebody a way in: one link, through your broker\n  \
           join       take somebody up on an invitation\n  \
           host       serve this machine's screen to a paired device\n  \
           connect    show a paired host's screen\n  \
           lock       lock a paired host's screen, and nothing else\n  \
           logoff     end the login session on a paired host\n  \
           reboot     restart a paired host\n  \
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
