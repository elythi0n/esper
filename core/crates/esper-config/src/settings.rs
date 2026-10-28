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
                // **Both words mean the same thing**: this machine works it
                // out. `none` is what the file has always accepted and `auto`
                // is what the design documents call it — a person who writes
                // the word the document taught them should not get an error.
                if text.eq_ignore_ascii_case("none") || text.eq_ignore_ascii_case("auto") {
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

    /*
        Read a value that is about to be **written**, which is stricter.

        # Why writing and reading are not the same rule

        Because a file somebody already has must keep loading. This format is
        `key = value`, one per line, with no escapes: a value is quoted on the
        way out and the quotes are taken off on the way in, and that is the
        whole of it. So a newline in a value cannot survive a round trip —
        Rust's own quoting writes it as the two characters `\n`, the parser has
        no rule that turns them back, and `config set device.name $'one\ntwo'`
        stored something quietly different from what was asked for. A quote is
        worse: it closes the value early, and the rest of the line becomes
        something the parser refuses.

        Refusing those at the point somebody *writes* one costs them a second
        attempt. Refusing them at the point the program *reads* the file costs
        them a machine that will not start — over a file that has been loading
        for however long, because a rule was tightened under it. So the reader
        stays exactly as liberal as it was and this is the writer's own rule.

        Refused rather than escaped, because adding escapes to this format is
        adding a format.

        # Errors

        As [`Kind::read`], and for text that one line cannot hold.
    */
    pub fn read_for_writing(self, raw: &str) -> Result<Value, String> {
        let value = self.read(raw)?;
        if matches!(self, Self::Text) {
            for bad in raw.trim().chars() {
                if bad == '"' || bad == '\\' || bad.is_control() {
                    return Err(format!(
                        "cannot hold {bad:?}: this file is `key = value` with no escapes, \
                         so a value is text on one line"
                    ));
                }
            }
        }
        Ok(value)
    }

    /// How to write one of these, for the file's own comments.
    #[must_use]
    pub fn shape(self) -> String {
        match self {
            Self::Text => "text".to_owned(),
            Self::Flag => "true or false".to_owned(),
            Self::Whole { least, most } => format!("a whole number, {least} to {most}"),
            Self::WholeOrNone { least, most } => {
                format!("a whole number, {least} to {most}, or auto")
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
        about: "The name other devices see for this machine. Leave it empty to use its hostname.",
    },
    Setting {
        key: "host.listen",
        kind: Kind::Address,
        default: Value::Text(Cow::Borrowed("0.0.0.0:7645")),
        about: "The address and port this machine listens on for connections.",
    },
    Setting {
        key: "host.backend",
        kind: Kind::Choice(BACKENDS),
        default: Value::Text(Cow::Borrowed("auto")),
        about: "How this machine captures its screen. `auto` picks one that works; choosing a \
                specific one uses only that.",
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
        about: "How many test screens the synthetic backend draws, each half the size of the one \
                before. Only used with the synthetic backend.",
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
        about: "What the synthetic test screen shows: `screen` is a counter and a moving box, \
                `still` never changes, `video` changes every pixel every frame. Only used with the \
                synthetic backend.",
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
        about: "Whether to lock this machine when a session ends. `unattended` locks only when the \
                session started without asking anyone here, `always` always locks, `never` never \
                does. Pressing Ctrl+Alt+Shift+Escape ends a session and always locks.",
    },
    Setting {
        key: "host.monitor",
        kind: Kind::WholeOrNone { least: 0, most: 64 },
        default: Value::Nothing,
        about: "Which monitor to share. None means the main one.",
    },
    Setting {
        key: "host.fps",
        kind: Kind::Whole {
            least: 1,
            most: 240,
        },
        default: Value::Whole(30),
        about: "The most frames per second to send.",
    },
    Setting {
        key: "host.budget",
        kind: Kind::Cores,
        default: Value::Fraction(1.0),
        about: "How much processor power video encoding may use. 1 is one processor core; most \
                machines have 4 or more.",
    },
    Setting {
        key: "host.max_resolution",
        kind: Kind::WholeOrNone {
            least: 240,
            most: 8192,
        },
        default: Value::Whole(1440),
        about: "The longest side of the picture to send, in pixels. None sends the screen at its \
                own size.",
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
        about: "Turn this machine's own screens dark during a session. The other end still sees \
                everything; someone walking past here sees nothing.",
    },
    Setting {
        key: "host.lock_local_input",
        kind: Kind::Flag,
        // **The other half of the privacy screen, and off for the same
        // reason.** That one stops somebody walking past *seeing* a session;
        // this stops them typing into it. The case is a machine in a shared
        // room being worked on remotely by the person who owns it, and the two
        // halves are wanted together or not at all.
        //
        // It detaches this machine's own keyboard and pointer from their
        // master for the session, which leaves remote injection working —
        // injection is delivered as the master. A *grab* was the obvious
        // implementation and cannot work: it redirects injected events too, so
        // it would block the remote control it exists to protect.
        //
        // Every device is registered with a keeper before it is detached, and
        // a keeper that cannot be started means nothing is detached. Without
        // that, a host that died would leave the person at that machine with
        // no keyboard and no pointer — worse than a dark monitor, where they
        // can still type blind.
        //
        // **And the panic key goes with them**, which is the price of this and
        // is said in the `about` rather than left for somebody to discover. A
        // hotkey is a key combination, and a keyboard with no master sends its
        // keys nowhere — so while this is on, a session ends from the far end,
        // on its own clock, or when the host stops. That is the same trade
        // somebody makes when they ask for a machine nobody at it can type
        // into; it is not a trade to make on their behalf, which is why this
        // is off by default.
        default: Value::Flag(false),
        about: "Block this machine's own keyboard and mouse during a session, so nobody here can \
                type into it. Remote control still works, and they come back as soon as the \
                session ends or Esper stops. While this is on, Ctrl+Alt+Shift+Escape here cannot \
                end the session.",
    },
    Setting {
        key: "host.plain_background",
        kind: Kind::Flag,
        // **Bandwidth, not privacy — and off, because it is somebody's
        // desktop.** A desktop is windows over a background; the windows are
        // what the far end came for, and a photograph behind them is 63% of a
        // keyframe at 1280x720 with a tenth of the screen covered, 48% with a
        // quarter, 25% with half (measured in
        // `esper-encode/tests/background_cost.rs`).
        //
        // It lands on the **first picture** and on every keyframe after a
        // change of size, because capture is damage-driven and a still
        // background is sent once and never again. Which is exactly where
        // this product measured its worst behaviour: a link whose buffer
        // could not hold one keyframe showed no picture at all.
        //
        // Off because the person at the machine watches their own wallpaper
        // go black for the session. And it only works where the desktop has
        // *published* its background pixmap — a background nothing can put
        // back is one this may not take away.
        default: Value::Flag(false),
        about: "Replace this machine's wallpaper with a plain colour during a session, which makes \
                the picture smaller to send. Your wallpaper comes back when the session ends.",
    },
    Setting {
        key: "host.allow_asking",
        kind: Kind::Flag,
        // **On, because the alternative is worse in practice.** A session that
        // cannot ask leaves the person at the far end telephoning the person
        // at the machine to talk them through `esper grant` — and the fastest
        // way through that call is granting more than the moment needs, for
        // ever, because nobody wants to do it twice.
        //
        // What asking cannot do is grant anything: the answer comes from
        // somebody at the machine, it lasts for that session only, and nothing
        // is written to the device list. A session may put at most three
        // questions and a refusal is good for thirty seconds, so a device
        // cannot make somebody's screen unusable with prompts.
        //
        // Off is for a machine that should never draw a dialog on its own
        // screen — a kiosk, a machine in a rack with somebody's session on it
        // — and the far end is told it is a standing no rather than a refusal
        // worth retrying.
        default: Value::Flag(true),
        about: "Let an open session ask someone at this machine for a permission it does not have, \
                such as control or file transfer. The answer lasts only for that session.",
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
        about: "Allow sessions even when this machine cannot show its 'being viewed' badge. Off \
                refuses sessions on a display where the badge cannot be shown, so nobody is \
                watched without knowing.",
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
        about: "Refuse devices that are allowed to connect without asking while this machine's \
                screen is locked. If Esper cannot tell whether it is locked, it treats it as \
                locked and says so.",
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
        about: "How many seconds someone here has to answer a connection request before it is \
                refused. Between 5 and 300.",
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
        about: "Send video at the speed the viewer reports it can receive, which keeps the picture \
                smooth on slow networks. Off sends as fast as the network accepts.",
    },
    Setting {
        key: "session.buffer",
        // Five to two thousand: below a frame interval at any framerate this
        // product reaches, and above the widest wait the adaptive rule will
        // ever choose by itself. Somebody pinning it is allowed outside what
        // `auto` would pick — that is what an override is for — and not
        // outside what a number of milliseconds can sensibly mean.
        kind: Kind::WholeOrNone {
            least: 5,
            most: 2000,
        },
        // **`auto` is right almost always, which is not always.**
        //
        // The wait is how long a controller gives the rest of a frame before
        // deciding it is as complete as it is going to get, and it is derived
        // from the framerate and widened to what the link has shown. That rule
        // was measured into existence — a link whose frames take longer than
        // the gap between frames had every keyframe cut into pieces and
        // discarded, and a session showed nothing at all.
        //
        // The override is for the two cases a rule cannot cover: somebody on a
        // LAN who wants the shortest wait there is and will accept a torn
        // frame for it, and somebody whose link this rule reads wrongly. A
        // pinned wait is the answer rather than a floor — the adaptation would
        // otherwise widen it back on exactly the link it was pinned for.
        //
        // It was promised in `capture-and-codec.md` and did not exist: no
        // setting, no plumbing, no caller. A promise a product does not keep
        // is worse than a feature it does not have.
        default: Value::Nothing,
        about: "How long the picture waits for a frame that is still arriving, in milliseconds, \
                when you view another machine. `auto` works it out for you and is almost always \
                right. A smaller number feels quicker but may show half-drawn frames.",
    },
    Setting {
        key: "session.idle_timeout",
        kind: Kind::Time,
        default: Value::Nothing,
        about: "End a terminal session after this long with nothing typed and nothing printed. \
                None never ends it, because a quiet terminal may still be working.",
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
        about: "How long to keep the activity log. Older entries are deleted when Esper starts and \
                once a day after. `none` keeps everything.",
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
        about: "The largest the activity log may grow, in megabytes. The oldest entries are \
                deleted to stay under it. `none` means no limit.",
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
        about: "Which services on or near this machine a paired device may connect to through port \
                forwarding, as host:port entries separated by commas, such as db.internal:5432 or \
                db.internal:* for any port. Empty means only services on this machine itself.",
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
        about: "Allow file transfer on this machine. Off by default; each device's permissions \
                still decide who may send files.",
    },
    Setting {
        key: "files.root",
        kind: Kind::Text,
        // **A directory of its own, never a home directory and never `/`.**
        // Everything a peer can name is relative to this and cannot leave it,
        // so what this points at is the whole of what a file permission
        // reaches. Empty means the default below, worked out where paths are.
        default: Value::Text(Cow::Borrowed("")),
        about: "The folder file transfers use. Devices can only reach files inside it. Empty means \
                ~/Esper.",
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
        about: "The largest file this machine will send or receive, in bytes. `none` means no \
                limit.",
    },
    Setting {
        key: "network.port_mapping",
        kind: Kind::Flag,
        // **Off, and this one is not a close call.** A mapping is a hole in a
        // firewall, opened by a program, on a router the person may share with
        // a household or an office — and what it buys is skipping an
        // introducer, not making anything work that otherwise would not. On
        // means this machine is reachable from the whole internet rather than
        // from whoever a broker introduced, which is a decision about somebody
        // else's network as much as their machine.
        default: Value::Flag(false),
        about: "Ask your router to open a port for this machine, so other devices can connect \
                directly over the internet. Off by default, because it opens a hole in your \
                firewall while Esper runs.",
    },
    Setting {
        key: "discovery.mdns",
        kind: Kind::Flag,
        // **The one deliberate exception to strict defaults.** It reveals that
        // a machine runs Esper to a network segment that is usually the
        // operator's own house, and it buys the best thing about the product
        // on a home network: you open it and your machines are there.
        default: Value::Flag(true),
        about: "Let devices on this network find this machine, and find them.",
    },
    Setting {
        key: "discovery.mdns_interfaces",
        kind: Kind::Text,
        // **Named interfaces, or the ones that look like a local network.**
        // Empty is not "none": it is "work it out", which on a machine with
        // one route to the LAN is the same answer anybody would give.
        default: Value::Text(Cow::Borrowed("")),
        about: "Which network interfaces to be found on, separated by commas. Empty means every \
                normal local network, but not loopback, VPNs or container networks.",
    },
    Setting {
        key: "connectivity.overlay_interfaces",
        kind: Kind::Text,
        // **Names Esper missed, never names it should stop believing.** The
        // escape hatch opens one way on purpose: calling a path private when
        // it is not costs a wasted dial, and there is no setting that can turn
        // a public path into one Esper trusts more.
        default: Value::Text(Cow::Borrowed("")),
        about: "Extra network interfaces to treat as private networks like Tailscale or WireGuard, \
                separated by commas. Empty finds the common ones automatically.",
    },
    Setting {
        key: "broker.address",
        kind: Kind::Address,
        // **No default broker address anywhere in Esper.** Not in the binary,
        // not in the shipped configuration, not in the installer. Without one
        // the broker tiers do not exist, and that is the product working
        // correctly rather than a gap.
        default: Value::Nothing,
        about: "The address of a broker: a small server you run so devices on different networks \
                can find each other. Only needed for invitations and devices you cannot reach \
                directly.",
    },
    Setting {
        key: "broker.key",
        kind: Kind::Key,
        default: Value::Nothing,
        about: "The broker's key, which it prints when it starts. Esper refuses a broker whose key \
                does not match.",
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

#[cfg(test)]
mod text_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::Kind;

    /*
    A text value is what the file can hold and give back.

    The format is `key = value` with no escapes: a value is quoted and the
    quotes are taken off again. So a newline cannot survive a round trip —
    Rust's quoting writes it as the two characters `\n`, the parser has no rule
    that turns them back, and what was set quietly became something else. A
    quote is worse: it closes the value early and the rest of the line is
    something the parser refuses, which is a file the machine will not start
    with.
    */
    #[test]
    fn text_is_what_one_line_can_hold() {
        for good in [
            "elythion",
            "the one under the stairs",
            "デスクトップ · مكتب",
            "",
        ] {
            Kind::Text
                .read_for_writing(good)
                .unwrap_or_else(|e| panic!("{good:?}: {e}"));
        }

        // **And the reader stays liberal.** A file that has been loading is a
        // file that keeps loading: tightening what a value may be would stop
        // a machine starting over something written before the rule existed.
        for already in ["one\ntwo", "say \"hello\"", "back\\slash"] {
            Kind::Text
                .read(already)
                .unwrap_or_else(|e| panic!("a file holding {already:?} stopped loading: {e}"));
        }
        for bad in [
            "one\ntwo",
            "one\rtwo",
            "one\ttwo",
            "say \"hello\"",
            "back\\slash",
            "nul\0",
        ] {
            let refused = Kind::Text.read_for_writing(bad).expect_err(&format!(
                "{bad:?} was accepted into a file that cannot hold it"
            ));
            // Says what the shape is, because the next thing somebody does is
            // pick a different value.
            assert!(refused.contains("one line"), "{bad:?}: {refused}");
        }
    }

    /// And what is accepted round-trips: written as the file writes it, read
    /// back as the parser reads it, the same text.
    #[test]
    fn what_is_accepted_survives_the_file() {
        for good in [
            "elythion",
            "the one under the stairs",
            "デスクトップ",
            "a#b",
            "a = b",
        ] {
            let value = Kind::Text.read_for_writing(good).expect("accepted");
            let (written, _) = crate::set_in("", "device.name", &value);
            let read = crate::parse(&written).expect("a file this wrote");
            let (_, back, _) = read.last().expect("the line");
            assert_eq!(back, good, "written as {written:?}");
        }
    }
}
