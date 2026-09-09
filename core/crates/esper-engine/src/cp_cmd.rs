//! `esper cp` — moving a file from a terminal, with no window in the path.
//!
//! # Why this comes before any of the interface
//!
//! The fleet this product is for is headless. The machines where moving a file
//! matters most are the ones with no screen to drag onto, and a transfer
//! feature that arrives interface-first is one that does not work where it is
//! needed. Retrofitting a command onto a protocol shaped around a window is
//! how that becomes permanent.
//!
//! It is `scp`-shaped because that shape is in people's fingers, and it is
//! safe with no terminal attached: everything it says goes to standard error,
//! so a script's standard output is its own.
//!
//! # The local end gets the same treatment as the far one
//!
//! A file landing here goes through the same name rules, the same partial, the
//! same hash and the same one-step rename as a file landing there. Not for
//! symmetry: **a copy that is interrupted must not leave something under the
//! name a build script is watching for**, and that is as true on a laptop as
//! on a server.

use std::path::{Path, PathBuf};

use esper_files::{Limits, Offer, Receiving, SafePath, Sending, Vault};
use esper_proto::{FileAccept, FileMessage, FileRefusal, FileWant, Permission};
use esper_transport::Control;

/// One end of a copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Side {
    /// A path on this machine.
    Here(PathBuf),
    /// A device, and a path inside its file directory.
    There { device: String, path: String },
}

/// What `esper cp` was asked to do.
#[derive(Debug)]
pub struct CopyOptions {
    pub from: Side,
    pub to: Side,
    /// Replace what is there. **Off unless said**, because a copy that eats
    /// work is a copy people stop trusting.
    pub force: bool,
    pub via: Option<esper_rendezvous::Via>,
    pub mdns: bool,
    /// Report progress in lines a program can read, rather than a bar.
    ///
    /// **For a parent process, not for a person.** The bar is drawn only onto
    /// a terminal, so anything that starts `cp` with a pipe — the window's
    /// drop, a script with a progress display of its own — sees nothing at all
    /// between "started" and "finished". This asks for one line per report
    /// instead, which survives a pipe.
    pub plain_progress: bool,
}

/// Split `device:path` from a plain local path.
///
/// **The last colon, not the first.** A device can be named by an address, and
/// an address has a colon in it — `127.0.0.1:7645:reports/x` is a real thing
/// to type while a host is being set up. Splitting at the last one works for
/// that and for `elythion:reports/x` alike, and costs nothing on the path side:
/// a colon is a character no path this carries may contain, because it is a
/// separator on Windows.
///
/// A single letter before it is a Windows drive rather than a device: `C:\\x`
/// is somewhere on this machine. A device named with one letter is a device
/// that has to be typed with more.
fn read_side(text: &str) -> Side {
    if let Some((device, path)) = text.rsplit_once(':')
        && device.chars().count() > 1
        && !device.contains('/')
        && !device.contains('\\')
    {
        return Side::There {
            device: device.to_owned(),
            path: path.to_owned(),
        };
    }
    Side::Here(PathBuf::from(text))
}

/// Read the command line.
///
/// # Errors
///
/// With the usage, when it is not one copy between one local and one remote.
pub fn parse(args: &[String]) -> Result<CopyOptions, String> {
    let mut plain: Vec<String> = Vec::new();
    let mut force = false;
    let mut via_addr = None;
    let mut via_key = None;
    let mut mdns = crate::settings::flag("discovery.mdns", true);
    let mut plain_progress = false;
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        match arg.as_str() {
            "--force" | "-f" => {
                force = true;
                i += 1;
            }
            "--no-mdns" => {
                mdns = false;
                i += 1;
            }
            "--progress" => {
                plain_progress = true;
                i += 1;
            }
            "--via" => {
                let text = args
                    .get(i + 1)
                    .ok_or_else(|| "--via needs a broker address".to_owned())?;
                via_addr = Some(crate::net::parse_addr(text)?);
                i += 2;
            }
            "--broker-key" => {
                let text = args
                    .get(i + 1)
                    .ok_or_else(|| "--broker-key needs a key".to_owned())?;
                via_key = Some(crate::broker_cmd::parse_key(text)?);
                i += 2;
            }
            other if other.starts_with('-') && other != "-" => {
                return Err(format!("unknown option {other:?}\n{}", usage()));
            }
            other => {
                plain.push(other.to_owned());
                i += 1;
            }
        }
    }
    let [from, to] = plain.as_slice() else {
        return Err(format!(
            "cp takes exactly one source and one destination\n{}",
            usage()
        ));
    };
    let (from, to) = (read_side(from), read_side(to));

    // **One end here and one end there.** Two local paths is what `cp` is for,
    // and two remote ones would be this machine relaying a copy between two
    // others — which is a different feature with a different permission.
    match (&from, &to) {
        (Side::Here(_), Side::There { .. }) | (Side::There { .. }, Side::Here(_)) => {}
        (Side::Here(_), Side::Here(_)) => {
            return Err(format!(
                "both of those are on this machine; that is what `cp` is for\n{}",
                usage()
            ));
        }
        (Side::There { .. }, Side::There { .. }) => {
            return Err(format!(
                "both of those are on other machines; copy through this one in two steps\n{}",
                usage()
            ));
        }
    }
    let via = match crate::session_cmd::broker_from(via_addr, via_key)? {
        Some(via) => Some(via),
        None => crate::settings::broker()?,
    };
    Ok(CopyOptions {
        from,
        to,
        force,
        via,
        mdns,
        plain_progress,
    })
}

/// What `esper cp` takes.
pub fn usage() -> String {
    let me = crate::net::me();
    format!(
        "usage: {me} cp <source> <destination>\n\
         \n\
         Copy one file to or from a paired host. One end is a path on this\n\
         machine; the other is <device>:<path>, relative to that machine's\n\
         file directory.\n\
         \n\
         \x20 --force              replace what is already there (never silent)\n\
         \x20 --via <broker>       be introduced by a broker when nothing direct works\n\
         \x20 --broker-key <key>   the broker's key, when it is not remembered\n\
         \x20 --no-mdns            do not ask the local network where the host is\n\
         \x20 --progress           report progress in lines, for a program to read\n\
         \n\
         for example: {me} cp report.pdf elythion:reports/\n\
         \x20            {me} cp elythion:logs/build.log ./"
    )
}

/// A local path, as a directory to work in and a name inside it.
///
/// **The directory is resolved once, here.** A person's own `~/work` is very
/// often a link to somewhere else, and refusing to write through it would be
/// refusing to work on the machine they are sitting at. Resolving it at the
/// moment they name it is not a hole: it is the path they typed, resolved when
/// they typed it. What is *not* resolved afterwards is anything under it.
fn local(at: &Path, name_from: Option<&str>) -> Result<(Vault, SafePath), String> {
    let (dir, name) = if at.is_dir() || at.as_os_str().is_empty() {
        let name = name_from.ok_or_else(|| {
            format!(
                "{} is a directory; say what to call the file in it",
                at.display()
            )
        })?;
        (at.to_path_buf(), name.to_owned())
    } else {
        let dir = at.parent().unwrap_or(Path::new(".")).to_path_buf();
        let name = at
            .file_name()
            .ok_or_else(|| format!("{} does not name a file", at.display()))?
            .to_string_lossy()
            .into_owned();
        (
            if dir.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                dir
            },
            name,
        )
    };
    let real = std::fs::canonicalize(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let vault = Vault::open(&real).map_err(|e| e.to_string())?;
    let path = SafePath::parse(&name).map_err(|why| {
        format!("{name}: {why} — a name has to be one every machine in a fleet can hold")
    })?;
    Ok((vault, path))
}

/// The last component of a remote path, for filling in a local directory.
fn tail(path: &str) -> Option<&str> {
    path.rsplit('/').find(|part| !part.is_empty())
}

/// Do the copy.
///
/// # Errors
///
/// With a sentence for the person who typed it.
pub fn run(o: &CopyOptions) -> Result<(), String> {
    let identity = crate::paths::identity()?;
    let (device, remote_path, pushing) = match (&o.from, &o.to) {
        (Side::Here(_), Side::There { device, path }) => (device.clone(), path.clone(), true),
        (Side::There { device, path }, Side::Here(_)) => (device.clone(), path.clone(), false),
        _ => return Err("one end has to be on another machine".to_owned()),
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("could not start: {e}"))?;

    runtime.block_on(async move {
        // A device can be named, or named by where it is. An address says
        // where without saying which; the key check settles the identity
        // either way, exactly as it does for `connect`.
        let at = crate::net::parse_addr(&device).ok();
        let reached = crate::session_cmd::reach(
            &identity,
            &crate::session_cmd::Reaching {
                label: "📄 esper cp",
                addr: at,
                named: if at.is_some() { None } else { Some(&device) },
                via: o.via,
                mdns: o.mdns,
                // Everything goes to standard error, so a script's standard
                // output is its own.
                aside: true,
            },
        )
        .await?;
        let crate::session_cmd::Reached {
            endpoint,
            mut session,
            ..
        } = reached;

        let (media, mut control) = esper_link::parts(&mut session).map_err(|e| e.to_string())?;
        let want = if pushing {
            Permission::FilesIn
        } else {
            Permission::FilesOut
        };
        let grant = esper_transport::request_session(
            &mut control,
            &esper_proto::SessionRequest {
                kind: esper_proto::SessionKind::Files,
                want: vec![want],
                codecs: Vec::new(),
                reason: "a file".to_owned(),
            },
        )
        .await
        .map_err(|e| match e {
            esper_transport::AuthorizeError::Refused(d) => {
                format!("the host refused: {} ({})", d.detail, d.reason.0)
            }
            other => format!("agreeing what the session may do: {other}"),
        })?;
        if !grant.allows(want) {
            return Err(format!(
                "that host has not granted this device {}",
                want.name()
            ));
        }

        let mut stream = session
            .open_control()
            .await
            .map_err(|e| format!("could not open a file stream: {e}"))?;
        stream
            .send(&esper_proto::StreamPurpose::Files)
            .await
            .map_err(|e| format!("could not say what the stream was for: {e}"))?;

        let outcome = if pushing {
            send_one(&mut stream, o, &remote_path).await
        } else {
            fetch_one(&mut stream, o, &remote_path).await
        };

        // **Recorded whether it worked or not**, the same rule the host's own
        // side follows: a device repeatedly asking for paths it cannot have is
        // a pattern, and only the refusals show it. The path is the remote one
        // as this end named it, never a byte of what crossed.
        crate::trail::went_out(
            session.peer_key(),
            esper_audit::Event::FileTransferred {
                way: if pushing { "out" } else { "in" }.to_owned(),
                path: remote_path.clone(),
                size: outcome.as_ref().copied().unwrap_or(0),
                outcome: match &outcome {
                    Ok(_) => "copied".to_owned(),
                    Err(why) => format!("refused: {why}"),
                },
            },
        );

        media.close(esper_proto::Reason::SESSION_COMPLETE);
        media.linger(std::time::Duration::from_millis(200)).await;
        drop(endpoint);
        outcome.map(|_| ())
    })
}

/// Push a local file to the host.
/// Returns the file's size, so the trail records the real number rather than
/// a plausible one.
async fn send_one(stream: &mut Control, o: &CopyOptions, remote: &str) -> Result<u64, String> {
    let Side::Here(at) = &o.from else {
        return Err("nothing local to send".to_owned());
    };
    let (vault, name) = local(at, None)?;
    let mut sending = Sending::begin(&vault, &name, Limits::default())
        .map_err(|why| format!("{}: {}", at.display(), why.message()))?;

    // A destination that names a directory takes the source's own name.
    let destination = if remote.is_empty() || remote.ends_with('/') {
        format!("{remote}{}", name.file_name())
    } else {
        remote.to_owned()
    };
    let mut offer = sending.offer().to_wire();
    offer.path = destination.clone();
    offer.replace = o.force;
    // Read before the offer is handed over, so the trail's number is the one
    // that was actually sent.
    let size = offer.bytes;
    eprintln!(
        "  sending    {} → {destination} ({} bytes)",
        at.display(),
        offer.bytes
    );

    stream
        .send(&FileMessage::Offer(offer))
        .await
        .map_err(|e| e.to_string())?;
    let from = match stream.recv::<FileMessage>().await {
        Ok(FileMessage::Accept(accept)) => accept.from,
        Ok(FileMessage::Refuse(why)) => return Err(advice(why, &destination)),
        Ok(other) => return Err(format!("the host answered with {}", other.name())),
        Err(e) => return Err(format!("the host said nothing: {e}")),
    };
    if from > 0 {
        eprintln!("  resuming   from {from} bytes in");
    }
    sending.resume_at(from).map_err(|e| e.to_string())?;

    // **A figure that moves, and a way to stop.** With one line at the start
    // and one at the end, the only way to tell a ten-gigabyte copy that is
    // slow from one that is stuck is to watch the network.
    let progress = Progress::new("sending", o.plain_progress);
    let stop = crate::files_task::stop_on_interrupt();
    let mut report = |sent: u64, total: u64| {
        progress.say(sent, total);
        if stop.load(std::sync::atomic::Ordering::Relaxed) {
            crate::files_task::Going::Stop
        } else {
            crate::files_task::Going::On
        }
    };
    let pushed = crate::files_task::push_reporting(stream, &mut sending, &mut report).await;
    progress.done();
    pushed?;

    match stream.recv::<FileMessage>().await {
        Ok(FileMessage::Done(end)) if end.is_stored() => {
            eprintln!("  sent       {destination}");
            Ok(size)
        }
        Ok(FileMessage::Done(end)) => Err(end.message().to_owned()),
        Ok(other) => Err(format!("the host answered with {}", other.name())),
        Err(e) => Err(format!("the host did not say how it went: {e}")),
    }
}

/// Pull a file from the host to here.
/// Returns the file's size, for the same reason [`send_one`] does.
async fn fetch_one(stream: &mut Control, o: &CopyOptions, remote: &str) -> Result<u64, String> {
    let Side::Here(at) = &o.to else {
        return Err("nowhere local to put it".to_owned());
    };
    stream
        .send(&FileMessage::Want(FileWant {
            path: remote.to_owned(),
        }))
        .await
        .map_err(|e| e.to_string())?;

    let wire = match stream.recv::<FileMessage>().await {
        Ok(FileMessage::Have(offer)) => offer,
        Ok(FileMessage::Refuse(why)) => return Err(advice(why, remote)),
        Ok(other) => return Err(format!("the host answered with {}", other.name())),
        Err(e) => return Err(format!("the host said nothing: {e}")),
    };

    // **The name comes from what was asked for, never from the answer.** A
    // host replies with a path of its own, and a receiver that took its word
    // would let the far end choose the name of a file on this machine — `.`
    // something, in whichever directory the person happened to be standing in.
    // What lands here is named after what they typed.
    let (vault, name) = local(at, tail(remote))?;
    let mut offer = Offer::check(&wire).map_err(|why| why.message().to_owned())?;
    offer.path = name.clone();
    offer.replace = o.force;
    eprintln!(
        "  fetching   {remote} → {} ({} bytes)",
        vault.shown().join(name.as_str()).display(),
        offer.bytes
    );

    let mut receiving = match Receiving::begin(&vault, &offer, Limits::default()) {
        Ok(receiving) => receiving,
        Err(why) => {
            let _ = stream.send(&FileMessage::Refuse(why)).await;
            return Err(advice(why, name.as_str()));
        }
    };
    let from = receiving.from();
    if from > 0 {
        eprintln!("  resuming   from {from} bytes in");
    }
    stream
        .send(&FileMessage::Accept(FileAccept { from }))
        .await
        .map_err(|e| e.to_string())?;

    // The same figure the sending end draws, at the same interval, so a fetch
    // of a large file is not a command that prints one line and then looks
    // stuck for an hour.
    let progress = Progress::new("fetching", o.plain_progress);
    let mut buffer = vec![0u8; esper_files::AT_A_TIME];
    let mut got_so_far = from;
    let mut last_said = from;
    while receiving.left() > 0 {
        let got = match stream.recv_raw(&mut buffer).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                eprintln!("  · it stopped part way ({e})");
                break;
            }
        };
        receiving
            .take(buffer.get(..got).unwrap_or_default())
            .map_err(|e| e.to_string())?;
        got_so_far = got_so_far.saturating_add(got as u64);
        if got_so_far.saturating_sub(last_said) >= crate::files_task::REPORT_EVERY {
            last_said = got_so_far;
            progress.say(got_so_far, offer.bytes);
        }
    }
    progress.say(got_so_far, offer.bytes);
    progress.done();
    let end = receiving.finish(&vault);
    let _ = stream.send(&FileMessage::Done(end)).await;
    if end.is_stored() {
        eprintln!(
            "  fetched    {}",
            vault.shown().join(name.as_str()).display()
        );
        return Ok(offer.bytes);
    }
    Err(end.message().to_owned())
}

/// How a copy says how far it has got.
///
/// **A bar for a person, a line for a program.** The bar is drawn with
/// carriage returns onto a terminal, which is unreadable in a log and
/// invisible through a pipe — so anything that starts `cp` as a child process
/// hears nothing at all between "started" and "finished". `--progress` asks
/// for one plain line per report instead, which survives a pipe.
struct Progress {
    /// What this end is doing, for the line a person reads.
    word: &'static str,
    plain: bool,
    bar: bool,
}

impl Progress {
    fn new(word: &'static str, plain: bool) -> Self {
        Self {
            word,
            plain,
            bar: !plain && std::io::IsTerminal::is_terminal(&std::io::stderr()),
        }
    }

    /// What to print for this report, or nothing when there is nothing to say.
    ///
    /// Separate from printing it so a test can ask what a pipe would be shown
    /// without being one: the whole point of `--progress` is the case where
    /// the bar prints nothing, and that case is invisible from inside a test
    /// that captures stderr.
    fn line(&self, done: u64, total: u64) -> Option<String> {
        if self.plain {
            return Some(format!("  progress   {done} of {total} bytes"));
        }
        if self.bar {
            return Some(format!(
                "\r  {:<10} {}",
                self.word,
                progress_bar(done, total)
            ));
        }
        None
    }

    fn say(&self, done: u64, total: u64) {
        let Some(line) = self.line(done, total) else {
            return;
        };
        if self.plain {
            eprintln!("{line}");
        } else {
            eprint!("{line}");
            let _ = std::io::Write::flush(&mut std::io::stderr());
        }
    }

    /// Finish the line the bar has been rewriting.
    fn done(&self) {
        if self.bar {
            eprintln!();
        }
    }
}

/// A refusal, with what to do about it.
///
/// **The prompt happens where the person is.** A receiver that found something
/// already there refuses rather than asking, because an unattended host has
/// nobody to ask — so the asking is here, at the end that typed the command.
fn advice(why: FileRefusal, what: &str) -> String {
    match why {
        FileRefusal::Exists => {
            format!("{what} is already there — pass --force to replace it")
        }
        FileRefusal::NotPermitted => format!(
            "{} — ask the host's owner to grant files-in or files-out",
            why.message()
        ),
        FileRefusal::NotEnabled => format!(
            "{} — its owner sets files.enabled to switch it on",
            why.message()
        ),
        other => other.message().to_owned(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn parse_args(args: &[&str]) -> Result<CopyOptions, String> {
        parse(&args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>())
    }

    /// **The flag, and everything without it.** `--progress` must not change
    /// what a copy does, only what it says.
    #[test]
    fn progress_is_off_unless_asked_for() {
        assert!(
            !parse_args(&["a.txt", "elythion:b.txt"])
                .expect("a copy")
                .plain_progress
        );
        let asked = parse_args(&["--progress", "a.txt", "elythion:b.txt"]).expect("a copy");
        assert!(asked.plain_progress);
        assert!(!asked.force, "--progress changed something else");
        let after = parse_args(&["a.txt", "elythion:b.txt", "--progress"]).expect("a copy");
        assert!(after.plain_progress, "only accepted before the paths");
    }

    /// **A drive letter is not a device.** `C:\\x` is somewhere on this
    /// machine, and a rule that read it as a host called `C` would make the
    /// command unusable on Windows in the least obvious way.
    #[test]
    fn a_local_path_and_a_remote_one_are_told_apart() {
        assert_eq!(read_side("report.pdf"), Side::Here("report.pdf".into()));
        assert_eq!(read_side("./a/b"), Side::Here("./a/b".into()));
        assert_eq!(read_side("/tmp/x"), Side::Here("/tmp/x".into()));
        assert_eq!(read_side("C:\\Users\\x"), Side::Here("C:\\Users\\x".into()));
        assert_eq!(read_side("c:/users/x"), Side::Here("c:/users/x".into()));
        // A path with a colon in a component is still local.
        assert_eq!(read_side("./odd:name"), Side::Here("./odd:name".into()));
        assert_eq!(
            read_side("elythion:reports/q3.pdf"),
            Side::There {
                device: "elythion".to_owned(),
                path: "reports/q3.pdf".to_owned()
            }
        );
        // A fingerprint prefix works the same way.
        assert_eq!(
            read_side("91800b2a:x"),
            Side::There {
                device: "91800b2a".to_owned(),
                path: "x".to_owned()
            }
        );
        // **An address is a way of naming a device**, and an address has a
        // colon in it. The last one is the separator.
        assert_eq!(
            read_side("127.0.0.1:7645:reports/q3.pdf"),
            Side::There {
                device: "127.0.0.1:7645".to_owned(),
                path: "reports/q3.pdf".to_owned()
            }
        );
        assert_eq!(
            read_side("[::1]:7645:x"),
            Side::There {
                device: "[::1]:7645".to_owned(),
                path: "x".to_owned()
            }
        );
    }

    #[test]
    fn one_end_here_and_one_end_there_or_it_is_not_a_copy() {
        assert!(parse_args(&["a.txt", "elythion:b.txt"]).is_ok());
        assert!(parse_args(&["elythion:b.txt", "a.txt"]).is_ok());
        // Two local paths is what `cp` is for.
        let both_here = parse_args(&["a.txt", "b.txt"]).expect_err("refused");
        assert!(both_here.contains("this machine"), "{both_here}");
        // Two remote ones would be this machine relaying between two others.
        let both_there = parse_args(&["elythion:a", "plum:b"]).expect_err("refused");
        assert!(both_there.contains("two steps"), "{both_there}");
        assert!(parse_args(&["only-one"]).is_err());
        assert!(parse_args(&[]).is_err());
        assert!(parse_args(&["a", "elythion:b", "c"]).is_err());
    }

    /// **Replacing is a decision somebody made.**
    #[test]
    fn nothing_is_replaced_unless_it_was_asked_for() {
        assert!(!parse_args(&["a", "elythion:b"]).expect("a copy").force);
        assert!(
            parse_args(&["--force", "a", "elythion:b"])
                .expect("a copy")
                .force
        );
        assert!(parse_args(&["a", "elythion:b", "-f"]).expect("a copy").force);
        // And the refusal says how, rather than only that.
        let said = advice(FileRefusal::Exists, "reports/q3.pdf");
        assert!(said.contains("--force"), "{said}");
        assert!(said.contains("reports/q3.pdf"), "{said}");
    }

    #[test]
    fn an_unknown_flag_is_refused_with_the_usage() {
        let e = parse_args(&["--recursive", "a", "elythion:b"]).expect_err("refused");
        assert!(e.contains("unknown option \"--recursive\""), "{e}");
        assert!(e.contains("usage:"), "{e}");
        // A bare `-` is a filename, not a flag.
        assert!(parse_args(&["-", "elythion:b"]).is_ok());
    }

    #[test]
    fn a_directory_destination_takes_the_name_of_what_is_being_copied() {
        assert_eq!(tail("reports/q3.pdf"), Some("q3.pdf"));
        assert_eq!(tail("q3.pdf"), Some("q3.pdf"));
        assert_eq!(tail("reports/"), Some("reports"));
        assert_eq!(tail(""), None);
    }

    /// **A name a person typed is a name a person typed.** The only thing
    /// `tail` is ever given is the path from the command line, so a host
    /// answering a request for `logs/x.log` with a path of its own choosing
    /// cannot decide what appears in somebody's working directory.
    #[test]
    fn the_far_end_does_not_get_to_name_a_file_on_this_machine() {
        // Assembled, or this test's own text is the thing it finds.
        let derived_from_the_reply = format!("tail(&wire{}path)", ".");
        let source = include_str!("cp_cmd.rs");
        assert!(
            !source.contains(&derived_from_the_reply),
            "the local name is being derived from what the far end replied"
        );
        // And even if it were, the tail of a path is a single component that
        // then has to pass the name rules.
        assert_eq!(tail("../../.bashrc"), Some(".bashrc"));
        assert!(SafePath::parse("..").is_err());
    }

    /// Every option the parser takes is in the usage, and everything the usage
    /// promises the parser takes. Written from memory once and got wrong once.
    #[test]
    fn the_usage_and_the_parser_agree() {
        let source = include_str!("cp_cmd.rs");
        let listed = usage();
        let arms: Vec<&str> = source
            .lines()
            .filter_map(|l| l.trim().strip_prefix('"'))
            .filter_map(|l| l.split('"').next())
            .filter(|l| l.starts_with("--"))
            .collect();
        for flag in &arms {
            assert!(
                listed.contains(flag),
                "the parser takes {flag} and the usage does not mention it"
            );
        }
        for word in listed.split_whitespace() {
            let flag = word.trim_matches(|c: char| !c.is_ascii_graphic());
            if flag.starts_with("--") {
                assert!(
                    arms.contains(&flag) || flag == "--force",
                    "the usage promises {flag} and the parser does not take it"
                );
            }
        }
    }
}

#[cfg(test)]
mod progress_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::{Progress, progress_bar};

    /// **The case a terminal cannot see.** `--progress` exists because the bar
    /// is drawn only onto a terminal, so a child process started with a pipe
    /// is told nothing at all. Asking the reporter what it would print, rather
    /// than capturing stderr, is the only way to check the silent case.
    #[test]
    fn a_pipe_hears_nothing_unless_progress_is_asked_for() {
        // What `cp` does today when a script runs it: no bar, no lines.
        let quiet = Progress {
            word: "sending",
            plain: false,
            bar: false,
        };
        assert_eq!(quiet.line(500, 1000), None);

        // What the window and `--progress` get instead. The figures are the
        // whole point, so both of them are in the line.
        let asked = Progress {
            word: "sending",
            plain: true,
            bar: false,
        };
        let said = asked.line(500, 1000).expect("a line");
        assert!(said.contains("500 of 1000 bytes"), "{said}");
        // One line per report, so a reader can split on newlines.
        assert!(!said.contains('\n'), "{said}");
        assert!(!said.contains('\r'), "{said}");
    }

    /// **The bar still looks the same.** Two callers share one reporter now,
    /// and the padding is what lines a fetch up under a send.
    #[test]
    fn the_bar_is_unchanged_and_both_words_line_up() {
        let sending = Progress {
            word: "sending",
            plain: false,
            bar: true,
        };
        let fetching = Progress {
            word: "fetching",
            plain: false,
            bar: true,
        };
        let a = sending.line(500, 1000).expect("a bar");
        let b = fetching.line(500, 1000).expect("a bar");
        assert!(a.starts_with("\r  sending    ["), "{a:?}");
        assert!(b.starts_with("\r  fetching   ["), "{b:?}");
        assert_eq!(
            a.find('['),
            b.find('['),
            "the bars start in different columns: {a:?} {b:?}"
        );
    }

    /// **The two ends, and the middle.** A bar is the kind of thing that is
    /// obviously right and divides by zero on an empty file.
    #[test]
    fn a_bar_covers_both_ends_and_an_empty_file() {
        assert!(progress_bar(0, 1000).contains("  0%"));
        assert!(progress_bar(500, 1000).contains(" 50%"));
        assert!(progress_bar(1000, 1000).contains("100%"));
        // An empty file is finished the moment it starts, and dividing by its
        // size is the only way this could panic.
        assert!(progress_bar(0, 0).contains("100%"));
    }

    /// **It never says more than all of it**, however the numbers arrive. A
    /// resumed transfer counts bytes it did not send this time, and a bar
    /// that read 130% would be the first thing anybody noticed.
    #[test]
    fn it_never_reports_more_than_the_whole_file() {
        let over = progress_bar(1300, 1000);
        assert!(over.contains("100%"), "{over}");
        // And the bar itself does not grow past its width.
        let bars = over.chars().filter(|c| *c == '=').count();
        assert!(bars <= 24, "the bar was {bars} wide");
    }

    /// Every line is the same length, or the display jitters as it counts.
    #[test]
    fn the_bar_is_a_fixed_width() {
        let a = progress_bar(1, 1_000_000);
        let b = progress_bar(999_999, 1_000_000);
        let width = |s: &str| s.chars().take_while(|c| *c != ']').count();
        assert_eq!(width(&a), width(&b));
    }
}

/// One line of progress: a bar, a percentage and the bytes.
///
/// Fixed width so the line does not jitter as the numbers grow, and drawn
/// with a carriage return by the caller rather than a newline.
fn progress_bar(sent: u64, total: u64) -> String {
    const WIDE: u64 = 24;
    // A total of zero is an empty file, which is finished the moment it
    // starts — and dividing by it would be the only way this could panic.
    let filled = if total == 0 {
        WIDE
    } else {
        sent.saturating_mul(WIDE) / total.max(1)
    };
    let filled = filled.min(WIDE) as usize;
    let percent = if total == 0 {
        100
    } else {
        sent.saturating_mul(100) / total.max(1)
    };
    format!(
        "[{}{}] {:>3}%  {} of {} bytes",
        "=".repeat(filled),
        " ".repeat(WIDE as usize - filled),
        percent.min(100),
        sent,
        total
    )
}
