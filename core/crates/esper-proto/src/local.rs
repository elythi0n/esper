//! The control plane's channel to the engine.
//!
//! Not the wire protocol. These messages never leave the machine: they travel
//! over the pipe between `esper` and `esper-engine`, in the same
//! length-prefixed CBOR framing, because one implementation of framing to get
//! right is better than two.
//!
//! # What belongs here
//!
//! Commands and state, at roughly ten messages a second. **Media never crosses
//! this channel** — if a per-frame message ever appears here, the reason for
//! the process split is gone and only its cost remains.
//!
//! # Why the answers carry sentences
//!
//! Every capability answers with what it *is* rather than a boolean, because
//! the useful half of "no capture backend" is which one was looked for and
//! what it said. A control plane that could only report a false would have to
//! invent the explanation, and it does not know.

use serde::{Deserialize, Serialize};

/// What the control plane asks the engine to do.
///
/// Tagged, and it will stay tagged: a channel whose messages are distinguished
/// by what the reader expects next is a channel that desynchronises the first
/// time somebody adds a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    /// What are you, and what can this machine do?
    ///
    /// Answered with [`Report::Status`]. The engine **probes** rather than
    /// reporting what it was built with: an encoder that is compiled in and
    /// cannot start is the failure this exists to find.
    Status,
}

/// What the engine says back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Report {
    /// The answer to [`Command::Status`].
    Status(Status),
    /// The command could not be carried out, and why.
    ///
    /// A sentence rather than a code: these are read by a person running
    /// `esper doctor`, and there is nothing on the other side that branches
    /// on them.
    Failed { detail: String },
}

/// What one thing on this machine can do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capability {
    /// Whether it works **now**, established by trying it where trying is
    /// cheap.
    pub available: bool,
    /// What was found, or why nothing was. Shown to a person; never parsed.
    pub detail: String,
}

impl Capability {
    /// Something that works, and what it is.
    #[must_use]
    pub fn working(detail: impl Into<String>) -> Self {
        Self {
            available: true,
            detail: detail.into(),
        }
    }

    /// Something that does not, and why.
    #[must_use]
    pub fn missing(detail: impl Into<String>) -> Self {
        Self {
            available: false,
            detail: detail.into(),
        }
    }
}

/// Everything `esper doctor` needs to say what this machine can do.
///
/// One message rather than a question per capability: the answers are wanted
/// together, they are cheap to gather, and a round trip per line would make
/// the seam's shape visible in how long the command takes.
///
/// **Presence is deliberately absent.** Whether this machine can show that it
/// is being watched is worth knowing and cannot be established without
/// starting the indicator, which puts a window on somebody's screen and takes
/// a global hotkey. A diagnostic must not disturb the machine it is
/// diagnosing, so that answer waits for a probe that does not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    /// The engine's release, as it was built.
    pub engine: String,
    /// How this machine can capture a screen.
    pub capture: Capability,
    /// How it can encode one, and whether that is hardware.
    pub encoder: Capability,
    /// How it can inject input.
    pub input: Capability,
    /// Networks the person already runs, by interface name. Empty is ordinary.
    pub overlays: Vec<String>,
    /// Whether this machine announces itself on the local network.
    pub mdns: bool,
    /// The broker this machine would use, if one is configured. **No default
    /// exists anywhere in Esper**, so `None` is the ordinary answer.
    pub broker: Option<String>,
    /// How many devices are paired.
    pub paired: u32,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn status() -> Status {
        Status {
            engine: "0.0.0".to_owned(),
            capture: Capability::working("x11"),
            encoder: Capability::working("openh264, software"),
            input: Capability::missing("no display"),
            overlays: vec!["wg0".to_owned()],
            mdns: true,
            broker: None,
            paired: 2,
        }
    }

    #[test]
    fn a_status_survives_the_wire() {
        let out = crate::encode(&Report::Status(status())).expect("encode");
        let back: Report = crate::decode(&out).expect("decode");
        assert_eq!(back, Report::Status(status()));
    }

    #[test]
    fn a_failure_survives_the_wire() {
        let out = crate::encode(&Report::Failed {
            detail: "no display".to_owned(),
        })
        .expect("encode");
        let back: Report = crate::decode(&out).expect("decode");
        assert!(matches!(back, Report::Failed { .. }));
    }

    #[test]
    fn a_command_survives_the_wire() {
        let out = crate::encode(&Command::Status).expect("encode");
        assert_eq!(
            crate::decode::<Command>(&out).expect("decode"),
            Command::Status
        );
    }

    /// What the tags do and do not separate.
    ///
    /// The two directions travel on the same pipe and are read by different
    /// processes, each expecting its own type — that is what keeps them apart,
    /// and it is worth being exact about it because the tag alone does not.
    ///
    /// An externally tagged enum represents a unit variant as a bare string
    /// and a struct variant as a one-key map. So a **command does not decode
    /// as a report** — no report is a unit variant — but `Report::Status(..)`
    /// decodes as `Command::Status` quite happily, because serde reads the key
    /// and stops. Anything relying on the shape of a message to know which way
    /// it was going would be relying on that.
    #[test]
    fn a_command_does_not_decode_as_a_report() {
        let command = crate::encode(&Command::Status).expect("encode");
        assert!(crate::decode::<Report>(&command).is_err());

        let failed = crate::encode(&Report::Failed {
            detail: "no".to_owned(),
        })
        .expect("encode");
        assert!(crate::decode::<Command>(&failed).is_err());
    }
}
