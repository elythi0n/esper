//! Whether somebody at the host agrees to this session.
//!
//! # What `unattended` means
//!
//! A paired device may open a session when either a person at the host accepts
//! it, or the device holds [`Permission::Unattended`] — which is exactly what
//! that permission is *for*. Before this existed, `unattended` was a
//! permission that granted nothing and withholding it prevented nothing:
//! every paired device could connect to an empty room.
//!
//! The default grant at pairing is `view` and `control` and **not**
//! `unattended`, so consent is the default and unattended access is a
//! deliberate act — one command, on the machine it affects, by somebody who
//! is already there.
//!
//! # Fail closed, and say what to do
//!
//! A host running as a service has no terminal to ask at. The answer there is
//! **no**, not "well, nobody objected": a permission that evaporates when the
//! room is empty is the opposite of a permission. So the session is refused
//! with [`Reason::NOBODY_TO_ASK`], and the host prints the one command that
//! changes it — because a deployment that stops working for a good reason
//! still has to say which reason.
//!
//! # Why the answer is remembered for a moment
//!
//! A session that drops and reconnects — a network blip, a laptop lid, the
//! park-and-resume path — must not ask again. Somebody who accepted eight
//! seconds ago would accept again, and asking twice for one intention teaches
//! them to answer without reading. So an acceptance stands for [`GRACE`],
//! per device, and no longer.
//!
//! It is safe because the device is already authenticated: the grace applies
//! to a key that finished a TLS handshake, not to an address or a name.

use std::io::{BufRead as _, IsTerminal as _, Write as _};
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use esper_crypto::Device;
use esper_proto::{Permission, Reason, SessionRequest};
use esper_transport::{Policy, Verdict};

/// How long somebody has to answer.
///
/// Long enough to walk back to the machine, short enough that a controller is
/// not left wondering whether anything is happening. The controller says what
/// it is waiting for after a second of silence, so this is not a blind wait.
pub const ANSWER_WITHIN: Duration = Duration::from_secs(45);

/// How long an acceptance stands for, so a reconnect is not a second question.
///
/// Longer than the park timeout, which is what a session that dropped waits
/// out before it is gone; short enough that somebody who accepted, left, and
/// came back to find a session open would still recognise it as the one they
/// allowed.
pub const GRACE: Duration = Duration::from_secs(120);

/// Somebody at the host, and what they said.
pub trait Ask: Send + Sync + 'static {
    /// Whether this machine has anybody to ask at all.
    ///
    /// Checked separately from asking, because "no" to a question nobody was
    /// asked is a different answer from "no" from a person, and the two need
    /// different words.
    fn can_ask(&self) -> bool;

    /// Ask, and wait up to `within`.
    fn ask(&self, about: &Asked, within: Duration) -> Answer;
}

/// What a person is being asked about.
#[derive(Debug, Clone)]
pub struct Asked {
    /// What the device is called here.
    pub name: String,
    /// The short form of its key, so two devices with one name are still two.
    pub fingerprint: String,
    /// What the session would be able to do — the clamped set, not the
    /// request, because what somebody is agreeing to is what will happen.
    pub granted: Vec<Permission>,
    /// What kind of session: a screen, a shell, a forward.
    pub kind: &'static str,
}

/// The three answers, which are not two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// Somebody said yes.
    Yes,
    /// Somebody said no.
    No,
    /// Nobody said anything in time.
    Silence,
}

/// The most of a peer-supplied name to put in a question.
///
/// Long enough for any name somebody would choose, short enough that a name
/// cannot push the rest of the question off the screen.
const NAME_SHOWN: usize = 48;

/// A peer-supplied string, safe to print where somebody is deciding.
///
/// **The prompt sanitises what it prints, rather than trusting that something
/// else did.** A device name arrives from the far end, and the handshake does
/// reject control characters in one — but this is the place a person makes a
/// security decision, and it should not depend on an invariant established
/// three layers away and one process earlier. The device store is a file, and
/// a name in it is only as checked as whatever wrote it.
///
/// What it prevents is specific: a name containing a newline and a few spaces
/// can draw a *second*, fake question underneath the real one, and a person
/// who answers the wrong one has consented to something they did not read. A
/// name carrying an escape sequence can erase the real one entirely.
fn shown(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars().filter(|c| !c.is_control()) {
        if out.chars().count() >= NAME_SHOWN {
            out.push('…');
            break;
        }
        out.push(c);
    }
    if out.is_empty() {
        "an unnamed device".to_owned()
    } else {
        out
    }
}

/// A question asked on the host's terminal.
///
/// One reader thread owns stdin for the life of the process, because stdin
/// cannot be read from two places and a prompt must not compete with anything
/// else for it.
pub struct Console {
    lines: Mutex<mpsc::Receiver<String>>,
    a_terminal: bool,
}

impl std::fmt::Debug for Console {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Console")
            .field("a_terminal", &self.a_terminal)
            .finish()
    }
}

impl Console {
    /// Start reading the host's terminal, if it has one.
    #[must_use]
    pub fn open() -> Self {
        let a_terminal = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
        let (tx, rx) = mpsc::channel();
        if a_terminal {
            // A detached thread, deliberately: it lives as long as the process
            // and there is nothing to join it for. A blocking read on stdin
            // cannot be cancelled, so a thread that could be joined would be a
            // thread that stopped a clean exit.
            std::thread::spawn(move || {
                let stdin = std::io::stdin();
                for line in stdin.lock().lines() {
                    let Ok(line) = line else { break };
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            });
        }
        Self {
            lines: Mutex::new(rx),
            a_terminal,
        }
    }
}

impl Console {
    /// Whether there is a terminal to ask at.
    ///
    /// Inherent as well as on [`Ask`], so a caller that only wants to ask one
    /// question does not have to import a trait to find out whether it can.
    #[must_use]
    pub const fn can_ask(&self) -> bool {
        self.a_terminal
    }

    /// Ask a person at the terminal a yes-or-no question, safely.
    ///
    /// **One place that knows how to do this**, because the two things that
    /// make it safe are the two easiest to leave out, and leaving either out
    /// is a hole rather than a papercut:
    ///
    /// - **Anything already typed is thrown away first.** A newline sitting in
    ///   the terminal's buffer from before the question was printed is not an
    ///   answer to it, and something granted by a stray keystroke is something
    ///   nobody agreed to.
    /// - **Only yes is yes.** An empty line is the default and the default is
    ///   no; anything unrecognised is asked again rather than guessed at,
    ///   because a guess in this direction is the one that says yes.
    ///
    /// `lines` is the question's context, one line each; `question` is the
    /// last line, which is what somebody is answering.
    pub fn confirm(&self, lines: &[String], question: &str, within: Duration) -> Answer {
        let Ok(typed) = self.lines.lock() else {
            // The reader's channel is poisoned, which means a panic somewhere
            // holding it. Refusing is the only safe answer.
            return Answer::No;
        };
        while typed.try_recv().is_ok() {}

        println!();
        for (i, line) in lines.iter().enumerate() {
            let edge = if i == 0 { '┌' } else { '│' };
            println!("  {edge} {line}");
        }
        print!("  └ {question} [y/N] ({}s) ", within.as_secs().max(1));
        // Flushed, because the prompt has no newline: without this it sits in
        // the buffer and somebody is asked a question they cannot see.
        let _ = std::io::stdout().flush();

        let deadline = Instant::now() + within;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                println!("\n  · nobody answered");
                return Answer::Silence;
            }
            match typed.recv_timeout(left) {
                Ok(line) => {
                    let said = line.trim().to_ascii_lowercase();
                    if said == "y" || said == "yes" {
                        return Answer::Yes;
                    }
                    if said.is_empty() || said == "n" || said == "no" {
                        return Answer::No;
                    }
                    print!("  └ please answer y or n: ");
                    let _ = std::io::stdout().flush();
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    println!("\n  · nobody answered");
                    return Answer::Silence;
                }
                // The terminal closed. Nobody is there to ask any more.
                Err(mpsc::RecvTimeoutError::Disconnected) => return Answer::Silence,
            }
        }
    }
}

impl Ask for Console {
    fn can_ask(&self) -> bool {
        Self::can_ask(self)
    }

    fn ask(&self, about: &Asked, within: Duration) -> Answer {
        let what = if about.granted.is_empty() {
            "nothing".to_owned()
        } else {
            about
                .granted
                .iter()
                .map(|p| p.name())
                .collect::<Vec<_>>()
                .join(", ")
        };
        self.confirm(
            &[
                format!("a device wants to open a {} session", about.kind),
                format!(
                    "from     {} ({})",
                    shown(&about.name),
                    shown(&about.fingerprint)
                ),
                format!("allowing {what}"),
            ],
            "allow it?",
            within,
        )
    }
}

/// How a session came to be allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Allowed {
    /// Somebody at the host said yes.
    Accepted,
    /// Somebody said yes recently enough that this did not need asking again.
    StillAccepted,
    /// The device holds [`Permission::Unattended`].
    Unattended,
    /// The device is here on a one-session invitation, which is a consent
    /// somebody at this machine already gave.
    Invited,
}

/// Whether a person was at the host when this session was allowed.
///
/// The distinction the screen locker needs: somebody who answered a prompt is
/// at the machine and would not thank you for locking it under them, and
/// somebody who was never asked is, as far as this host can tell, not there.
#[must_use]
pub const fn somebody_answered(how: Allowed) -> bool {
    match how {
        Allowed::Accepted | Allowed::StillAccepted => true,
        // An invitation was made by a person, minutes or hours ago, and says
        // nothing about whether they are still in the room. Unattended says
        // outright that nobody was asked.
        Allowed::Invited | Allowed::Unattended => false,
    }
}

/// How a session was allowed, for the host's own output.
#[must_use]
pub const fn describe(how: Allowed) -> &'static str {
    match how {
        Allowed::Accepted => "somebody at the host",
        Allowed::StillAccepted => "somebody at the host a moment ago",
        Allowed::Unattended => "the unattended permission",
        Allowed::Invited => "the invitation it arrived on",
    }
}

/// Where the host's idea of the local time comes from.
///
/// A trait so a test can pin the clock: a schedule that only opens on Tuesday
/// morning is otherwise a test that passes once a week.
pub trait LocalTime: Send + Sync + std::fmt::Debug {
    /// The local time now, or why it could not be worked out.
    fn now(&self) -> Result<esper_clock::Civil, String>;
}

/// The machine's own answer, from its own timezone database.
#[derive(Debug, Default)]
pub struct MachineTime;

impl LocalTime for MachineTime {
    /// **Read at every decision, not cached.** A laptop that flew somewhere
    /// has a different local day, and a host that had to be restarted to
    /// notice would enforce yesterday's timezone on today's schedule. It is a
    /// two-kilobyte file read once per session, against a decision that
    /// already involves a network handshake.
    fn now(&self) -> Result<esper_clock::Civil, String> {
        let zone = esper_clock::Zone::local().map_err(|e| e.to_string())?;
        let unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "this machine's clock is set before 1970".to_owned())?
            .as_secs();
        let unix = i64::try_from(unix).map_err(|_| "this machine's clock is absurd".to_owned())?;
        Ok(zone.civil_at(unix))
    }
}

/// Whether the unattended permission applies right now, and why not.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Bounded {
    /// It applies: no schedule, or the schedule allows this moment.
    Applies,
    /// It does not, and this is what to tell the far end.
    Outside(String),
}

/// Grants what the device holds, once somebody has agreed to it.
pub struct AskAtTheHost {
    ask: Box<dyn Ask>,
    /// Where the local time comes from, for reading a device's schedule.
    clock: Box<dyn LocalTime>,
    within: Duration,
    grace: Duration,
    /// When each device was last accepted, and how the last session was
    /// allowed, for whoever reports it.
    recent: Mutex<Vec<([u8; 32], Instant)>>,
    last: Mutex<Option<Allowed>>,
}

impl std::fmt::Debug for AskAtTheHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AskAtTheHost")
            .field("can_ask", &self.ask.can_ask())
            .field("within", &self.within)
            .finish()
    }
}

/// The most devices whose acceptance is remembered.
///
/// A bound because this is written from the network's side of a decision: a
/// host somebody keeps connecting to from many devices must not grow a list
/// per key. The oldest goes, which is also the one closest to expiring.
const MOST_REMEMBERED: usize = 32;

impl AskAtTheHost {
    /// Ask on the host's terminal.
    #[must_use]
    pub fn on_the_terminal() -> Self {
        Self::asking(Box::new(Console::open()))
    }

    /// Ask whoever this says, with the shipping waits.
    #[must_use]
    pub fn asking(ask: Box<dyn Ask>) -> Self {
        Self {
            ask,
            clock: Box::new(MachineTime),
            within: ANSWER_WITHIN,
            grace: GRACE,
            recent: Mutex::new(Vec::new()),
            last: Mutex::new(None),
        }
    }

    /// The same, reading the time from somewhere else.
    ///
    /// For a test with a schedule in it, which otherwise passes on Tuesdays.
    ///
    /// `#[cfg(test)]` for the reason `asking_within` is: this is a binary
    /// crate, so a constructor only the tests call would be dead code in the
    /// shipped build rather than an API somebody might use.
    #[cfg(test)]
    #[must_use]
    pub fn asking_at(ask: Box<dyn Ask>, clock: Box<dyn LocalTime>) -> Self {
        Self {
            ask,
            clock,
            within: ANSWER_WITHIN,
            grace: GRACE,
            recent: Mutex::new(Vec::new()),
            last: Mutex::new(None),
        }
    }

    /// The same, with the waits as arguments.
    ///
    /// Separate so they can be *tested*: a test that proved a
    /// forty-five-second prompt by waiting forty-five seconds is a test
    /// nobody runs.
    ///
    /// `#[cfg(test)]` because this is a binary crate, so a constructor only
    /// the tests call is dead code in the shipped build rather than an API
    /// somebody might use.
    #[cfg(test)]
    #[must_use]
    pub fn asking_within(ask: Box<dyn Ask>, within: Duration, grace: Duration) -> Self {
        Self {
            ask,
            clock: Box::new(MachineTime),
            within,
            grace,
            recent: Mutex::new(Vec::new()),
            last: Mutex::new(None),
        }
    }

    /// Whether the unattended permission applies at this moment.
    ///
    /// [`Bounded::Applies`] for a device with no schedule, whatever the time —
    /// so a host with no timezone database behaves exactly as it did before
    /// schedules existed.
    ///
    /// **A schedule that cannot be evaluated does not apply.** If the
    /// machine's timezone cannot be read, the hours somebody wrote cannot be
    /// checked, and assuming they are satisfied would turn an unreadable file
    /// into unbounded access. The session is not refused for it — the prompt
    /// is still there — but the shortcut past the prompt is.
    fn bounded(&self, device: &Device) -> Bounded {
        let Some(schedule) = device.unattended_when.as_ref() else {
            return Bounded::Applies;
        };
        match self.clock.now() {
            Ok(now) if schedule.allows(&now) => Bounded::Applies,
            Ok(now) => Bounded::Outside(format!(
                "this device may connect unattended {}, and it is {:02}:{:02} on {} here",
                schedule.spelled(),
                now.hour,
                now.minute,
                weekday_name(now.weekday)
            )),
            Err(why) => Bounded::Outside(format!(
                "this device may connect unattended {}, and this host cannot tell what \
                 time it is locally ({why})",
                schedule.spelled()
            )),
        }
    }

    /// Whether this host can ask anybody.
    #[must_use]
    pub fn can_ask(&self) -> bool {
        self.ask.can_ask()
    }

    /// How the last session it allowed came to be allowed.
    #[must_use]
    pub fn how_the_last_one_was_allowed(&self) -> Option<Allowed> {
        self.last.lock().ok().and_then(|g| *g)
    }

    fn remember(&self, key: [u8; 32], now: Instant) {
        let Ok(mut recent) = self.recent.lock() else {
            return;
        };
        recent.retain(|(k, _)| *k != key);
        recent.push((key, now));
        while recent.len() > MOST_REMEMBERED {
            recent.remove(0);
        }
    }

    fn accepted_recently(&self, key: &[u8; 32], now: Instant) -> bool {
        let Ok(mut recent) = self.recent.lock() else {
            return false;
        };
        // Expired entries go on the way past, so the list does not hold keys
        // whose grace ran out days ago.
        recent.retain(|(_, at)| now.saturating_duration_since(*at) < self.grace);
        recent.iter().any(|(k, _)| k == key)
    }

    fn note(&self, how: Allowed) {
        if let Ok(mut last) = self.last.lock() {
            *last = Some(how);
        }
    }
}

impl Policy for AskAtTheHost {
    fn decide(&self, device: &Device, request: &SessionRequest) -> Verdict {
        // What the session would actually get, which is what somebody is being
        // asked to agree to. Asking about the *request* would be asking about
        // something that may not happen: a device requesting the clipboard it
        // does not hold would have a person agreeing to a clipboard.
        let granted: Vec<Permission> = request
            .want
            .iter()
            .copied()
            .filter(|p| device.may(*p))
            .collect();

        // **Not this policy's refusal to make.** An empty grant, or a kind of
        // session the device may not have, is what `decide_and_clamp` is for
        // and it says so better than this could. Asking somebody to agree to
        // a session that is about to be refused anyway would be asking them a
        // question with no consequence.
        if granted.is_empty() {
            return Verdict::Grant(granted);
        }

        // **The permission, and the hours it was granted for.** `unattended`
        // with no schedule means whenever, which is what it has always meant.
        // With one, it applies inside those hours and not outside — and
        // outside them the device is an ordinary paired device rather than a
        // locked-out one, so the prompt below is still its way in.
        let bounded = self.bounded(device);
        if device.may(Permission::Unattended) && bounded == Bounded::Applies {
            self.note(Allowed::Unattended);
            return Verdict::Grant(granted);
        }

        // **An invitation is its own consent.** A one-session pairing was
        // created by somebody at this machine, for one connection, with a
        // deadline -- so the person a prompt would ask is the person who
        // issued it, minutes ago, and asking again is the same redundancy the
        // grace window exists to remove.
        //
        // It is also what keeps the safer path the easier one. Prompting here
        // would make invitations useless on a host with no terminal, and the
        // way round that would be to grant a one-off guest `unattended`
        // permanently -- strictly worse than what it replaced. A control that
        // makes the safe path harder buys less security than it costs.
        //
        // Safe because the *limits* on an invitation do not depend on this:
        // `Pairing::admits` enforces the one session and the expiry, and the
        // session path checks it separately.
        if device.pairing.is_once() {
            self.note(Allowed::Invited);
            return Verdict::Grant(granted);
        }

        let key = device.public_key;
        let now = Instant::now();
        if self.accepted_recently(&key, now) {
            self.note(Allowed::StillAccepted);
            return Verdict::Grant(granted);
        }

        if !self.ask.can_ask() {
            // **The reason names the thing somebody can change.** A device
            // with the permission, refused because it is half past three in
            // the morning, is not a device that "may not connect unattended":
            // saying so would send whoever reads it to grant a permission it
            // already holds. So the schedule is the reason when the schedule
            // is the cause.
            if let Bounded::Outside(why) = bounded {
                return Verdict::Deny(Reason::OUT_OF_SCHEDULE, why);
            }
            return Verdict::Deny(
                Reason::NOBODY_TO_ASK,
                format!(
                    "this host has nobody to ask, and this device may not connect \
                     unattended. On the host: esper-engine grant {} unattended",
                    device.fingerprint().hex()
                ),
            );
        }

        let asked = Asked {
            name: device.name.clone(),
            fingerprint: device.fingerprint().hex(),
            granted: granted.clone(),
            kind: request.kind.name(),
        };
        match self.ask.ask(&asked, self.within) {
            Answer::Yes => {
                self.remember(key, now);
                self.note(Allowed::Accepted);
                Verdict::Grant(granted)
            }
            Answer::No => Verdict::Deny(
                Reason::PROMPT_REFUSED,
                "somebody at the host declined this session".to_owned(),
            ),
            Answer::Silence => Verdict::Deny(
                Reason::PROMPT_TIMEOUT,
                "nobody at the host answered".to_owned(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// An answer decided in advance, and a count of how often it was wanted.
    struct Scripted {
        says: Answer,
        present: bool,
        asked: AtomicUsize,
        about: Mutex<Vec<Asked>>,
    }

    impl Scripted {
        fn saying(says: Answer) -> Self {
            Self {
                says,
                present: true,
                asked: AtomicUsize::new(0),
                about: Mutex::new(Vec::new()),
            }
        }
        fn nobody() -> Self {
            Self {
                says: Answer::Silence,
                present: false,
                asked: AtomicUsize::new(0),
                about: Mutex::new(Vec::new()),
            }
        }
    }

    impl Ask for std::sync::Arc<Scripted> {
        fn can_ask(&self) -> bool {
            self.present
        }
        fn ask(&self, about: &Asked, _within: Duration) -> Answer {
            self.asked.fetch_add(1, Ordering::Relaxed);
            self.about.lock().unwrap().push(about.clone());
            self.says
        }
    }

    fn device(permissions: &[Permission]) -> Device {
        let mut d = Device::new([7u8; 32], "laptop".to_owned(), 0);
        d.permissions = permissions.to_vec();
        d
    }

    /// A clock pinned to a moment, so a test about Tuesday morning does not
    /// pass only on Tuesday morning.
    #[derive(Debug)]
    struct Pinned(esper_clock::Civil);

    impl Pinned {
        fn at(weekday: esper_clock::Weekday, hour: u8, minute: u8) -> Box<dyn LocalTime> {
            Box::new(Self(esper_clock::Civil {
                year: 2026,
                month: 9,
                day: 7,
                hour,
                minute,
                second: 0,
                weekday,
            }))
        }
    }

    impl LocalTime for Pinned {
        fn now(&self) -> Result<esper_clock::Civil, String> {
            Ok(self.0)
        }
    }

    /// A clock that cannot say, which is a machine with no timezone data.
    #[derive(Debug)]
    struct NoIdea;

    impl LocalTime for NoIdea {
        fn now(&self) -> Result<esper_clock::Civil, String> {
            Err("no /etc/localtime".to_owned())
        }
    }

    /// A device with the permission and a schedule.
    fn scheduled(spec: &str) -> Device {
        let mut d = device(&[Permission::View, Permission::Unattended]);
        d.unattended_when = Some(esper_clock::Schedule::parse(spec).expect("a schedule"));
        d
    }

    /// **Inside its hours, an unattended device is not asked about.** Which is
    /// the whole point: a schedule bounds when the permission applies, and
    /// inside the window it applies exactly as it always did.
    #[test]
    fn inside_its_schedule_an_unattended_device_is_let_straight_in() {
        let ask = std::sync::Arc::new(Scripted::saying(Answer::No));
        let policy = AskAtTheHost::asking_at(
            Box::new(std::sync::Arc::clone(&ask)),
            Pinned::at(esper_clock::Weekday::Tuesday, 9, 0),
        );
        let verdict = policy.decide(
            &scheduled("weekdays 08:00-18:00"),
            &wants(&[Permission::View]),
        );
        assert!(matches!(verdict, Verdict::Grant(_)), "{verdict:?}");
        assert_eq!(
            ask.asked.load(Ordering::Relaxed),
            0,
            "nobody should have been asked inside the schedule"
        );
        assert_eq!(
            policy.how_the_last_one_was_allowed(),
            Some(Allowed::Unattended)
        );
    }

    /// **Outside its hours, it is asked about like any other device.** A
    /// schedule bounds the permission, not the device — the machine most
    /// likely to need help at 3am is the one somebody is sitting in front of
    /// at 3am.
    #[test]
    fn outside_its_schedule_it_is_asked_about_like_anybody_else() {
        let ask = std::sync::Arc::new(Scripted::saying(Answer::Yes));
        let policy = AskAtTheHost::asking_at(
            Box::new(std::sync::Arc::clone(&ask)),
            Pinned::at(esper_clock::Weekday::Tuesday, 3, 30),
        );
        let verdict = policy.decide(
            &scheduled("weekdays 08:00-18:00"),
            &wants(&[Permission::View]),
        );
        assert!(matches!(verdict, Verdict::Grant(_)), "{verdict:?}");
        assert_eq!(
            ask.asked.load(Ordering::Relaxed),
            1,
            "somebody should have been asked outside the schedule"
        );
        assert_eq!(
            policy.how_the_last_one_was_allowed(),
            Some(Allowed::Accepted)
        );
    }

    /// And if they say no, it is refused — the prompt is a real question
    /// outside the window rather than a formality.
    #[test]
    fn outside_its_schedule_a_no_is_a_refusal() {
        let ask = std::sync::Arc::new(Scripted::saying(Answer::No));
        let policy = AskAtTheHost::asking_at(
            Box::new(std::sync::Arc::clone(&ask)),
            Pinned::at(esper_clock::Weekday::Saturday, 12, 0),
        );
        let verdict = policy.decide(
            &scheduled("weekdays 08:00-18:00"),
            &wants(&[Permission::View]),
        );
        assert!(
            matches!(verdict, Verdict::Deny(Reason::PROMPT_REFUSED, _)),
            "{verdict:?}"
        );
    }

    /// **The refusal names the schedule, not the permission.** A device
    /// refused at half past three in the morning holds the permission
    /// already; telling somebody to grant it would send them to the wrong
    /// place entirely.
    #[test]
    fn with_nobody_to_ask_the_reason_is_the_schedule_that_caused_it() {
        let policy = AskAtTheHost::asking_at(
            Box::new(std::sync::Arc::new(Scripted::nobody())),
            Pinned::at(esper_clock::Weekday::Tuesday, 3, 30),
        );
        let verdict = policy.decide(
            &scheduled("weekdays 08:00-18:00"),
            &wants(&[Permission::View]),
        );
        let Verdict::Deny(reason, why) = verdict else {
            panic!("expected a refusal, got {verdict:?}");
        };
        assert_eq!(reason, Reason::OUT_OF_SCHEDULE);
        assert!(
            why.contains("weekdays 08:00-18:00")
                && why.contains("03:30")
                && why.contains("Tuesday"),
            "the refusal should say the schedule and the local time: {why:?}"
        );
        assert!(
            !why.contains("grant"),
            "it must not tell somebody to grant a permission the device holds: {why:?}"
        );
    }

    /// A device with no schedule is unchanged: the permission means whenever,
    /// which is what it meant before schedules existed and is right for a
    /// machine in a rack.
    #[test]
    fn a_device_with_no_schedule_is_unbounded_as_it_always_was() {
        let ask = std::sync::Arc::new(Scripted::nobody());
        let policy = AskAtTheHost::asking_at(
            Box::new(std::sync::Arc::clone(&ask)),
            Pinned::at(esper_clock::Weekday::Sunday, 3, 0),
        );
        let device = device(&[Permission::View, Permission::Unattended]);
        assert!(device.unattended_when.is_none());
        let verdict = policy.decide(&device, &wants(&[Permission::View]));
        assert!(matches!(verdict, Verdict::Grant(_)), "{verdict:?}");
    }

    /// **A schedule that cannot be evaluated does not apply.** If the machine
    /// cannot tell what time it is locally, the hours somebody wrote cannot be
    /// checked — and assuming they are satisfied would turn an unreadable file
    /// into unbounded access.
    #[test]
    fn a_schedule_that_cannot_be_evaluated_does_not_let_anybody_in() {
        let policy = AskAtTheHost::asking_at(
            Box::new(std::sync::Arc::new(Scripted::nobody())),
            Box::new(NoIdea),
        );
        let verdict = policy.decide(
            &scheduled("weekdays 08:00-18:00"),
            &wants(&[Permission::View]),
        );
        let Verdict::Deny(reason, why) = verdict else {
            panic!("expected a refusal, got {verdict:?}");
        };
        assert_eq!(reason, Reason::OUT_OF_SCHEDULE);
        assert!(
            why.contains("cannot tell what time"),
            "the refusal should say why it could not be checked: {why:?}"
        );
    }

    /// A schedule on a device **without** the permission changes nothing: the
    /// permission is what grants, and the schedule only bounds it.
    #[test]
    fn a_schedule_without_the_permission_grants_nothing_on_its_own() {
        let ask = std::sync::Arc::new(Scripted::nobody());
        let policy = AskAtTheHost::asking_at(
            Box::new(std::sync::Arc::clone(&ask)),
            Pinned::at(esper_clock::Weekday::Tuesday, 9, 0),
        );
        let mut device = device(&[Permission::View]);
        device.unattended_when = Some(esper_clock::Schedule::parse("always").expect("a schedule"));
        let verdict = policy.decide(&device, &wants(&[Permission::View]));
        assert!(
            matches!(verdict, Verdict::Deny(Reason::NOBODY_TO_ASK, _)),
            "{verdict:?}"
        );
    }

    fn wants(want: &[Permission]) -> SessionRequest {
        SessionRequest {
            kind: esper_proto::SessionKind::Screen,
            want: want.to_vec(),
            codecs: vec!["h264".to_owned()],
            reason: "a screen".to_owned(),
        }
    }

    fn policy(ask: &std::sync::Arc<Scripted>) -> AskAtTheHost {
        AskAtTheHost::asking_within(
            Box::new(std::sync::Arc::clone(ask)),
            Duration::from_millis(50),
            GRACE,
        )
    }

    /// **The whole point: a host with nobody to ask refuses.**
    ///
    /// Not "well, nobody objected". A permission that evaporates when the room
    /// is empty is the opposite of a permission, and before this existed every
    /// paired device could connect to an empty room whether or not it had
    /// been granted `unattended`.
    #[test]
    fn a_host_with_nobody_to_ask_refuses_a_device_that_may_not_connect_unattended() {
        let ask = std::sync::Arc::new(Scripted::nobody());
        let p = policy(&ask);
        let v = p.decide(
            &device(&[Permission::View, Permission::Control]),
            &wants(&[Permission::View]),
        );
        match v {
            Verdict::Deny(reason, detail) => {
                assert_eq!(reason, Reason::NOBODY_TO_ASK);
                // And it says the one command that changes it, because a
                // deployment that stops working for a good reason still has
                // to say which reason.
                assert!(detail.contains("unattended"), "{detail}");
                assert!(detail.contains("grant"), "{detail}");
            }
            Verdict::Grant(g) => panic!("granted {g:?} with nobody to ask"),
        }
        assert_eq!(ask.asked.load(Ordering::Relaxed), 0, "and nobody was asked");
    }

    /// And that is exactly what the permission is for.
    #[test]
    fn a_device_holding_unattended_is_not_asked_about() {
        let ask = std::sync::Arc::new(Scripted::nobody());
        let p = policy(&ask);
        let v = p.decide(
            &device(&[Permission::View, Permission::Unattended]),
            &wants(&[Permission::View]),
        );
        assert_eq!(v, Verdict::Grant(vec![Permission::View]));
        assert_eq!(ask.asked.load(Ordering::Relaxed), 0);
        assert_eq!(
            p.how_the_last_one_was_allowed(),
            Some(Allowed::Unattended),
            "and the host can say which it was"
        );
    }

    /// **Three answers, not two.** Nobody answering is worth trying again when
    /// they are back; somebody declining is not, and a controller told the
    /// wrong one of those does the wrong thing next.
    #[test]
    fn yes_no_and_silence_are_three_different_answers() {
        for (says, expected) in [
            (Answer::Yes, None),
            (Answer::No, Some(Reason::PROMPT_REFUSED)),
            (Answer::Silence, Some(Reason::PROMPT_TIMEOUT)),
        ] {
            let ask = std::sync::Arc::new(Scripted::saying(says));
            let p = policy(&ask);
            let v = p.decide(
                &device(&[Permission::View, Permission::Control]),
                &wants(&[Permission::View, Permission::Control]),
            );
            match (expected, v) {
                (None, Verdict::Grant(g)) => {
                    assert_eq!(g, vec![Permission::View, Permission::Control]);
                    assert_eq!(p.how_the_last_one_was_allowed(), Some(Allowed::Accepted));
                }
                (Some(want), Verdict::Deny(reason, _)) => assert_eq!(reason, want),
                (e, v) => panic!("{says:?} gave {v:?}, expected {e:?}"),
            }
            assert_eq!(ask.asked.load(Ordering::Relaxed), 1);
        }
    }

    /// **Somebody agrees to what will happen, not to what was asked for.**
    ///
    /// A device requesting the clipboard it does not hold would otherwise have
    /// a person agreeing to a clipboard — and then wondering, reasonably, why
    /// the thing they allowed is not what the session does.
    #[test]
    fn the_question_is_about_the_clamped_set_not_the_request() {
        let ask = std::sync::Arc::new(Scripted::saying(Answer::Yes));
        let p = policy(&ask);
        let v = p.decide(
            &device(&[Permission::View]),
            &wants(&[Permission::View, Permission::ClipboardOut]),
        );
        assert_eq!(v, Verdict::Grant(vec![Permission::View]));
        let about = ask.about.lock().unwrap();
        assert_eq!(about.len(), 1);
        let one = about.first().expect("one question");
        assert_eq!(one.granted, vec![Permission::View]);
        // And the key is in the question, so two devices with one name are
        // still two devices.
        assert!(!one.fingerprint.is_empty());
        assert_eq!(one.name, "laptop");
    }

    /// **An invitation is its own consent**, so it is not asked about again.
    ///
    /// The person a prompt would ask is the person who issued it. And
    /// prompting would make invitations useless on a host with no terminal,
    /// where the way round it is to grant a one-off guest `unattended`
    /// permanently — so the control would push people toward something worse
    /// than what it replaced.
    #[test]
    fn a_one_session_invitation_is_not_asked_about() {
        let ask = std::sync::Arc::new(Scripted::nobody());
        let p = policy(&ask);
        let mut d = device(&[Permission::View, Permission::Control]);
        d.pairing = esper_crypto::device::Pairing::Once {
            used: false,
            expires_at: u64::MAX,
        };
        assert!(matches!(
            p.decide(&d, &wants(&[Permission::View])),
            Verdict::Grant(_)
        ));
        assert_eq!(ask.asked.load(Ordering::Relaxed), 0);
        assert_eq!(p.how_the_last_one_was_allowed(), Some(Allowed::Invited));
    }

    /// And an ordinary pairing is still asked about, so the rule above is
    /// about invitations rather than about pairings in general.
    #[test]
    fn an_ordinary_pairing_is_still_asked_about() {
        let ask = std::sync::Arc::new(Scripted::saying(Answer::Yes));
        let p = policy(&ask);
        let d = device(&[Permission::View, Permission::Control]);
        assert_eq!(d.pairing, esper_crypto::device::Pairing::Kept);
        let _ = p.decide(&d, &wants(&[Permission::View]));
        assert_eq!(ask.asked.load(Ordering::Relaxed), 1);
    }

    /// A session that is about to be refused anyway is not worth a question.
    #[test]
    fn nobody_is_asked_about_a_session_that_would_get_nothing() {
        let ask = std::sync::Arc::new(Scripted::saying(Answer::Yes));
        let p = policy(&ask);
        let v = p.decide(&device(&[Permission::View]), &wants(&[Permission::FilesIn]));
        // Left for `decide_and_clamp`, which says why far better than a
        // prompt could.
        assert_eq!(v, Verdict::Grant(vec![]));
        assert_eq!(ask.asked.load(Ordering::Relaxed), 0);
    }

    /// **A reconnect is not a second question.**
    ///
    /// A network blip, a laptop lid, the park-and-resume path: somebody who
    /// accepted eight seconds ago would accept again, and asking twice for one
    /// intention teaches them to answer without reading.
    #[test]
    fn an_acceptance_stands_for_a_moment_so_a_reconnect_is_not_asked_again() {
        let ask = std::sync::Arc::new(Scripted::saying(Answer::Yes));
        let p = policy(&ask);
        let d = device(&[Permission::View, Permission::Control]);
        for _ in 0..3 {
            assert!(matches!(
                p.decide(&d, &wants(&[Permission::View])),
                Verdict::Grant(_)
            ));
        }
        assert_eq!(
            ask.asked.load(Ordering::Relaxed),
            1,
            "one intention, one question"
        );
        assert_eq!(
            p.how_the_last_one_was_allowed(),
            Some(Allowed::StillAccepted),
            "and the host says it was the grace rather than a fresh yes"
        );
    }

    /// And it does not stand for ever.
    #[test]
    fn the_grace_runs_out() {
        let ask = std::sync::Arc::new(Scripted::saying(Answer::Yes));
        let p = AskAtTheHost::asking_within(
            Box::new(std::sync::Arc::clone(&ask)),
            Duration::from_millis(50),
            Duration::from_millis(20),
        );
        let d = device(&[Permission::View, Permission::Control]);
        let _ = p.decide(&d, &wants(&[Permission::View]));
        std::thread::sleep(Duration::from_millis(40));
        let _ = p.decide(&d, &wants(&[Permission::View]));
        assert_eq!(
            ask.asked.load(Ordering::Relaxed),
            2,
            "past the grace, the question is asked again"
        );
    }

    /// A refusal is not remembered as an acceptance, which would be the worst
    /// possible way round.
    #[test]
    fn a_refusal_is_not_remembered() {
        let ask = std::sync::Arc::new(Scripted::saying(Answer::No));
        let p = policy(&ask);
        let d = device(&[Permission::View, Permission::Control]);
        for _ in 0..3 {
            assert!(matches!(
                p.decide(&d, &wants(&[Permission::View])),
                Verdict::Deny(..)
            ));
        }
        assert_eq!(
            ask.asked.load(Ordering::Relaxed),
            3,
            "every attempt is asked about, because none was ever allowed"
        );
    }

    /// The remembered list is bounded: it is written from the network's side
    /// of a decision, so a host somebody connects to from many devices must
    /// not grow a list per key.
    #[test]
    fn what_is_remembered_is_bounded() {
        let ask = std::sync::Arc::new(Scripted::saying(Answer::Yes));
        let p = policy(&ask);
        for n in 0..(MOST_REMEMBERED as u8 + 40) {
            let mut d = device(&[Permission::View, Permission::Control]);
            d.public_key = [n; 32];
            let _ = p.decide(&d, &wants(&[Permission::View]));
        }
        assert!(p.recent.lock().unwrap().len() <= MOST_REMEMBERED);
    }
}

#[cfg(test)]
mod shown_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::{NAME_SHOWN, shown};

    /// **A name cannot draw a second, fake question.**
    ///
    /// The attack is small and complete: a device named with a newline and a
    /// few spaces prints its own `allow it?` line under the real one, and a
    /// person who answers the wrong question has consented to something they
    /// never read. An escape sequence erases the real question instead.
    #[test]
    fn a_hostile_name_cannot_draw_its_own_prompt() {
        let hostile = "laptop\n  \u{2514} allow it? [y/N] \u{1b}[2Koffice-pc";
        let safe = shown(hostile);
        assert!(!safe.contains('\n'), "{safe:?}");
        assert!(!safe.chars().any(char::is_control), "{safe:?}");
        assert!(
            safe.starts_with("laptop"),
            "the real name still survives: {safe:?}"
        );
    }

    /// And it cannot push the question off the screen either.
    #[test]
    fn a_name_shown_in_a_question_is_bounded() {
        let safe = shown(&"a".repeat(4096));
        assert!(
            safe.chars().count() <= NAME_SHOWN + 1,
            "{} characters",
            safe.chars().count()
        );
    }

    /// An empty name is a label rather than a gap: somebody reading the
    /// question should see that the device has no name.
    #[test]
    fn a_name_that_was_only_control_characters_still_reads_as_something() {
        assert_eq!(shown("\u{7}\u{7}"), "an unnamed device");
    }
}

/// A day of the week as somebody would say it.
///
/// Spelled out rather than abbreviated, because this appears in a refusal a
/// person reads once and has to understand immediately.
const fn weekday_name(day: esper_clock::Weekday) -> &'static str {
    match day {
        esper_clock::Weekday::Monday => "Monday",
        esper_clock::Weekday::Tuesday => "Tuesday",
        esper_clock::Weekday::Wednesday => "Wednesday",
        esper_clock::Weekday::Thursday => "Thursday",
        esper_clock::Weekday::Friday => "Friday",
        esper_clock::Weekday::Saturday => "Saturday",
        esper_clock::Weekday::Sunday => "Sunday",
    }
}
