//! The channel a supervisor watches this host through.
//!
//! # Why a host needs one
//!
//! `esper host` is the one verb that runs for weeks on a machine nobody is
//! looking at, so the control plane supervises it rather than handing the
//! process over. A supervisor that can only see *whether the process exists*
//! cannot tell a working host from a wedged one — and the wedged one is the
//! failure that matters, because it goes on looking fine while nobody can
//! connect.
//!
//! # Why a socket
//!
//! Not stdio: a host prints a live report there, and pushing that through a
//! supervisor would turn a layout into a transcript. Not an inherited file
//! descriptor: taking one needs `from_raw_fd`, and this workspace forbids
//! unsafe code. So the supervisor listens on a Unix socket and this process
//! connects back to it, carrying the same length-prefixed CBOR as the `serve`
//! seam — one framing to get right rather than two.
//!
//! # Who may connect, and who may answer
//!
//! The socket lives in a directory only its owner can enter, and the
//! supervisor checks that the process connecting is the child it started.
//! Both halves are needed. The directory keeps other people out; the check
//! keeps another program belonging to the same person from answering these
//! questions on the host's behalf, and the answers are a health report and an
//! extract of the audit trail. A health check somebody else can answer is not
//! a health check.

use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

use esper_proto::{Alive, Command, Hello, Report, Role, read_frame, write_frame};

/// What this host is doing, for the supervisor to read.
///
/// Kept in a [`tokio::sync::watch`] channel rather than passed: the accept
/// loop is busy inside a session for hours at a time, and a supervisor's
/// question has to be answerable *during* one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Doing {
    /// Unix seconds when this host started listening.
    pub since: u64,
    /// Where it is listening, as resolved — `--listen 0` is listening
    /// somewhere, and this is where.
    pub listening: String,
    /// Who is connected right now, in the word form a person can compare
    /// against a device list. Empty when nobody is.
    pub serving: String,
    /// How many sessions have been served since this host started.
    pub served: u32,
}

/// Connect to the supervisor, agree on the protocol, and answer it until it
/// goes away.
///
/// The connection and the handshake happen **here**, before the caller starts
/// serving, so a channel that cannot be opened is a startup error somebody
/// sees rather than a silence discovered later. Answering happens on a thread
/// of its own, because the reads block and the runtime has a screen to capture.
#[cfg(unix)]
pub fn watch_from(
    path: &Path,
    doing: tokio::sync::watch::Receiver<Doing>,
    stopping: tokio::sync::watch::Receiver<bool>,
) -> Result<(), String> {
    let stream = std::os::unix::net::UnixStream::connect(path)
        .map_err(|e| format!("connecting to the supervisor at {}: {e}", path.display()))?;
    let mut reader = BufReader::new(
        stream
            .try_clone()
            .map_err(|e| format!("splitting the supervisor channel: {e}"))?,
    );
    let mut writer = BufWriter::new(stream);
    handshake(&mut reader, &mut writer)?;
    std::thread::Builder::new()
        .name("supervisor".to_owned())
        .spawn(move || answer_until_closed(reader, writer, doing, stopping))
        .map_err(|e| format!("starting the supervisor channel: {e}"))?;
    Ok(())
}

/// Refused rather than ignored, on a platform with no Unix sockets.
///
/// A flag that silently did nothing would leave a supervisor certain it was
/// watching a host it could not see.
#[cfg(not(unix))]
pub fn watch_from(
    _path: &Path,
    _doing: tokio::sync::watch::Receiver<Doing>,
    _stopping: tokio::sync::watch::Receiver<bool>,
) -> Result<(), String> {
    Err(
        "--watched-by has no channel on this platform yet: it is a unix socket, and \
         Windows gets a named pipe with the rest of its host"
            .to_owned(),
    )
}

/// Exchange `Hello` and refuse a peer speaking a different protocol.
///
/// The same check as the `serve` seam, on the path that had none: until this
/// existed a mixed install was discovered by whichever *other* command
/// somebody ran, and the verb that runs for weeks would restart five times and
/// give up without ever saying which half was from a different install.
fn handshake<R: Read, W: Write>(reader: &mut R, writer: &mut W) -> Result<(), String> {
    // The engine identifies as the host role with a zero device key, as the
    // `serve` seam does: the supervisor is not a peer, and a key here would be
    // one more copy of an identity for no reader.
    let mut ours = Hello::new(Role::Host, [0u8; 32], "esper-engine");
    ours.engine = esper_buildinfo::VERSION.to_owned();
    write_frame(writer, &ours).map_err(|e| format!("greeting the supervisor: {e}"))?;
    writer
        .flush()
        .map_err(|e| format!("greeting the supervisor: {e}"))?;

    let peer: Hello =
        read_frame(reader).map_err(|e| format!("reading the supervisor's greeting: {e}"))?;
    if !peer.version_matches() {
        return Err(format!(
            "protocol version mismatch: this engine speaks {}, the supervisor speaks {} — \
             the two ship together, so one of them is from a different install",
            esper_proto::PROTOCOL_VERSION,
            peer.proto
        ));
    }
    if !peer.name_is_acceptable() {
        return Err("the supervisor sent an unacceptable name".to_owned());
    }
    Ok(())
}

/// Answer questions until the supervisor closes the channel.
fn answer_until_closed<R: Read, W: Write>(
    mut reader: R,
    mut writer: W,
    doing: tokio::sync::watch::Receiver<Doing>,
    stopping: tokio::sync::watch::Receiver<bool>,
) {
    loop {
        let command = match read_frame::<Command, _>(&mut reader) {
            Ok(command) => command,
            Err(e) => {
                // A closed channel during shutdown is the supervisor going
                // away first, which is ordinary and not worth a line. At any
                // other time it means this host is now running unwatched, and
                // that is worth knowing.
                if !*stopping.borrow() {
                    eprintln!("  · the supervisor stopped listening ({e}); running unwatched");
                }
                return;
            }
        };
        let report = answer(&command, &doing);
        if write_frame(&mut writer, &report).is_err() || writer.flush().is_err() {
            return;
        }
    }
}

/// Answer one question.
fn answer(command: &Command, doing: &tokio::sync::watch::Receiver<Doing>) -> Report {
    match command {
        Command::Alive => {
            let d = doing.borrow();
            Report::Alive(Alive {
                since: d.since,
                listening: d.listening.clone(),
                serving: d.serving.clone(),
                served: d.served,
            })
        }
        // **Refused rather than answered, both of them.**
        //
        // `Status` probes: it starts an encoder and opens a capture backend to
        // find out whether they work. Doing that inside the process holding
        // the screen would disturb the machine being hosted, which is the one
        // thing a diagnostic must not do.
        //
        // `Sessions` reads the audit trail, and this channel is a socket. The
        // trail leaves this process in exactly one place — `serve`, over a
        // pipe its parent is holding — and that narrowness is a claim
        // `scripts/check-trail-stays.py` enforces. Answering here would have
        // added a second way out for a question nobody is asking: a
        // supervisor wants to know whether its host is working, not who was
        // on the machine last Tuesday.
        Command::Status | Command::Sessions { .. } => Report::Failed {
            detail: "this engine is hosting, and answers only whether it is working. \
                     Ask a `serve` engine for anything else — `esper doctor` and \
                     `esper sessions` both do."
                .to_owned(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doing() -> tokio::sync::watch::Receiver<Doing> {
        let (_tx, rx) = tokio::sync::watch::channel(Doing {
            since: 1_756_700_000,
            listening: "0.0.0.0:7645".to_owned(),
            serving: "giraffe tapestry geyser rifle jersey domain razor oboe".to_owned(),
            served: 3,
        });
        // The sender is dropped, which is exactly the case worth checking:
        // a receiver whose sender has gone still reads the last value, so an
        // answer never becomes zeroes because the loop ended first.
        rx
    }

    #[test]
    fn alive_reports_what_the_host_is_doing() {
        let Report::Alive(alive) = answer(&Command::Alive, &doing()) else {
            panic!("Alive must be answered with an Alive report");
        };
        assert_eq!(alive.served, 3);
        assert_eq!(alive.listening, "0.0.0.0:7645");
        assert!(alive.serving.starts_with("giraffe"));
    }

    #[test]
    fn a_status_probe_is_refused_with_a_reason_and_somewhere_to_ask() {
        let Report::Failed { detail } = answer(&Command::Status, &doing()) else {
            panic!("a host must refuse a status probe");
        };
        assert!(detail.contains("hosting"), "{detail}");
        assert!(
            detail.contains("serve"),
            "says where to ask instead: {detail}"
        );
    }

    /// The trail leaves this process in one place, and this is not it.
    ///
    /// A socket that could hand back an extract of the audit log would be a
    /// second way out for a question a supervisor never asks.
    #[test]
    fn the_trail_is_not_served_over_this_channel() {
        let asked = Command::Sessions { most: 50 };
        let Report::Failed { detail } = answer(&asked, &doing()) else {
            panic!("a host must not read the trail out over its supervision channel");
        };
        assert!(detail.contains("only whether it is working"), "{detail}");
    }

    #[test]
    fn a_supervisor_speaking_another_protocol_is_named_in_the_refusal() {
        let mut theirs = Hello::new(Role::Controller, [0u8; 32], "esper");
        theirs.proto = esper_proto::PROTOCOL_VERSION.wrapping_add(1);
        let mut greeting = Vec::new();
        write_frame(&mut greeting, &theirs).expect("encode");

        let mut ours = Vec::new();
        let err = handshake(&mut greeting.as_slice(), &mut ours)
            .expect_err("a version mismatch must not be accepted");
        assert!(err.contains("different install"), "{err}");
        assert!(
            err.contains(&esper_proto::PROTOCOL_VERSION.to_string()),
            "says which versions disagree: {err}"
        );
    }
}
