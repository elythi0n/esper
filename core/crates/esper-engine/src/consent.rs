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
use esper_presence::locked::Locked;
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

/// Whether the screen at this machine is locked.
///
/// A trait so the policy can be tested against every answer — including the
/// one that matters most, which is *no answer at all*. A machine whose lock
/// state cannot be established is the case a real deployment meets, and it is
/// the case a test would otherwise never reach.
pub trait LockState: Send + Sync + std::fmt::Debug {
    /// Ask now. Called once per connection attempt by a device that would
    /// otherwise walk straight in, so it may cost a round trip and must not
    /// cost a subscription.
    ///
    /// There is no "nobody asked" implementation of this: a policy with the
    /// bound off holds no probe at all, because the question costs a process
    /// and a display connection and a host nobody switched this on for should
    /// pay for neither.
    fn locked(&self) -> Locked;

    /// The same question, for a session that is already running.
    ///
    /// **Asked every few seconds rather than once**, so it may only use probes
    /// that cost nothing. The expensive one takes the keyboard for a round trip
    /// to find out whether it can, and a keystroke lost every few seconds from
    /// whoever is at the machine would be a policy they switched off.
    ///
    /// Where nothing free can answer, this is [`Locked::CannotTell`] — and the
    /// caller says that the bound applies on arrival and not during a session,
    /// which is a smaller promise and the only honest one there.
    fn locked_cheaply(&self) -> Locked;
}

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

    /// Ask whether a session that is **already running** may do one more
    /// thing, and wait up to `within`.
    ///
    /// A separate question from [`Ask::ask`] because it is a separate
    /// decision, and somebody reading "a device wants to open a screen
    /// session" while watching that device use their screen would rightly
    /// wonder what had happened to the first one.
    ///
    /// `about.granted` is **what saying yes would add** rather than what the
    /// session already holds: what a person is agreeing to is the change.
    fn ask_for_more(&self, about: &Asked, within: Duration) -> Answer;

    /// Where the question would appear, for the host to say when it starts.
    ///
    /// **An operator has to be able to find out where they will be asked.** A
    /// host that will draw a prompt on a screen nobody is watching, or wait
    /// for a line on a terminal that scrolled away, is a host whose sessions
    /// time out for a reason nobody can see from the outside.
    fn asked_at(&self) -> &'static str;
}

/// What a person is being asked about.
#[derive(Debug, Clone)]
pub struct Asked {
    /// What the device is called here.
    pub name: String,
    /// The short form of its key, so two devices with one name are still two.
    ///
    /// Hex, because that is what the device list shows and what `grant` and
    /// `forget` take: this is the identifier somebody *acts* on.
    pub fingerprint: String,
    /// The same key as words, which is the form somebody *compares*.
    ///
    /// Eight words read as four and four is a code somebody checks; sixteen
    /// hex digits is a string somebody's eye slides over. Both are here
    /// because a prompt is where the comparison happens and a command line is
    /// where the action does.
    ///
    /// Written on every platform and read by the prompt that draws it, which
    /// only Linux has a backend for yet — so off Linux this is a field nothing
    /// has reached rather than a field nobody needs. A Windows prompt reads
    /// exactly this.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub words: String,
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

    fn asked_at(&self) -> &'static str {
        "this terminal"
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

    fn ask_for_more(&self, about: &Asked, within: Duration) -> Answer {
        let what = about
            .granted
            .iter()
            .map(|p| p.name())
            .collect::<Vec<_>>()
            .join(", ");
        self.confirm(
            &[
                "a device already watching this screen wants more".to_owned(),
                format!(
                    "from     {} ({})",
                    shown(&about.name),
                    shown(&about.fingerprint)
                ),
                format!("allowing {what}"),
                "for this session only — not saved".to_owned(),
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
    /// How to find out whether the screen is locked, and whether to care.
    ///
    /// `None` is the policy switched off, which is also the answer that costs
    /// nothing: the probe shells out and opens a display connection, and a
    /// host nobody set this on should not pay for either.
    lock: Option<Box<dyn LockState>>,
    within: Duration,
    grace: Duration,
    /// When each device was last accepted, and how the last session was
    /// allowed, for whoever reports it.
    recent: Mutex<Vec<([u8; 32], Instant)>>,
    last: Mutex<Option<Allowed>>,
    /// Whether a session that is already open may ask to be allowed more.
    ///
    /// A standing decision of this machine's rather than an answer to any one
    /// request — see `host.allow_asking`. Off means the question is never put
    /// to anybody, and the far end is told that it is a standing no rather
    /// than a refusal it could try again.
    may_widen: bool,
    /// When each device was last told no by a person at this machine.
    ///
    /// **On the host rather than in the session, and shared by both questions
    /// this machine asks** — may this device open a session, and may a session
    /// already open be widened. A refusal that bound only the thing it was
    /// said about would bind nothing: a session's own count is reset by
    /// reconnecting, and reconnecting is free.
    ///
    /// It is one fact — *somebody here said no to this device* — so one place
    /// holds it and both paths honour it.
    refused: Mutex<Vec<([u8; 32], Instant)>>,
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
    /// Where a question would appear.
    #[must_use]
    pub fn asked_at(&self) -> &'static str {
        self.ask.asked_at()
    }

    /// How long is left of a refusal this device was given, if any is.
    ///
    /// Used by both questions: a device told no about opening a session is not
    /// asked about again for a moment, and neither is one told no about being
    /// allowed more.
    fn refused_recently(&self, key: &[u8; 32], now: Instant) -> Option<Duration> {
        let refused = self.refused.lock().ok()?;
        refused.iter().find_map(|(k, when)| {
            if k != key {
                return None;
            }
            let since = now.saturating_duration_since(*when);
            (since < Knocking::AGAIN_AFTER).then(|| Knocking::AGAIN_AFTER.saturating_sub(since))
        })
    }

    /// Remember that somebody said no to this device.
    ///
    /// Bounded the same way acceptances are, and for the same reason: this is
    /// written from the network's side of a decision, so a host somebody
    /// connects to from many devices must not grow a list per key. The oldest
    /// goes, which is also the one closest to expiring.
    fn note_refusal(&self, key: [u8; 32], now: Instant) {
        let Ok(mut refused) = self.refused.lock() else {
            return;
        };
        refused.retain(|(k, when)| {
            *k != key && now.saturating_duration_since(*when) < Knocking::AGAIN_AFTER
        });
        if refused.len() >= MOST_REMEMBERED {
            refused.remove(0);
        }
        refused.push((key, now));
    }

    /// Whether a session already open may ask for more, as this host has it
    /// set.
    ///
    /// Off unless somebody says so: see `host.allow_asking`, which defaults to
    /// allowing it.
    #[must_use]
    pub const fn never_asked_for_more(mut self) -> Self {
        self.may_widen = false;
        self
    }

    /// Also refuse `unattended` while the screen is locked.
    ///
    /// Off unless a caller says so, and the caller is a setting — see
    /// `policy.deny_when_locked`. What this bounds is the one permission that
    /// removes the question: a device without `unattended` is already asked
    /// about, and a locked screen answers that by nobody being there.
    #[must_use]
    pub fn not_while_locked(mut self, lock: Box<dyn LockState>) -> Self {
        self.lock = Some(lock);
        self
    }

    /// What the lock policy would say right now, or `None` when it is off.
    ///
    /// **For the host to print when it starts.** A policy that fails closed
    /// has to fail visibly: somebody who switched this on and whose machine
    /// cannot be asked deserves to read that on the line above the audit path
    /// rather than to discover it from a refusal at three in the morning.
    #[must_use]
    pub fn lock_state(&self) -> Option<Locked> {
        self.lock.as_ref().map(|l| l.locked())
    }

    /// The same, using only what is free to ask, for a session already running.
    ///
    /// `None` when the bound is off. See [`LockState::locked_cheaply`] for why
    /// a running session gets a different question from an arriving device.
    #[must_use]
    pub fn lock_state_cheaply(&self) -> Option<Locked> {
        self.lock.as_ref().map(|l| l.locked_cheaply())
    }

    /// Ask whoever this says, waiting `within`.
    ///
    /// **A setting rather than a constant**, because the right answer is
    /// somebody's own: a machine on a desk wants less than a machine in
    /// another room. Bounded by the settings themselves, so nothing here has
    /// to defend against a prompt that vanishes before it can be read.
    #[must_use]
    pub fn asking_for(ask: Box<dyn Ask>, within: Duration) -> Self {
        Self {
            within,
            ..Self::asking(ask)
        }
    }

    /// Ask whoever this says, with the shipping waits.
    #[must_use]
    pub fn asking(ask: Box<dyn Ask>) -> Self {
        Self {
            ask,
            clock: Box::new(MachineTime),
            lock: None,
            within: ANSWER_WITHIN,
            grace: GRACE,
            recent: Mutex::new(Vec::new()),
            last: Mutex::new(None),
            may_widen: true,
            refused: Mutex::new(Vec::new()),
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
            lock: None,
            within: ANSWER_WITHIN,
            grace: GRACE,
            recent: Mutex::new(Vec::new()),
            last: Mutex::new(None),
            may_widen: true,
            refused: Mutex::new(Vec::new()),
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
            lock: None,
            within,
            grace,
            recent: Mutex::new(Vec::new()),
            last: Mutex::new(None),
            may_widen: true,
            refused: Mutex::new(Vec::new()),
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

/// The permissions a session that is already open may **ask** to be allowed.
///
/// **A session may ask to do more of what it is, not to become something
/// else.** Everything left out is left out for a stated reason rather than
/// caution:
///
/// - `view` is what a screen session *is*, so a session that has it cannot ask
///   for it and one that has lost it is ending rather than narrowing.
/// - `terminal` and `forward` open a different kind of session, each with its
///   own request and its own prompt at the moment it happens. A yes here would
///   be somebody agreeing to a shell in a dialog about a screen.
/// - `unattended`, `logoff` and `reboot` **outlive the session**. The first
///   lets the next connection in without anybody being asked; the other two
///   take the machine away. None of them is a thing to hand over in the middle
///   of something else, and all three are what `esper grant --yes` is for.
/// - `lock` is a verb a controller asks for when it wants it, and is refused
///   or allowed then.
/// - `record` is announced by the controller rather than granted by the host —
///   the honest limit is written up where the recording is.
const MAY_BE_ASKED_FOR: [Permission; 6] = [
    Permission::Control,
    Permission::ClipboardOut,
    Permission::ClipboardIn,
    Permission::FilesOut,
    Permission::FilesIn,
    Permission::Audio,
];

/// What the host says about a session asking to be allowed one more thing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Widening {
    /// It may, from now until this session ends.
    ///
    /// `asked` says whether a person was put to the question. False means the
    /// device already holds the permission in the store and this session
    /// simply did not open with it — which is not an escalation at all, and is
    /// a different line in the log.
    Allowed { asked: bool },
    /// It already may, so nothing changed and nobody was disturbed.
    AlreadyHeld,
    /// It may not, with a code the far end can act on and a sentence a person
    /// can read.
    Refused(Reason, String),
}

/// How often one session may put a question to whoever is at the machine.
///
/// **A limit on questions, not on permissions.** Every ask draws a prompt in
/// front of somebody, and a session that could ask without end could stop them
/// working — while the person answering has no way to say "and stop asking".
/// Three, because the honest reasons to ask twice are a mistyped answer and a
/// second permission, and nothing needs a fourth.
///
/// The clock is a parameter for the reason it is everywhere else in this tree:
/// a test that proved a thirty-second wait by waiting thirty seconds is a test
/// nobody runs.
#[derive(Debug, Default)]
pub struct Knocking {
    put: usize,
}

impl Knocking {
    /// The most questions one session may put.
    pub const MOST: usize = 3;

    /// How long after a refusal before another may be put.
    ///
    /// **Because "no" needs to mean something for a moment.** A session that
    /// re-asked the instant somebody clicked refuse would be a session that
    /// took the answer as a starting position.
    pub const AGAIN_AFTER: Duration = Duration::from_secs(30);

    /// Nothing asked yet.
    #[must_use]
    pub const fn new() -> Self {
        Self { put: 0 }
    }

    /// Whether this session has any questions left.
    ///
    /// The clock is a parameter for the reason it is everywhere else in this
    /// tree, even where this one does not read it yet: the cooling-off after a
    /// refusal is kept by the host — see
    /// [`AskAtTheHost::refused_more_recently`] — because a session's own count
    /// is reset by reconnecting.
    fn may_put(&self, _now: Instant) -> Result<(), (Reason, String)> {
        if self.put >= Self::MOST {
            return Err((
                Reason::ASKED_ENOUGH,
                format!(
                    "this session has asked {} times, which is as often as it may. A \
                     permission that is wanted from now on is one to grant at the host",
                    Self::MOST
                ),
            ));
        }
        Ok(())
    }
}

impl AskAtTheHost {
    /// Whether a session already open may be allowed one more thing.
    ///
    /// **Everything about this decision is here**, so there is one place to
    /// read the rule and one place to test it: what may be asked for at all,
    /// whether this machine puts such questions, whether anybody is there,
    /// how often one session may ask, and what a person said.
    ///
    /// `holds` is what the *session* may do now, and `device` is what the
    /// store says — the two differ, and the difference is the whole point: a
    /// session that did not ask for something its device already holds is not
    /// escalating by asking for it now.
    ///
    /// **Nothing here writes to the store.** A yes is about the session in
    /// front of somebody; a permission that outlived it would be one nobody
    /// agreed to.
    pub fn widen(
        &self,
        device: &Device,
        wanted: Permission,
        holds: &[Permission],
        knocking: &mut Knocking,
        now: Instant,
    ) -> Widening {
        if holds.contains(&wanted) {
            return Widening::AlreadyHeld;
        }
        if !MAY_BE_ASKED_FOR.contains(&wanted) {
            return Widening::Refused(
                Reason::PERMISSION_DENIED,
                format!(
                    "{} is not something a session that is already open may ask for. On \
                     the host: esper-engine grant {} {}",
                    wanted.name(),
                    device.fingerprint().hex(),
                    wanted.name()
                ),
            );
        }
        if !self.may_widen {
            return Widening::Refused(
                Reason::NO_ASKING_HERE,
                "this host does not let a session ask for more than it opened with. On \
                 the host: host.allow_asking"
                    .to_owned(),
            );
        }
        // **Already granted at the machine, so there is nobody to ask.** The
        // person whose machine it is has said yes to this permission already,
        // in the store, deliberately; a session that simply did not ask for it
        // when it opened is not escalating by asking now, and putting the
        // question again would teach somebody that these prompts are noise.
        if device.may(wanted) {
            return Widening::Allowed { asked: false };
        }
        if let Err((reason, detail)) = knocking.may_put(now) {
            return Widening::Refused(reason, detail);
        }
        if let Some(left) = self.refused_recently(&device.public_key, now) {
            // **A refusal is the host's, not the session's.** The count above
            // is per session, and a device that could reconnect would get a
            // fresh one — so a "no" that only bound the session it was said
            // in would bind nothing at all for a device holding `unattended`,
            // which opens sessions without anybody being asked. Found by the
            // red-team pass, which reconnected.
            return Widening::Refused(
                Reason::ASKED_ENOUGH,
                format!(
                    "somebody at the host said no to this device; another question may be \
                     put in {}s",
                    left.as_secs().max(1)
                ),
            );
        }
        // **Counted here, before anybody is asked rather than after.** A
        // question a host cannot put to anybody still costs it a store read
        // and a thread, and a session that could ask for ever because nobody
        // was there to answer would be a session spinning that for as long as
        // it stayed open.
        knocking.put = knocking.put.saturating_add(1);
        if !self.ask.can_ask() {
            return Widening::Refused(
                Reason::NOBODY_TO_ASK,
                format!(
                    "this host has nobody to ask, so nothing can be allowed that was not \
                     granted already. On the host: esper-engine grant {} {}",
                    device.fingerprint().hex(),
                    wanted.name()
                ),
            );
        }
        let asked = Asked {
            name: device.name.clone(),
            fingerprint: device.fingerprint().hex(),
            words: device.fingerprint().inline(),
            // What a yes allows, which is what somebody is agreeing to.
            granted: vec![wanted],
            kind: esper_proto::SessionKind::Screen.name(),
        };
        match self.ask.ask_for_more(&asked, self.within) {
            Answer::Yes => Widening::Allowed { asked: true },
            Answer::No => {
                self.note_refusal(device.public_key, now);
                Widening::Refused(
                    Reason::PROMPT_REFUSED,
                    "somebody at the host said no".to_owned(),
                )
            }
            // **Not counted as a refusal.** Nobody said no; nobody was there
            // to say anything, and a cooling-off period after silence would
            // punish the far end for the host's screen being unattended.
            Answer::Silence => Widening::Refused(
                Reason::PROMPT_TIMEOUT,
                "nobody at the host answered".to_owned(),
            ),
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
            // **And not while the screen is locked, where somebody asked for
            // that bound.** A locked screen is the closest thing a machine has
            // to "nobody is at this desk", and an operator who wants
            // `unattended` for the hours they are present wants it to stop
            // when they lock up and walk away.
            //
            // A state that **cannot be established counts as locked**. The
            // alternative is a policy that silently does nothing on a machine
            // that cannot answer, which is the worst of the three outcomes;
            // the host says which it is when it starts, so nobody meets this
            // for the first time in a refusal.
            if let Some(lock) = &self.lock {
                let state = lock.locked();
                if !matches!(state, Locked::No(_)) {
                    return Verdict::Deny(
                        Reason::HOST_IS_LOCKED,
                        format!(
                            "the screen at this host is {} and policy.deny_when_locked is on, \
                             so this device may not walk in unasked. It can when somebody \
                             unlocks the machine",
                            state.describe()
                        ),
                    );
                }
            }
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

        // **A refusal stands for a moment, without asking again.** The
        // question has been put to somebody and answered; putting it again
        // because the far end reconnected would make the answer worthless and
        // the machine unusable.
        if let Some(left) = self.refused_recently(&key, now) {
            return Verdict::Deny(
                Reason::PROMPT_REFUSED,
                format!(
                    "somebody at the host declined this device; it may be asked about again \
                     in {}s",
                    left.as_secs().max(1)
                ),
            );
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
            words: device.fingerprint().inline(),
            granted: granted.clone(),
            kind: request.kind.name(),
        };
        match self.ask.ask(&asked, self.within) {
            Answer::Yes => {
                self.remember(key, now);
                self.note(Allowed::Accepted);
                Verdict::Grant(granted)
            }
            Answer::No => {
                // **Remembered, or "no" means nothing.** An acceptance has
                // been remembered since the grace window existed, so a
                // reconnect inside it is not asked about twice — and a
                // refusal was remembered nowhere at all, so the same
                // reconnect put the dialog straight back up. A paired device
                // with no permissions could make a screen unusable that way,
                // as fast as it could open a connection.
                self.note_refusal(key, now);
                Verdict::Deny(
                    Reason::PROMPT_REFUSED,
                    "somebody at the host declined this session".to_owned(),
                )
            }
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
        fn ask_for_more(&self, about: &Asked, within: Duration) -> Answer {
            // The same script answers both questions: what a test about
            // widening is checking is the rule, and a second script would let
            // the two drift.
            self.ask(about, within)
        }
        fn asked_at(&self) -> &'static str {
            "a script"
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

    // # Asking to be allowed more, mid-session
    //
    // The rule has one place — `AskAtTheHost::widen` — and these hold it to
    // every branch of it. What they cannot check is the wording on somebody's
    // screen; that is `esper-dialog`'s own tests and the smoke suite's amber
    // pixels.

    /// A session with `view` asks for `control`, and somebody says yes.
    #[test]
    fn a_person_at_the_host_can_widen_one_session() {
        let ask = std::sync::Arc::new(Scripted::saying(Answer::Yes));
        let policy = AskAtTheHost::asking(Box::new(std::sync::Arc::clone(&ask)));
        let mut knocking = Knocking::new();
        let answer = policy.widen(
            &device(&[Permission::View]),
            Permission::Control,
            &[Permission::View],
            &mut knocking,
            Instant::now(),
        );
        assert_eq!(answer, Widening::Allowed { asked: true });
        assert_eq!(ask.asked.load(Ordering::Relaxed), 1);
        // **And what they were asked about was the change**, not the whole
        // set: agreeing to "view, control" when the session already has view
        // is a question about something that has already happened.
        let put = ask.about.lock().unwrap();
        assert_eq!(put.len(), 1);
        let only = put.first().expect("one question");
        assert_eq!(only.granted, vec![Permission::Control]);
    }

    /// **And it is one session's answer, so nothing is written down.** The
    /// rule cannot write to the store — it takes the device record by
    /// reference and hands back a verdict — and this holds that shape: the
    /// permissions on the record are the same afterwards.
    #[test]
    fn widening_a_session_does_not_change_what_the_device_may_do() {
        let ask = std::sync::Arc::new(Scripted::saying(Answer::Yes));
        let policy = AskAtTheHost::asking(Box::new(std::sync::Arc::clone(&ask)));
        let record = device(&[Permission::View]);
        let mut knocking = Knocking::new();
        let answer = policy.widen(
            &record,
            Permission::Control,
            &[Permission::View],
            &mut knocking,
            Instant::now(),
        );
        assert_eq!(answer, Widening::Allowed { asked: true });
        assert_eq!(record.permissions, vec![Permission::View]);
    }

    /// Somebody saying no is a refusal, and it holds for a while: a session
    /// that re-asked the instant the answer came would treat "no" as an
    /// opening position.
    #[test]
    fn a_refusal_holds_for_long_enough_to_mean_something() {
        let ask = std::sync::Arc::new(Scripted::saying(Answer::No));
        let policy = AskAtTheHost::asking(Box::new(std::sync::Arc::clone(&ask)));
        let mut knocking = Knocking::new();
        let now = Instant::now();
        let first = policy.widen(
            &device(&[Permission::View]),
            Permission::Control,
            &[Permission::View],
            &mut knocking,
            now,
        );
        assert!(
            matches!(first, Widening::Refused(Reason::PROMPT_REFUSED, _)),
            "{first:?}"
        );
        let again = policy.widen(
            &device(&[Permission::View]),
            Permission::Control,
            &[Permission::View],
            &mut knocking,
            now + Duration::from_secs(1),
        );
        assert!(
            matches!(again, Widening::Refused(Reason::ASKED_ENOUGH, _)),
            "{again:?}"
        );
        assert_eq!(
            ask.asked.load(Ordering::Relaxed),
            1,
            "nobody should have been asked a second time inside the cooling-off period"
        );
        // And afterwards the question may be put again.
        let later = policy.widen(
            &device(&[Permission::View]),
            Permission::Control,
            &[Permission::View],
            &mut knocking,
            now + Knocking::AGAIN_AFTER + Duration::from_secs(1),
        );
        assert!(
            matches!(later, Widening::Refused(Reason::PROMPT_REFUSED, _)),
            "{later:?}"
        );
        assert_eq!(ask.asked.load(Ordering::Relaxed), 2);
    }

    /// **A refusal outlives the session it was said in**, because a session's
    /// own count is reset by reconnecting — and a device holding `unattended`
    /// reconnects without anybody being asked, so a "no" that bound only one
    /// session would bind nothing at all.
    #[test]
    fn a_refusal_is_not_reset_by_opening_another_session() {
        let ask = std::sync::Arc::new(Scripted::saying(Answer::No));
        let policy = AskAtTheHost::asking(Box::new(std::sync::Arc::clone(&ask)));
        let now = Instant::now();

        let mut first = Knocking::new();
        let said_no = policy.widen(
            &device(&[Permission::View]),
            Permission::Control,
            &[Permission::View],
            &mut first,
            now,
        );
        assert!(
            matches!(said_no, Widening::Refused(Reason::PROMPT_REFUSED, _)),
            "{said_no:?}"
        );

        // A new session: a new count, the same host.
        let mut second = Knocking::new();
        let again = policy.widen(
            &device(&[Permission::View]),
            Permission::Control,
            &[Permission::View],
            &mut second,
            now + Duration::from_secs(1),
        );
        assert!(
            matches!(again, Widening::Refused(Reason::ASKED_ENOUGH, _)),
            "reconnecting got the question asked again: {again:?}"
        );
        assert_eq!(
            ask.asked.load(Ordering::Relaxed),
            1,
            "nobody should have been asked twice"
        );

        // And it does not last for ever: the cooling-off is a bound, not a ban.
        let mut third = Knocking::new();
        let later = policy.widen(
            &device(&[Permission::View]),
            Permission::Control,
            &[Permission::View],
            &mut third,
            now + Knocking::AGAIN_AFTER + Duration::from_secs(1),
        );
        assert!(
            matches!(later, Widening::Refused(Reason::PROMPT_REFUSED, _)),
            "{later:?}"
        );
    }

    /// **A question nobody can answer still counts against the session.**
    /// Otherwise a session on a host with nobody at it could put the question
    /// for as long as it stayed open, and each one costs a store read and a
    /// thread — measured against, since nothing else would ever stop it.
    #[test]
    fn a_host_with_nobody_to_ask_still_bounds_the_asking() {
        let policy = AskAtTheHost::asking(Box::new(std::sync::Arc::new(Scripted::nobody())));
        let mut knocking = Knocking::new();
        let now = Instant::now();
        for _ in 0..Knocking::MOST {
            let answer = policy.widen(
                &device(&[Permission::View]),
                Permission::Control,
                &[Permission::View],
                &mut knocking,
                now,
            );
            assert!(
                matches!(answer, Widening::Refused(Reason::NOBODY_TO_ASK, _)),
                "{answer:?}"
            );
        }
        let enough = policy.widen(
            &device(&[Permission::View]),
            Permission::Control,
            &[Permission::View],
            &mut knocking,
            now,
        );
        assert!(
            matches!(enough, Widening::Refused(Reason::ASKED_ENOUGH, _)),
            "a session with nobody to ask asked a fourth time: {enough:?}"
        );
    }

    /// **Nobody answering is not somebody saying no**, so it starts no
    /// cooling-off period: the far end would otherwise be punished for the
    /// host's screen being unattended, and the reason code is the one that
    /// says to try again.
    #[test]
    fn silence_is_not_a_refusal_and_may_be_asked_again_at_once() {
        let ask = std::sync::Arc::new(Scripted::saying(Answer::Silence));
        let policy = AskAtTheHost::asking(Box::new(std::sync::Arc::clone(&ask)));
        let mut knocking = Knocking::new();
        let now = Instant::now();
        for _ in 0..2 {
            let answer = policy.widen(
                &device(&[Permission::View]),
                Permission::Control,
                &[Permission::View],
                &mut knocking,
                now,
            );
            assert!(
                matches!(answer, Widening::Refused(Reason::PROMPT_TIMEOUT, _)),
                "{answer:?}"
            );
        }
        assert_eq!(ask.asked.load(Ordering::Relaxed), 2);
    }

    /// **Three questions, and no more.** Every one draws a prompt in front of
    /// whoever is at the machine, and the person answering has no way to say
    /// "and stop asking".
    #[test]
    fn one_session_may_put_only_so_many_questions() {
        let ask = std::sync::Arc::new(Scripted::saying(Answer::Silence));
        let policy = AskAtTheHost::asking(Box::new(std::sync::Arc::clone(&ask)));
        let mut knocking = Knocking::new();
        let now = Instant::now();
        for _ in 0..Knocking::MOST {
            let answer = policy.widen(
                &device(&[Permission::View]),
                Permission::Control,
                &[Permission::View],
                &mut knocking,
                now,
            );
            assert!(
                matches!(answer, Widening::Refused(Reason::PROMPT_TIMEOUT, _)),
                "{answer:?}"
            );
        }
        let enough = policy.widen(
            &device(&[Permission::View]),
            Permission::Control,
            &[Permission::View],
            &mut knocking,
            now,
        );
        assert!(
            matches!(enough, Widening::Refused(Reason::ASKED_ENOUGH, _)),
            "{enough:?}"
        );
        assert_eq!(
            ask.asked.load(Ordering::Relaxed),
            Knocking::MOST,
            "the limit has to stop the question being put, not just its answer being used"
        );
    }

    /// A host with nobody to ask says so, and the reason names what to do
    /// about it — the same shape the session refusal has.
    #[test]
    fn with_nobody_to_ask_nothing_can_be_widened() {
        let policy = AskAtTheHost::asking(Box::new(std::sync::Arc::new(Scripted::nobody())));
        let mut knocking = Knocking::new();
        let answer = policy.widen(
            &device(&[Permission::View]),
            Permission::Control,
            &[Permission::View],
            &mut knocking,
            Instant::now(),
        );
        let Widening::Refused(reason, why) = answer else {
            panic!("expected a refusal");
        };
        assert_eq!(reason, Reason::NOBODY_TO_ASK);
        assert!(why.contains("grant"), "{why:?}");
    }

    /// **A host that never asks refuses without asking**, and says it is a
    /// standing answer rather than one worth trying again.
    #[test]
    fn a_host_with_asking_switched_off_puts_no_question() {
        let ask = std::sync::Arc::new(Scripted::saying(Answer::Yes));
        let policy =
            AskAtTheHost::asking(Box::new(std::sync::Arc::clone(&ask))).never_asked_for_more();
        let mut knocking = Knocking::new();
        let answer = policy.widen(
            &device(&[Permission::View]),
            Permission::Control,
            &[Permission::View],
            &mut knocking,
            Instant::now(),
        );
        assert!(
            matches!(answer, Widening::Refused(Reason::NO_ASKING_HERE, _)),
            "{answer:?}"
        );
        assert_eq!(ask.asked.load(Ordering::Relaxed), 0);
    }

    /// **The things a session may not ask for are refused before anybody is
    /// disturbed**, each for its own reason — see `MAY_BE_ASKED_FOR`.
    #[test]
    fn a_session_cannot_ask_for_what_outlives_it_or_replaces_it() {
        let ask = std::sync::Arc::new(Scripted::saying(Answer::Yes));
        let policy = AskAtTheHost::asking(Box::new(std::sync::Arc::clone(&ask)));
        for wanted in [
            Permission::Unattended,
            Permission::Reboot,
            Permission::Logoff,
            Permission::Terminal,
            Permission::Forward,
            Permission::Lock,
            Permission::Record,
        ] {
            let mut knocking = Knocking::new();
            let answer = policy.widen(
                &device(&[Permission::View]),
                wanted,
                &[Permission::View],
                &mut knocking,
                Instant::now(),
            );
            let Widening::Refused(reason, why) = answer else {
                panic!("{} should not be askable, got {answer:?}", wanted.name());
            };
            assert_eq!(reason, Reason::PERMISSION_DENIED, "{}", wanted.name());
            assert!(why.contains(wanted.name()), "{why:?}");
        }
        assert_eq!(
            ask.asked.load(Ordering::Relaxed),
            0,
            "nobody should be asked a question whose answer cannot be honoured"
        );
    }

    /// **A device that already holds it needs no question.** The person whose
    /// machine it is granted the permission deliberately; a session that did
    /// not ask for it when it opened is not escalating by asking now, and a
    /// prompt here would teach somebody that these are noise.
    #[test]
    fn a_permission_the_device_already_holds_needs_nobody_asked() {
        let ask = std::sync::Arc::new(Scripted::saying(Answer::No));
        let policy = AskAtTheHost::asking(Box::new(std::sync::Arc::clone(&ask)));
        let mut knocking = Knocking::new();
        let answer = policy.widen(
            &device(&[Permission::View, Permission::Control]),
            Permission::Control,
            // The session opened without it, which is the case this is about.
            &[Permission::View],
            &mut knocking,
            Instant::now(),
        );
        assert_eq!(answer, Widening::Allowed { asked: false });
        assert_eq!(ask.asked.load(Ordering::Relaxed), 0);
    }

    /// Asking for what the session already has changes nothing and disturbs
    /// nobody — and does not count against the limit, because a controller
    /// that lost track of its own state must not be able to spend somebody
    /// else's patience with it.
    #[test]
    fn asking_for_what_a_session_already_has_is_a_no_op() {
        let ask = std::sync::Arc::new(Scripted::saying(Answer::Yes));
        let policy = AskAtTheHost::asking(Box::new(std::sync::Arc::clone(&ask)));
        let mut knocking = Knocking::new();
        for _ in 0..Knocking::MOST + 2 {
            let answer = policy.widen(
                &device(&[Permission::View, Permission::Control]),
                Permission::Control,
                &[Permission::View, Permission::Control],
                &mut knocking,
                Instant::now(),
            );
            assert_eq!(answer, Widening::AlreadyHeld);
        }
        assert_eq!(ask.asked.load(Ordering::Relaxed), 0);
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

    /// A lock state decided in advance, and a count of how often it was asked.
    #[derive(Debug)]
    struct Screen {
        says: Locked,
        asked: AtomicUsize,
    }

    impl LockState for Screen {
        fn locked(&self) -> Locked {
            self.asked.fetch_add(1, Ordering::Relaxed);
            self.says
        }
        fn locked_cheaply(&self) -> Locked {
            self.locked()
        }
    }

    fn screen(says: Locked) -> std::sync::Arc<Screen> {
        std::sync::Arc::new(Screen {
            says,
            asked: AtomicUsize::new(0),
        })
    }

    impl LockState for std::sync::Arc<Screen> {
        fn locked(&self) -> Locked {
            (**self).locked()
        }
        fn locked_cheaply(&self) -> Locked {
            (**self).locked_cheaply()
        }
    }

    fn unattended() -> Device {
        device(&[
            Permission::View,
            Permission::Control,
            Permission::Unattended,
        ])
    }

    /// **A locked screen stops a device walking in unasked.**
    ///
    /// The bound the whole policy exists for: `unattended` for the hours
    /// somebody is at the machine, and not for the hours after they lock it
    /// and leave.
    #[test]
    fn a_locked_screen_refuses_an_unattended_device() {
        let ask = std::sync::Arc::new(Scripted::nobody());
        let seen = screen(Locked::Yes("a test"));
        let p = policy(&ask).not_while_locked(Box::new(std::sync::Arc::clone(&seen)));

        let v = p.decide(&unattended(), &wants(&[Permission::View]));
        let Verdict::Deny(code, detail) = v else {
            panic!("a locked screen let an unattended device in: {v:?}");
        };
        assert_eq!(code, Reason::HOST_IS_LOCKED);
        // The refusal says which state, which setting, and what changes it.
        assert!(detail.contains("locked"), "{detail}");
        assert!(detail.contains("policy.deny_when_locked"), "{detail}");
        assert!(detail.contains("unlocks"), "{detail}");
        assert_eq!(seen.asked.load(Ordering::Relaxed), 1);
    }

    /// And an unlocked screen does not.
    ///
    /// Its own test, because a policy that refused every unattended device
    /// would pass the one above while removing the permission entirely.
    #[test]
    fn an_unlocked_screen_lets_an_unattended_device_in() {
        let ask = std::sync::Arc::new(Scripted::nobody());
        let seen = screen(Locked::No("a test"));
        let p = policy(&ask).not_while_locked(Box::new(std::sync::Arc::clone(&seen)));

        let v = p.decide(&unattended(), &wants(&[Permission::View]));
        assert!(matches!(v, Verdict::Grant(_)), "{v:?}");
        assert_eq!(p.how_the_last_one_was_allowed(), Some(Allowed::Unattended));
    }

    /// **A state that cannot be established counts as locked.**
    ///
    /// The case a real deployment meets, and the one a test would otherwise
    /// never reach. The alternative is a policy somebody switched on that
    /// silently does nothing — and they find that out at three in the morning
    /// rather than when the host started.
    #[test]
    fn a_lock_state_nobody_can_establish_counts_as_locked() {
        let ask = std::sync::Arc::new(Scripted::nobody());
        let seen = screen(Locked::CannotTell);
        let p = policy(&ask).not_while_locked(Box::new(std::sync::Arc::clone(&seen)));

        let v = p.decide(&unattended(), &wants(&[Permission::View]));
        let Verdict::Deny(code, detail) = v else {
            panic!("an unanswerable lock state let a device in: {v:?}");
        };
        assert_eq!(code, Reason::HOST_IS_LOCKED);
        assert!(detail.contains("cannot be established"), "{detail}");
    }

    /// The bound is off unless somebody asked for it.
    #[test]
    fn without_the_policy_the_screen_is_never_asked_about() {
        let ask = std::sync::Arc::new(Scripted::nobody());
        let p = policy(&ask);
        assert_eq!(p.lock_state(), None, "a policy with no bound reported one");
        let v = p.decide(&unattended(), &wants(&[Permission::View]));
        assert!(matches!(v, Verdict::Grant(_)), "{v:?}");
    }

    /// **It bounds `unattended` and nothing else.**
    ///
    /// A device without that permission is asked about, and a locked screen
    /// answers that question by nobody being there — so refusing it here
    /// would replace a prompt that reaches the right conclusion with a
    /// refusal that names the wrong reason.
    #[test]
    fn a_device_without_unattended_is_still_asked_about() {
        let ask = std::sync::Arc::new(Scripted::saying(Answer::Yes));
        let seen = screen(Locked::Yes("a test"));
        let p = policy(&ask).not_while_locked(Box::new(std::sync::Arc::clone(&seen)));

        let v = p.decide(
            &device(&[Permission::View, Permission::Control]),
            &wants(&[Permission::View]),
        );
        assert!(matches!(v, Verdict::Grant(_)), "{v:?}");
        assert_eq!(ask.asked.load(Ordering::Relaxed), 1, "it was not asked");
        assert_eq!(
            seen.asked.load(Ordering::Relaxed),
            0,
            "the screen was asked about for a device the policy does not bound"
        );
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

    /// **A refusal is never an acceptance, and it is remembered as a
    /// refusal.**
    ///
    /// The first half is the one that would be catastrophic the other way
    /// round, and it has always held. The second half did not: a refusal was
    /// remembered *nowhere*, so a device could reconnect and put the dialog
    /// straight back up — as fast as it could open a connection, from a
    /// paired device with no permissions at all. This test asserted that
    /// behaviour ("every attempt is asked about") and so wrote the defect
    /// down as intended.
    #[test]
    fn a_refusal_is_never_an_acceptance_and_it_holds_for_a_moment() {
        let ask = std::sync::Arc::new(Scripted::saying(Answer::No));
        let p = policy(&ask);
        let d = device(&[Permission::View, Permission::Control]);
        for _ in 0..3 {
            assert!(
                matches!(p.decide(&d, &wants(&[Permission::View])), Verdict::Deny(..)),
                "a refusal must never come back as a grant"
            );
        }
        assert_eq!(
            ask.asked.load(Ordering::Relaxed),
            1,
            "the question was put to somebody and answered; asking again because the far \
             end reconnected makes the answer worthless"
        );

        // And it is a bound rather than a ban: past the cooling-off, the
        // question is put again, because a person's "no" was about a moment.
        p.note_refusal(d.public_key, Instant::now() - Knocking::AGAIN_AFTER);
        assert!(matches!(
            p.decide(&d, &wants(&[Permission::View])),
            Verdict::Deny(..)
        ));
        assert_eq!(ask.asked.load(Ordering::Relaxed), 2);
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
