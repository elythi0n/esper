//! Every setting there is.
//!
//! One table, and it is the only place a setting is declared: its name, its
//! shape, its default and the sentence explaining it. `config show` reads this
//! table, the file parser validates against it, the environment is derived
//! from it, and the "did you mean" suggestion searches it — so a setting
//! cannot exist in one of those and not the others.
//!
//! # The defaults are the secure set
//!
//! Where a default is a security decision it is the strict one, and the
//! comment says which decision it is. There is a test asserting each of them,
//! because a default that quietly relaxes is the failure mode this whole file
//! exists to prevent.

use std::borrow::Cow;

use crate::value::Value;

/// What shape a setting's value has, and what counts as a good one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Free text.
    Text,
    /// True or false.
    Flag,
    /// A whole number, within these bounds.
    Whole { least: u64, most: u64 },
    /// A number of cores, which may have a fraction.
    Cores,
    /// An address to bind or dial: `host:port`.
    Address,
    /// A device key, as sixty-four hex characters.
    Key,
    /// One of these words.
    Choice(&'static [&'static str]),
    /// A length of time: `30m`, `2h`, `90s`, or `none`.
    Time,
    /// A whole number or the word `none`.
    WholeOrNone { least: u64, most: u64 },
}

impl Kind {
    /// Read a raw value, or say what is wrong with it.
    pub fn read(self, raw: &str) -> Result<Value, String> {
        let text = raw.trim();
        match self {
            Self::Text => Ok(Value::owned_text(text)),
            Self::Flag => match text.to_ascii_lowercase().as_str() {
                "true" | "yes" | "on" | "1" => Ok(Value::Flag(true)),
                "false" | "no" | "off" | "0" => Ok(Value::Flag(false)),
                _ => Err("is not true or false".to_owned()),
            },
            Self::Whole { least, most } => {
                let n: u64 = text
                    .parse()
                    .map_err(|_| "is not a whole number".to_owned())?;
                if n < least || n > most {
                    return Err(format!("is outside {least} to {most}"));
                }
                Ok(Value::Whole(n))
            }
            Self::WholeOrNone { least, most } => {
                if text.eq_ignore_ascii_case("none") {
                    return Ok(Value::Nothing);
                }
                Self::Whole { least, most }.read(text)
            }
            Self::Cores => {
                let x: f64 = text.parse().map_err(|_| "is not a number".to_owned())?;
                if !x.is_finite() || x <= 0.0 || x > 1024.0 {
                    return Err("is not a plausible number of cores".to_owned());
                }
                Ok(Value::Fraction(x))
            }
            Self::Address => {
                if text.parse::<std::net::SocketAddr>().is_err() {
                    return Err("is not an address, which is `host:port`".to_owned());
                }
                Ok(Value::owned_text(text))
            }
            Self::Key => {
                if text.len() != 64 || !text.chars().all(|c| c.is_ascii_hexdigit()) {
                    return Err("is not a key, which is sixty-four hex characters".to_owned());
                }
                Ok(Value::owned_text(text.to_ascii_lowercase()))
            }
            Self::Choice(allowed) => {
                let lowered = text.to_ascii_lowercase();
                if allowed.contains(&lowered.as_str()) {
                    Ok(Value::owned_text(lowered))
                } else {
                    Err(format!("is not one of: {}", allowed.join(", ")))
                }
            }
            Self::Time => {
                if text.eq_ignore_ascii_case("none") || text == "0" {
                    return Ok(Value::Nothing);
                }
                let seconds = read_time(text)?;
                Ok(Value::Whole(seconds))
            }
        }
    }

    /// How to write one of these, for the file's own comments.
    #[must_use]
    pub fn shape(self) -> String {
        match self {
            Self::Text => "text".to_owned(),
            Self::Flag => "true or false".to_owned(),
            Self::Whole { least, most } => format!("a whole number, {least} to {most}"),
            Self::WholeOrNone { least, most } => {
                format!("a whole number, {least} to {most}, or none")
            }
            Self::Cores => "a number of cores, such as 1.5".to_owned(),
            Self::Address => "an address, as host:port".to_owned(),
            Self::Key => "a key, as sixty-four hex characters".to_owned(),
            Self::Choice(allowed) => format!("one of: {}", allowed.join(", ")),
            Self::Time => "a length of time, such as 30m, or none".to_owned(),
        }
    }
}

/// A length of time in seconds, the way somebody writes one.
fn read_time(text: &str) -> Result<u64, String> {
    let lowered = text.trim().to_ascii_lowercase();
    let (digits, unit) = match lowered.chars().last() {
        Some('s') => (&lowered[..lowered.len() - 1], 1u64),
        Some('m') => (&lowered[..lowered.len() - 1], 60),
        Some('h') => (&lowered[..lowered.len() - 1], 3600),
        Some('d') => (&lowered[..lowered.len() - 1], 86400),
        _ => (lowered.as_str(), 1),
    };
    let n: u64 = digits
        .parse()
        .map_err(|_| "is not a length of time, such as 30m or 90s".to_owned())?;
    n.checked_mul(unit)
        .ok_or_else(|| "is longer than any length of time needs to be".to_owned())
}

/// One setting the program knows about.
#[derive(Debug)]
pub struct Setting {
    /// Its name, in the file and, uppercased, in the environment.
    pub key: &'static str,
    /// What shape its value has.
    pub kind: Kind,
    /// What it is when nobody says.
    pub default: Value,
    /// One sentence, for `config show` and for the file's comments.
    pub about: &'static str,
}

/// Backends a host may be told to use.
const BACKENDS: &[&str] = &["auto", "x11", "synthetic"];

/// What the synthetic screen may be doing.
const MOTIONS: &[&str] = &["screen", "still", "video"];

/// When to lock this machine after a session ends.
const LOCK_AFTER: &[&str] = &["unattended", "always", "never"];

/// Every setting there is.
///
/// In the order somebody would read them: what this machine *is*, then what it
/// serves, then how it is found, then how it is reached.
pub static SETTINGS: &[Setting] = &[
    Setting {
        key: "device.name",
        kind: Kind::Text,
        default: Value::Text(Cow::Borrowed("")),
        about: "What to call this machine to the other end. Empty means its hostname.",
    },
    Setting {
        key: "host.listen",
        kind: Kind::Address,
        default: Value::Text(Cow::Borrowed("0.0.0.0:7645")),
        about: "Where the host listens.",
    },
    Setting {
        key: "host.backend",
        kind: Kind::Choice(BACKENDS),
        default: Value::Text(Cow::Borrowed("auto")),
        about: "Which capture backend to use. `auto` falls back and says which it chose; \
                naming one is a refusal to fall back.",
    },
    Setting {
        key: "host.synthetic_monitors",
        kind: Kind::Whole { least: 1, most: 8 },
        // Not a security decision, and not a test hook either: the synthetic
        // backend is already something a person can ask for by name, and how
        // many screens it pretends to have is the same kind of question as
        // what size they are. It exists because developing or testing a
        // multi-monitor viewer otherwise needs multi-monitor hardware.
        default: Value::Whole(1),
        about: "How many screens the synthetic backend pretends to have. Each is half the \
                size of the one before, so a switch between them is visible. Only applies \
                when the synthetic backend is in use.",
    },
    Setting {
        key: "host.synthetic_motion",
        kind: Kind::Choice(MOTIONS),
        // The same kind of question as how many screens it pretends to have,
        // and useful for the same reason: `still` is the only way to see what
        // a session watching a screen that nobody is touching actually costs,
        // and that is the state a screen spends most of its life in. `video`
        // is the other end -- every pixel different every frame -- for looking
        // at what the encoder does when a link cannot keep up.
        default: Value::Text(Cow::Borrowed("screen")),
        about: "What the synthetic backend's screen is doing: `screen` has a counter and a \
                moving box, `still` is a screen nobody is touching, `video` is every pixel \
                changing every frame. Only applies when the synthetic backend is in use.",
    },
    Setting {
        key: "host.lock_after_session",
        kind: Kind::Choice(LOCK_AFTER),
        // **`unattended` is a rule rather than a compromise between the other
        // two.** A session somebody answered a prompt for was watched by a
        // person who is still at the machine, and locking it under them is
        // rude and useless. A session nobody was asked about happened on a
        // machine with nobody at it, and leaving whatever the far end was
        // doing on the screen is exactly the exposure this closes. The host
        // already knows which of those happened, so the default needs no
        // configuring to be right in both cases.
        default: Value::Text(Cow::Borrowed("unattended")),
        about: "Lock this machine when a session ends: `unattended` locks only when nobody \
                here was asked to allow it, `always` locks whatever happened, `never` does \
                not. The panic key always locks, whatever this says.",
    },
    Setting {
        key: "host.monitor",
        kind: Kind::WholeOrNone { least: 0, most: 64 },
        default: Value::Nothing,
        about: "Which monitor to send. None means the primary one.",
    },
    Setting {
        key: "host.fps",
        kind: Kind::Whole {
            least: 1,
            most: 240,
        },
        default: Value::Whole(30),
        about: "The most frames a second to send.",
    },
    Setting {
        key: "host.budget",
        kind: Kind::Cores,
        default: Value::Fraction(1.0),
        about: "How much CPU the encoder may use, in cores.",
    },
    Setting {
        key: "host.max_resolution",
        kind: Kind::WholeOrNone {
            least: 240,
            most: 8192,
        },
        default: Value::Whole(1440),
        about: "The longest edge to send, in pixels. None sends the screen's own size.",
    },
    Setting {
        key: "host.privacy_screen",
        kind: Kind::Flag,
        // **Off, because a dark screen is alarming if nobody asked for it.**
        // On, this machine's own monitors go dark for the duration of every
        // session while the far end keeps its picture — for working on a
        // machine in a shared room, which is the case it exists for.
        //
        // A standing preference rather than a per-session prompt: somebody
        // whose machine is in a shared office wants this every time, and
        // somebody whose machine is at home never does. It is undone when the
        // session ends, when the panic key is pressed, and — by a second
        // process that outlives this one — if the host dies while it is on.
        default: Value::Flag(false),
        about: "Blank this machine's own screens while a session is running. The session keeps \
                its picture; somebody walking past this machine sees nothing.",
    },
    Setting {
        key: "host.serve_without_indicator",
        kind: Kind::Flag,
        // **Off, which is a refusal to serve.** Software that can watch a
        // screen has to make being watched obvious, and a host that cannot
        // show that has nothing to fall back on: the person in front of the
        // machine gets no warning at all. Until this setting existed such a
        // host printed a line about it and then served anyway, which is the
        // documented version of a guarantee rather than the enforced one.
        //
        // It is a setting rather than an absent option because there is one
        // real case for it — a display this product cannot draw on but
        // somebody still wants to reach — and the alternative to a switch is
        // that person having no product. Turning it on is a decision with a
        // name, which is the point.
        default: Value::Flag(false),
        about: "Serve sessions even when this machine cannot show that it is being watched. \
                Off refuses to serve on a display where the indicator cannot be confirmed.",
    },
    Setting {
        key: "policy.deny_when_locked",
        kind: Kind::Flag,
        // **Off, because it is a bound on `unattended` rather than a part of
        // it.** A machine in a rack has no screen to lock and would be
        // refusing sessions for a state it can never leave; a machine on a
        // desk is exactly where somebody wants this.
        //
        // On, a device holding `unattended` is refused while the screen is
        // locked — with a code of its own, because it is the one refusal that
        // fixes itself when somebody unlocks the machine. A device *without*
        // `unattended` is unaffected: it is asked about, and a locked screen
        // answers that question by nobody being there.
        //
        // **And where the lock state cannot be established, it is treated as
        // locked** — said out loud when the host starts, because a policy that
        // quietly does nothing is the one somebody finds out about at three in
        // the morning. See `esper-presence`'s `locked` module for the three
        // probes and what each of them knows.
        default: Value::Flag(false),
        about: "Refuse unattended devices while the screen at this machine is locked. Where \
                the lock state cannot be established it counts as locked, and the host says \
                so when it starts.",
    },
    Setting {
        key: "consent.answer_within",
        kind: Kind::Whole {
            least: 5,
            most: 300,
        },
        // **Forty-five seconds**, which is long enough to walk back to the
        // machine and short enough that a controller is not left wondering
        // whether anything is happening — it says what it is waiting for
        // after a second of silence, so this is not a blind wait.
        //
        // A setting because the right answer is somebody's own: a machine on
        // a desk wants less, and a machine in another room wants more. The
        // floor is five seconds rather than zero, because a prompt that
        // disappears before it can be read is not a prompt — it is a
        // refusal with extra steps, and the person who set it to one second
        // would not have meant that.
        default: Value::Whole(45),
        about: "How many seconds somebody at the host has to answer a connection prompt \
                before it is refused. Between 5 and 300.",
    },
    Setting {
        key: "host.pace_to_measured",
        kind: Kind::Flag,
        // **On, and switchable.** Six attempts at pacing a media session were
        // built and measured worse than not pacing at all, and this is the
        // seventh — the first inside the transport, where quinn's own pacer
        // does the work from the congestion window. It is measured better on
        // the link that motivated it and costs nothing on a clean one, and it
        // is still new enough that an operator seeing something strange should
        // be able to take it out of the picture in one line rather than by
        // rebuilding.
        default: Value::Flag(true),
        about: "Pace this machine's video at the rate the viewer measures the link at, rather \
                than at whatever the congestion window allows. Off falls back to plain Cubic.",
    },
    Setting {
        key: "session.idle_timeout",
        kind: Kind::Time,
        default: Value::Nothing,
        about: "End a terminal session after this long with no input and no output. \
                None is off, which is the default because a build that prints nothing \
                for an hour is working, not idle.",
    },
    Setting {
        key: "audit.retain",
        kind: Kind::Time,
        // **Ninety days, and not "for ever".**
        //
        // A trail is for answering questions weeks after the fact — "was
        // anybody on this machine while I was away" — and three months covers
        // that with room. Keeping it for ever sounds safer and is not: it is a
        // growing list of every machine somebody connected to, on a laptop
        // that can be stolen, answering a question nobody asked. The record
        // exists to be *read*, and a record nobody will ever read is only a
        // liability.
        //
        // `none` keeps everything, for somebody who has decided that is what
        // they want. It is a choice rather than the default.
        // **In seconds, because that is what a `Time` value *is*.** Written
        // as `"90d"` this looked right and did nothing: a `Time` reads to a
        // number of seconds, and a default left as text is a default the
        // number-reader cannot see — so the shipped policy would have been "no
        // age limit" while the setting said ninety days. Found by a smoke
        // check that set the value explicitly and watched the default case do
        // nothing.
        default: Value::Whole(90 * 86_400),
        about: "How long to keep the session trail. Older entries are deleted when the host                 starts and once a day while it runs. `none` keeps everything, which on a                 machine that can be lost is a decision rather than a default.",
    },
    Setting {
        key: "audit.max_size",
        kind: Kind::WholeOrNone {
            least: 1,
            most: 4096,
        },
        // **A ceiling as well as an age**, because the two fail differently.
        // A machine with a hundred sessions a day fills a disk long before
        // ninety days are up, and a retention policy that only understands
        // time would let it. The oldest entries go first, which is the same
        // rule the age applies, so the two cannot disagree about what to keep.
        default: Value::Whole(64),
        about: "Largest the session trail may grow, in megabytes. The oldest entries are                 deleted to stay inside it. `none` is no ceiling, which leaves the age above                 as the only limit.",
    },
    Setting {
        key: "forward.allow",
        kind: Kind::Text,
        // **Empty is loopback, never everything.** A forward can be granted
        // without a shell, so an unrestricted one would make the narrowest
        // grant the one with the widest reach — every machine on the host's
        // network, through a host that belongs to somebody else. The shape of
        // an entry is checked when the host starts, not here: this file's
        // parser knows about text, and what a forwarding target looks like
        // belongs with the code that dials one.
        default: Value::Text(Cow::Borrowed("")),
        about: "Targets this machine will forward to, as host:port entries separated by \
                commas — [::1]:5432 for an IPv6 literal, db.internal:* for any port. \
                Empty means this machine's own loopback and nothing else.",
    },
    Setting {
        key: "forward.max_connections",
        kind: Kind::Whole {
            least: 1,
            // Below the transport's 256 concurrent bidirectional streams, with
            // room left for the session's own control streams: one forwarded
            // connection is one stream, and a cap above what the transport
            // will open would be a promise the connection cannot keep.
            most: 192,
        },
        // Each is a stream, a socket and two copy buffers, so this is a bound
        // on what one session can make the host hold — checked before the
        // dial, not after.
        default: Value::Whole(64),
        about: "How many forwarded connections one session may have open at once.",
    },
    Setting {
        key: "files.enabled",
        kind: Kind::Flag,
        // **Off, like every other capability that reaches a disk.** A machine
        // that has not been asked to serve files does not serve them, whatever
        // a device holds: the permission says who may, and this says whether
        // this machine does at all.
        default: Value::Flag(false),
        about: "Serve file transfer at all. Off by default: the per-device permissions say \
                who may move files, and this says whether this machine does it.",
    },
    Setting {
        key: "files.root",
        kind: Kind::Text,
        // **A directory of its own, never a home directory and never `/`.**
        // Everything a peer can name is relative to this and cannot leave it,
        // so what this points at is the whole of what a file permission
        // reaches. Empty means the default below, worked out where paths are.
        default: Value::Text(Cow::Borrowed("")),
        about: "The directory file transfers happen inside. Everything a peer can name is \
                relative to it and nothing can leave it. Empty means ~/Esper.",
    },
    Setting {
        key: "files.max_bytes",
        kind: Kind::WholeOrNone {
            least: 1,
            // A terabyte. Past the point where a bound is protecting anything
            // and short of the point where the arithmetic gets interesting.
            most: 1024 * 1024 * 1024 * 1024,
        },
        // No limit of its own by default: the disk is the limit, and a
        // transfer is refused before the first byte if it will not fit. A
        // figure here is for a machine whose owner wants a smaller one.
        default: Value::Nothing,
        about: "The largest single file this machine will send or accept, in bytes. \
                `none` means whatever fits on the disk.",
    },
    Setting {
        key: "discovery.mdns",
        kind: Kind::Flag,
        // **The one deliberate exception to strict defaults.** It reveals that
        // a machine runs Esper to a network segment that is usually the
        // operator's own house, and it buys the best thing about the product
        // on a home network: you open it and your machines are there.
        default: Value::Flag(true),
        about: "Announce this machine on the local network, and look for others.",
    },
    Setting {
        key: "discovery.mdns_interfaces",
        kind: Kind::Text,
        // **Named interfaces, or the ones that look like a local network.**
        // Empty is not "none": it is "work it out", which on a machine with
        // one route to the LAN is the same answer anybody would give.
        default: Value::Text(Cow::Borrowed("")),
        about: "Interfaces to announce this machine on, separated by commas. Empty means \
                every ordinary local network — not loopback, not a tunnel, and not a \
                container bridge.",
    },
    Setting {
        key: "connectivity.overlay_interfaces",
        kind: Kind::Text,
        // **Names Esper missed, never names it should stop believing.** The
        // escape hatch opens one way on purpose: calling a path private when
        // it is not costs a wasted dial, and there is no setting that can turn
        // a public path into one Esper trusts more.
        default: Value::Text(Cow::Borrowed("")),
        about: "Interfaces to treat as overlay networks even if their names are not \
                recognised, separated by commas. Empty means autodetect: wg*, tailscale*, \
                tun*, utun*, zt* and nebula*.",
    },
    Setting {
        key: "broker.address",
        kind: Kind::Address,
        // **No default broker address anywhere in Esper.** Not in the binary,
        // not in the shipped configuration, not in the installer. Without one
        // the broker tiers do not exist, and that is the product working
        // correctly rather than a gap.
        default: Value::Nothing,
        about: "A broker you run, for when nothing direct works. There is no default.",
    },
    Setting {
        key: "broker.key",
        kind: Kind::Key,
        default: Value::Nothing,
        about: "That broker's key, pinned. `esper-broker serve` prints it.",
    },
];

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn every_default_is_the_shape_its_setting_says() {
        // A default that would be refused if somebody wrote it in a file is a
        // default nobody could restore.
        for setting in SETTINGS {
            if setting.default.is_nothing() {
                continue;
            }
            let written = match &setting.default {
                Value::Text(t) => t.to_string(),
                other => other.to_string(),
            };
            if written.is_empty() {
                continue;
            }
            assert!(
                setting.kind.read(&written).is_ok(),
                "{}'s default ({written}) is not a valid value for it",
                setting.key
            );
        }
    }

    #[test]
    fn the_defaults_are_the_secure_set() {
        // **The test the whole file exists for.** Each of these is a security
        // decision, and a default that quietly relaxes is the failure this
        // catches.
        let get = |key: &str| {
            SETTINGS
                .iter()
                .find(|s| s.key == key)
                .unwrap_or_else(|| panic!("{key} is not a setting"))
        };

        // No broker address anywhere: without one, those tiers do not exist.
        assert!(
            get("broker.address").default.is_nothing(),
            "a default broker address would be an endpoint nobody chose"
        );
        assert!(get("broker.key").default.is_nothing());

        // Discovery is the one deliberate exception, and it is on.
        assert_eq!(
            get("discovery.mdns").default,
            Value::Flag(true),
            "discovery is the documented exception to strict defaults"
        );

        // An idle timeout that killed a working build by default would be
        // data loss wearing a security feature's clothes.
        assert!(get("session.idle_timeout").default.is_nothing());

        // A backend that is named is a refusal to fall back; the default
        // falls back and says so.
        assert_eq!(
            get("host.backend").default,
            Value::Text(Cow::Borrowed("auto"))
        );
    }

    #[test]
    fn a_value_of_the_wrong_shape_says_what_is_wrong_with_it() {
        assert!(Kind::Flag.read("maybe").is_err());
        assert!(Kind::Address.read("7645").is_err());
        assert!(Kind::Address.read("0.0.0.0:7645").is_ok());
        assert!(Kind::Key.read("abc").is_err());
        assert!(Kind::Key.read(&"a".repeat(64)).is_ok());
        assert!(Kind::Key.read(&"z".repeat(64)).is_err());
        assert!(Kind::Choice(BACKENDS).read("wayland").is_err());
        assert!(Kind::Choice(BACKENDS).read("X11").is_ok());
        assert!(Kind::Whole { least: 1, most: 10 }.read("11").is_err());
        assert!(Kind::Cores.read("0").is_err());
        assert!(Kind::Cores.read("1.5").is_ok());
    }

    #[test]
    fn a_length_of_time_is_read_the_way_somebody_writes_one() {
        assert_eq!(Kind::Time.read("30m").unwrap(), Value::Whole(1800));
        assert_eq!(Kind::Time.read("2h").unwrap(), Value::Whole(7200));
        assert_eq!(Kind::Time.read("90s").unwrap(), Value::Whole(90));
        assert_eq!(Kind::Time.read("45").unwrap(), Value::Whole(45));
        // Off, and said as a decision rather than as an absence.
        assert!(Kind::Time.read("none").unwrap().is_nothing());
        assert!(Kind::Time.read("0").unwrap().is_nothing());
        assert!(Kind::Time.read("soon").is_err());
    }

    #[test]
    fn true_and_false_are_written_the_way_people_write_them() {
        for yes in ["true", "TRUE", "yes", "on", "1"] {
            assert_eq!(Kind::Flag.read(yes).unwrap(), Value::Flag(true), "{yes}");
        }
        for no in ["false", "No", "off", "0"] {
            assert_eq!(Kind::Flag.read(no).unwrap(), Value::Flag(false), "{no}");
        }
    }

    #[test]
    fn every_setting_says_what_it_is_for() {
        for setting in SETTINGS {
            assert!(
                !setting.about.is_empty(),
                "{} explains nothing",
                setting.key
            );
            assert!(
                setting.about.ends_with('.'),
                "{}'s explanation is not a sentence",
                setting.key
            );
            assert!(
                setting.key.contains('.'),
                "{} is not grouped under anything",
                setting.key
            );
        }
    }

    #[test]
    fn no_two_settings_have_the_same_name() {
        let mut seen = std::collections::BTreeSet::new();
        for setting in SETTINGS {
            assert!(
                seen.insert(setting.key),
                "{} is declared twice",
                setting.key
            );
        }
    }
}
