//! The host's indicator and panic key, on a thread of their own.
//!
//! # Why a thread
//!
//! The indicator and the panic key belong to one X connection, and that
//! connection has to be watched continuously: **the panic key is the one thing
//! in this product where a delay is measured against somebody's alarm.** A
//! session loop that polled it between frames would answer it whenever the
//! next frame happened to arrive.
//!
//! So a thread owns the connection, polls it every few tens of milliseconds,
//! and talks to the rest of the engine through two channels: what to show goes
//! in, and the panic goes out. The polling costs a wake-up every thirty
//! milliseconds doing almost nothing, which is the price of not having to
//! reason about how long the session loop might be busy.

// Nothing in this module runs on a platform with no backend for it: `start`
// refuses and the session says so. It is still compiled everywhere, which is
// deliberate — the thread below is generic over the trait, so it is the part a
// new platform reuses rather than rewrites, and compiling it everywhere is
// what keeps it honest.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use esper_presence::{HostEvent, Missing, Presence, Verdict, Watching};

/// How often the indicator's connection is looked at.
const TICK: Duration = Duration::from_millis(30);

/// How long a session waits for the indicator's thread to answer a question.
///
/// Only the blanking questions wait at all — showing and hiding are told, not
/// asked. Two seconds is a round trip to a display server on a loaded machine,
/// and the alternative to a bound is a session that hangs for ever because a
/// thread holding an X connection stopped answering.
const ANSWER_PATIENCE: Duration = Duration::from_secs(2);

/// How often the screen is asked whether the indicator is on it.
///
/// **A second, which is a claim about how long a session may run with a hidden
/// indicator.** Faster costs a round trip to the display server for nothing
/// most of the time; slower is a second of somebody's screen being watched
/// with nothing on it saying so, and this is the one guarantee where that
/// interval *is* the guarantee.
const VERIFY_EVERY: Duration = Duration::from_secs(1);

/// What the screen last said about the indicator.
///
/// **Not a boolean**, because "it is not there" splits into cases a caller
/// treats differently: one is rebuilt from here, one is not fixable from here
/// at all, and one is a check that could not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sighting {
    /// Nothing is being shown, and nothing should be.
    Down,
    /// On the screen, confirmed by reading the screen.
    OnScreen,
    /// Asked for, and not on the screen.
    Missing(Missing),
}

/// What the screen says, as it changes.
pub type Sightings = tokio::sync::watch::Receiver<Sighting>;

/// What the session wants the indicator to say.
#[derive(Debug)]
enum Command {
    Show(Watching),
    Hide,
    /// Work out what blanking would take, and say so.
    ///
    /// A question rather than an order, because the undo has to be registered
    /// with the keeper before anything goes dark — so the session asks, keeps
    /// the answer, and only then sends [`Command::Blank`].
    PlanBlanking(Sender<Result<esper_presence::Blanking, String>>),
    /// Blank the screens, applying a plan the session has registered.
    Blank(esper_presence::Blanking, Sender<Result<(), String>>),
    /// Put the screens back.
    Unblank(Sender<Result<(), String>>),
    Stop,
}

/// The handle the rest of the engine holds.
#[derive(Debug)]
pub struct Indicator {
    to_thread: Sender<Command>,
    /// What the screen says about the frame, kept current by the thread.
    sightings: Sightings,
    backend: &'static str,
    /// Whether the panic key is actually held by us.
    armed: bool,
    /// Why there is no panic key, when there is none. The backend's own words.
    why_no_panic_key: Option<&'static str>,
    /// Why the frame cannot be shown, when it cannot.
    why_cannot_show: Option<&'static str>,
    /// Whether an indicator can be shown at all.
    can_show: bool,
}

impl Indicator {
    /// Which backend is showing it.
    #[must_use]
    pub const fn backend(&self) -> &'static str {
        self.backend
    }

    /// Whether the panic key is working.
    #[must_use]
    pub const fn why_no_panic_key(&self) -> Option<&'static str> {
        self.why_no_panic_key
    }

    /// Why the frame cannot be shown, when it cannot.
    pub const fn why_cannot_show(&self) -> Option<&'static str> {
        self.why_cannot_show
    }

    pub const fn armed(&self) -> bool {
        self.armed
    }

    /// Whether an indicator can be shown.
    #[must_use]
    pub const fn can_show(&self) -> bool {
        self.can_show
    }

    /// What the screen says about the indicator, as it changes.
    ///
    /// **The half that makes the indicator a guarantee rather than an
    /// intention**: a session watches this and stops showing a picture when
    /// the screen stops showing the frame.
    #[must_use]
    pub fn sightings(&self) -> Sightings {
        self.sightings.clone()
    }

    /// Ask what blanking this machine's screens would take.
    ///
    /// The answer carries the undo, which a caller registers with a keeper
    /// **before** calling [`Self::blank`].
    ///
    /// # Errors
    ///
    /// If the indicator's thread is gone, or the display has no screen that
    /// could be turned off.
    pub fn blanking_plan(&self) -> Result<esper_presence::Blanking, String> {
        self.ask(Command::PlanBlanking)?
    }

    /// Blank the screens, with a plan somebody is holding the undo for.
    ///
    /// # Errors
    ///
    /// If the screens cannot be blanked, or if blanking them would stop the
    /// session having a picture — which the backend checks by reading its own
    /// screen back, and undoes before saying so.
    pub fn blank(&self, plan: &esper_presence::Blanking) -> Result<(), String> {
        self.ask(|reply| Command::Blank(plan.clone(), reply))?
    }

    /// Ask the thread something and wait for its answer.
    ///
    /// **Bounded by a patience**, because the alternative is a session that
    /// hangs for ever if the indicator's thread has died — and the thread
    /// holds an X connection, which is a thing that can block.
    fn ask<T>(&self, build: impl FnOnce(Sender<T>) -> Command) -> Result<T, String> {
        let (reply, answer) = std::sync::mpsc::channel();
        self.to_thread
            .send(build(reply))
            .map_err(|_| "the indicator's thread is gone".to_owned())?;
        answer
            .recv_timeout(ANSWER_PATIENCE)
            .map_err(|e| format!("the indicator's thread did not answer: {e}"))
    }

    /// Show that a session is happening.
    pub fn show(&self, watching: Watching) {
        // Best effort by design: the thread is gone only when the process is
        // going, and a session that cannot raise an indicator has already
        // been told so by `can_show`.
        let _ = self.to_thread.send(Command::Show(watching));
    }

    /// Take the indicator away.
    pub fn hide(&self) {
        let _ = self.to_thread.send(Command::Hide);
    }
}

impl Drop for Indicator {
    fn drop(&mut self) {
        let _ = self.to_thread.send(Command::Stop);
    }
}

/// Each press of the panic key, once.
///
/// **A queue, not a flag.** A flag was the first version and it was wrong: a
/// latched "it happened" makes every loop that waits on it return immediately
/// for ever, so the session ended, the accept loop saw the panic again, and
/// again — four and a half million times in three seconds, measured, filling
/// a log with 1.5 GB and burning a core.
///
/// A press is an *event*, acted on once by whoever is waiting. Only one loop
/// waits at a time — while a session is running the accept loop is inside it —
/// so one receiver is the whole mechanism.
pub type Presses = tokio::sync::mpsc::Receiver<HostEvent>;

/// Start the indicator and grab the panic key.
///
/// `Err` when this machine has no way to do either. **A caller must not treat
/// that as ordinary**: a host that cannot show it is being watched is failing
/// to provide something a person relies on, and it should say so loudly rather
/// than serve a session quietly.
pub fn start() -> Result<(Indicator, Presses), String> {
    #[cfg(target_os = "linux")]
    {
        // The same ladder shape as the input backends, and the same order:
        // X11 first because it works under XWayland, then wlroots' layer
        // shell for the sessions with no X server at all.
        //
        // Both reasons are kept when both fail. "No indicator" naming only
        // the X11 attempt sends somebody on a Wayland desktop looking for a
        // `DISPLAY` they are never going to find.
        let x11 = esper_presence_x11::X11Presence::open();
        match x11 {
            Ok(presence) => start_with(presence),
            Err(first) => match esper_presence_wlr::WlrPresence::open() {
                Ok(presence) => start_with(presence),
                Err(second) => Err(format!("{first}; and {second}")),
            },
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err("no session indicator is wired up for this platform".to_owned())
    }
}

/// Run the indicator thread for a backend that opened.
#[cfg(target_os = "linux")]
fn start_with<P: Presence + Send + 'static>(presence: P) -> Result<(Indicator, Presses), String> {
    {
        let armed = presence.panic_key_grabbed();
        let can_show = presence.can_show();
        let why_no_panic_key = presence.why_no_panic_key();
        let why_cannot_show = presence.why_cannot_show();
        let backend = esper_presence::Presence::backend(&presence);
        let (to_thread, commands) = std::sync::mpsc::channel();
        // Four deep: a person hitting the key repeatedly should not have
        // presses dropped, and nobody presses it more than a handful of times.
        let (panic_tx, panic_rx) = tokio::sync::mpsc::channel(4);
        let (seen_tx, sightings) = tokio::sync::watch::channel(Sighting::Down);
        std::thread::spawn(move || run(presence, commands, panic_tx, seen_tx));
        Ok((
            Indicator {
                to_thread,
                sightings,
                backend,
                armed,
                why_no_panic_key,
                why_cannot_show,
                can_show,
            },
            panic_rx,
        ))
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err("no presence backend is compiled in for this platform".to_owned())
    }
}

/// The thread: show what it is told, report the panic key, and keep asking the
/// screen whether the indicator is really on it.
fn run<P: Presence>(
    mut presence: P,
    commands: Receiver<Command>,
    presses: tokio::sync::mpsc::Sender<HostEvent>,
    seen: tokio::sync::watch::Sender<Sighting>,
) {
    // What the indicator was last asked to say, so the frame can be built
    // again after something destroys it — which needs the words, not just the
    // knowledge that something was showing.
    let mut asked: Option<Watching> = None;
    let mut checked_at = Instant::now();
    loop {
        // **Polled before the commands are read**, so a panic pressed at the
        // same moment the host is shutting down is still reported. Acting on
        // it costs nothing and reporting it is what closes the sessions; the
        // other order drops it on the floor.
        match presence.poll() {
            Ok(events) => {
                // **Which key, not just that one was pressed.** This
                // forwarded a bare `()` and filtered for `Panic`, which was
                // right while there was one key — and would have made the
                // pause key end the session.
                for event in events {
                    // `try_send`, and a full queue is dropped on purpose: four
                    // presses nobody has acted on yet means the fifth adds
                    // nothing, and blocking this thread would stop the
                    // indicator being kept on top.
                    let _ = presses.try_send(event);
                }
            }
            Err(e) => {
                tracing::error!("the session indicator's display went away: {e}");
                break;
            }
        }
        match commands.try_recv() {
            Ok(Command::Show(watching)) => {
                if let Err(e) = presence.show(&watching) {
                    tracing::error!("could not show the session indicator: {e}");
                }
                asked = Some(watching);
                // **Verified here, not on the next tick.** A session waits for
                // this answer before it sends a picture, so the wait is as long
                // as this check takes rather than as long as the cadence is.
                look(&mut presence, &seen, &asked);
                checked_at = Instant::now();
            }
            Ok(Command::Hide) => {
                if let Err(e) = presence.hide() {
                    tracing::error!("could not take the session indicator down: {e}");
                }
                asked = None;
                let _ = seen.send_if_modified(|s| replace(s, Sighting::Down));
            }
            Ok(Command::PlanBlanking(reply)) => {
                let _ = reply.send(presence.blanking_plan().map_err(|e| e.to_string()));
            }
            Ok(Command::Blank(plan, reply)) => {
                let _ = reply.send(presence.blank(&plan).map_err(|e| e.to_string()));
            }
            Ok(Command::Unblank(reply)) => {
                let _ = reply.send(presence.unblank().map_err(|e| e.to_string()));
            }
            Ok(Command::Stop) | Err(TryRecvError::Disconnected) => break,
            Err(TryRecvError::Empty) => {}
        }

        // **The watchdog.** Everything that hides the frame after it went up
        // happens without this process being told: a compositor restarting
        // destroys its windows, a program can be raised over it, and X11 has
        // no event for "your window is no longer the top thing at this pixel".
        // So the screen is asked, on a cadence, for as long as a session lasts.
        if asked.is_some() && checked_at.elapsed() >= VERIFY_EVERY {
            look(&mut presence, &seen, &asked);
            checked_at = Instant::now();
        }
        std::thread::sleep(TICK);
    }
    let _ = presence.hide();
    // **And the screens come back.** This is the thread's own last act, which
    // covers the ways a host ends that a session's own cleanup does not: the
    // keeper is the cover for the ways *this* does not run.
    let _ = presence.unblank();
}

/// Wait until the screen shows the indicator, or say why it does not.
///
/// **What "before a single frame goes out" costs.** A session asks for the
/// frame and then waits here, so the picture starts a round trip to the
/// display server later — milliseconds on the machines this runs on, and the
/// price of nobody being watched without being able to see it.
///
/// A patience rather than a single look, because `show` and the first
/// verification happen on another thread: an answer that has not arrived yet
/// is not an answer of "no".
///
/// # The patience is spent on a "no" as well
///
/// Not only on silence. A verdict of `Missing` is a reading, and a reading
/// taken while the display server is still catching up is not the answer: a
/// frame asked for, mapped and raised, and read back before it was painted
/// reads as *covered by the wallpaper*, which is what the screen honestly was
/// a few milliseconds earlier. Ending on the first no would make a session's
/// fate turn on how far the server had got.
///
/// On a fast, still `Xvfb` that window is narrow enough that a suite passes.
/// On an ordinary desktop it is wide enough to refuse sessions at random: the
/// same machine, the same command, twice in a row, said "6 of 12 places are
/// not its colour" and then "12 of 12" and then held a session fine.
///
/// So the deadline is the thing that decides. Every look after a "no" is
/// another chance for the screen to catch up, and what is reported when the
/// time runs out is the last reason given rather than the first — because the
/// last one is the state the screen settled in.
///
/// **This does not soften the guarantee.** Nothing is sent until the screen
/// has been read and has shown the indicator. What changes is only how long
/// the screen is given to show it.
pub async fn confirmed(indicator: &Indicator, patience: Duration) -> Result<usize, String> {
    let mut sightings = indicator.sightings();
    let deadline = tokio::time::Instant::now() + patience;
    let mut looks = 0usize;
    let mut last: Option<String> = None;
    loop {
        looks = looks.saturating_add(1);
        match &*sightings.borrow_and_update() {
            Sighting::OnScreen => return Ok(looks),
            Sighting::Missing(missing) => last = Some(missing.describe()),
            // Nothing has been reported yet: the thread has not got to it.
            Sighting::Down => {}
        }
        if tokio::time::timeout_at(deadline, sightings.changed())
            .await
            .is_err()
        {
            return Err(last.unwrap_or_else(|| {
                "the indicator's own thread did not answer in time".to_owned()
            }));
        }
    }
}

/// Ask the screen, put the frame back if that is what is wrong, and publish.
///
/// **One attempt at a rebuild, in here rather than in the caller.** A
/// compositor restarting is the ordinary reason the windows are gone, it is
/// recoverable, and it happens while somebody's screen is being watched — so
/// waiting for a session loop to notice and ask would be a gap of exactly the
/// kind this check exists to close.
fn look<P: Presence>(
    presence: &mut P,
    seen: &tokio::sync::watch::Sender<Sighting>,
    asked: &Option<Watching>,
) {
    let mut verdict = presence.verify();
    if let Ok(Verdict::Missing(missing)) = &verdict
        && missing.worth_rebuilding()
        && let Some(watching) = asked
    {
        tracing::warn!(
            "the session indicator is not on screen ({}), putting it back",
            missing.describe()
        );
        if presence.show(watching).is_ok() {
            verdict = presence.verify();
        }
    }
    let now = match verdict {
        Ok(Verdict::OnScreen { checked }) if checked > 0 => Sighting::OnScreen,
        Ok(Verdict::OnScreen { .. }) => Sighting::Missing(Missing::Unreadable(
            "the check looked at no part of the screen".to_owned(),
        )),
        Ok(Verdict::Missing(Missing::NothingAsked)) => Sighting::Down,
        Ok(Verdict::Missing(missing)) => Sighting::Missing(missing),
        // A backend that cannot answer is not a backend saying yes.
        Err(e) => Sighting::Missing(Missing::Unreadable(e.to_string())),
    };
    let _ = seen.send_if_modified(|s| replace(s, now));
}

/// Set a watched value, saying whether it changed.
///
/// `send_if_modified` wants that answer: sending on every check would wake
/// every session loop once a second to be told nothing had changed.
fn replace(slot: &mut Sighting, now: Sighting) -> bool {
    if *slot == now {
        return false;
    }
    *slot = now;
    true
}

/// A blanked screen, and the keeper holding the way back.
///
/// **Handed back rather than dropped.** Putting screens back can fail, and a
/// failure somebody has to be told about cannot be reported from a `Drop` — so
/// [`Self::give_back`] is the way out, and the keeper is what covers every
/// path that never reaches it.
pub struct Privacy {
    /// What went dark, for the line that says so.
    pub what: String,
    /// Held for as long as the screens are dark, and no longer.
    keeper: esper_keeper::Keeper,
    to_thread: Sender<Command>,
}

impl std::fmt::Debug for Privacy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Privacy").field("what", &self.what).finish()
    }
}

impl Privacy {
    /// Put the screens back, and let the keeper go.
    ///
    /// The order matters: the keeper is cancelled **after** the screens are
    /// back, so a host that dies in between is still covered. Cancelling first
    /// would open a gap of exactly the width of one restore.
    pub fn give_back(mut self) {
        let (reply, answer) = std::sync::mpsc::channel();
        let asked = self
            .to_thread
            .send(Command::Unblank(reply))
            .map_err(|_| "the indicator's thread is gone".to_owned())
            .and_then(|()| {
                answer
                    .recv_timeout(ANSWER_PATIENCE)
                    .map_err(|e| format!("the indicator's thread did not answer: {e}"))
            });
        match asked {
            Ok(Ok(())) => println!("      privacy    {} is back", self.what),
            Ok(Err(e)) | Err(e) => {
                // **Said loudly, and the keeper is left holding it.** Somebody
                // is sitting in front of a screen that is still dark, and the
                // one thing that must not happen next is this process
                // cancelling the only thing that would fix it.
                eprintln!("      · {} would not come back: {e}", self.what);
                eprintln!("        the keeper still has it; ending this host will restore it");
                return;
            }
        }
        self.keeper.cancel();
    }
}

/// Blank this machine's screens for the duration of a session.
///
/// **The order here is the safety story**, and it is why this is one function
/// rather than three calls at a call site: ask what blanking would take, start
/// something that will outlive this process, give it the undo, and only then
/// turn anything off. Any other order leaves a window where a crash leaves
/// somebody's monitor dark with nobody holding the way back.
///
/// # Errors
///
/// If the display cannot be blanked, if a keeper cannot be started, or if the
/// undo cannot be written down. **Every one of those refuses to blank
/// anything**, which is the point: the screens go dark only once somebody
/// other than this process can bring them back.
pub fn blank_for_the_session(indicator: &Indicator) -> Result<Privacy, String> {
    let plan = indicator.blanking_plan()?;
    let home = crate::paths::home()?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut keeper = esper_keeper::Keeper::start(&exe, &home).map_err(|e| e.to_string())?;
    for undo in &plan.undo {
        keeper.keep(undo.clone()).map_err(|e| e.to_string())?;
    }
    indicator.blank(&plan)?;
    Ok(Privacy {
        what: plan.what,
        keeper,
        to_thread: indicator.to_thread.clone(),
    })
}

/// This machine's own input, taken out of the picture for a session.
///
/// Held for as long as the session runs. Dropping it puts the devices back and
/// stops the keeper, in that order — the same shape as [`Privacy`], and for
/// the same reason: whatever ends the session should not have to remember.
/// The desktop background, plain for as long as this is held.
///
/// **The mildest of the three things a session changes and does not own**, and
/// held the same way for the same reason: a host that dies must not leave
/// somebody's wallpaper gone. Dropping it puts the background back and lets
/// the keeper go, in that order — see [`LocalInput`], which explains why the
/// order is that way round.
#[derive(Debug)]
pub struct PlainBackdrop {
    /// What a person would call what changed.
    pub what: String,
    /// How much of the picture actually moved, so a caller can say whether it
    /// was worth doing.
    pub moved: usize,
    /// Of how many places looked at.
    pub of: usize,
    plan: esper_presence::Backdrop,
    keeper: esper_keeper::Keeper,
}

impl Drop for PlainBackdrop {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        for undo in &self.plan.undo {
            if let Err(e) = esper_presence_x11::backdrop::put_back(undo) {
                // Said rather than swallowed: somebody is looking at a black
                // desktop, and the keeper still holds the record until it is
                // told otherwise.
                eprintln!("      · could not put the desktop background back: {e}");
            }
        }
        self.keeper.cancel();
    }
}

/// Make this machine's desktop background plain for the session.
///
/// **The order is the safety story**, and it is the same one the privacy
/// screen and the input lockout have: ask what the desktop published, start
/// something that will outlive this process, give it the undo, and only then
/// change anything.
///
/// # Errors
///
/// If the desktop published no background pixmap — nothing that can be put
/// back may be taken away — if a keeper cannot be started, or if the server
/// refuses. Every one of those changes nothing.
#[cfg(target_os = "linux")]
pub fn plain_backdrop() -> Result<PlainBackdrop, String> {
    let plan = esper_presence_x11::backdrop::plan()?;
    let home = crate::paths::home()?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut keeper = esper_keeper::Keeper::start(&exe, &home).map_err(|e| e.to_string())?;
    for undo in &plan.undo {
        keeper.keep(undo.clone()).map_err(|e| e.to_string())?;
    }
    let changed = esper_presence_x11::backdrop::plain(&plan)?;
    Ok(PlainBackdrop {
        what: plan.what.clone(),
        moved: changed.moved,
        of: changed.of,
        plan,
        keeper,
    })
}

/// No backend on this platform knows how.
#[cfg(not(target_os = "linux"))]
pub fn plain_backdrop() -> Result<PlainBackdrop, String> {
    Err("no backend on this platform can make this machine's desktop background plain".to_owned())
}

#[derive(Debug)]
pub struct LocalInput {
    /// What a person would call what stopped working.
    pub what: String,
    plan: esper_input::Floating,
    keeper: esper_keeper::Keeper,
}

impl Drop for LocalInput {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        for trouble in esper_input_x11::floating::reattach(&self.plan) {
            // Said rather than swallowed: somebody is sitting at a machine
            // whose keyboard has not come back, and the keeper is the other
            // half of this — it still holds the record until it is told
            // otherwise.
            eprintln!("      · could not give the input back: {trouble}");
        }
        // Cancelled last: the keeper's records are the way back, and letting
        // it go before the devices are attached would leave a window with
        // nobody holding them.
        self.keeper.cancel();
    }
}

/// Take this machine's own keyboard and pointer out of the picture.
///
/// **The order here is the safety story**, and it is the same one the privacy
/// screen has: ask what detaching would take, start something that will
/// outlive this process, give it the undo, and only then detach anything. Any
/// other order leaves a window where a crash leaves somebody with no keyboard
/// and nobody holding the way back — which is worse than a dark monitor,
/// because they cannot even type blind.
///
/// # Errors
///
/// If the display cannot be asked, if a keeper cannot be started, or if the
/// undo cannot be written down. **Every one of those detaches nothing**, which
/// is the point.
#[cfg(target_os = "linux")]
pub fn lock_local_input() -> Result<LocalInput, String> {
    let plan = esper_input_x11::floating::plan()?;
    let home = crate::paths::home()?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut keeper = esper_keeper::Keeper::start(&exe, &home).map_err(|e| e.to_string())?;
    for undo in &plan.undo {
        keeper.keep(undo.clone()).map_err(|e| e.to_string())?;
    }
    esper_input_x11::floating::detach(&plan)?;
    Ok(LocalInput {
        what: plan.what.clone(),
        plan,
        keeper,
    })
}

/// No backend on this platform knows how.
#[cfg(not(target_os = "linux"))]
pub fn lock_local_input() -> Result<LocalInput, String> {
    Err("no backend on this platform can take this machine's input out of the picture".to_owned())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use esper_presence::{NullPresence, Shown};
    use std::sync::mpsc::channel;

    fn watching(controlling: bool) -> Watching {
        Watching {
            peer: "somebody".to_owned(),
            controlling,
            recording: false,
            paused: false,
        }
    }

    /// **The two hotkeys are told apart, and neither becomes the other.**
    ///
    /// The channel carries which key it was, rather than a bare `()` the
    /// thread reads as `Panic`. With one key that distinction costs nothing;
    /// with two it is the difference between pausing and ending every session,
    /// which is the worst possible way for a key called "pause" to be wrong.
    #[test]
    fn a_pause_press_is_not_a_panic_press() {
        let mut presence = NullPresence::new();
        presence.pause_key();
        presence.panic_key();
        presence.pause_key();
        let (to_thread, commands) = channel();
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        // Stopped after one turn, the way the tests above do it: the presses
        // are already queued, so there is nothing to wait for.
        to_thread.send(Command::Stop).expect("send");
        run(
            presence,
            commands,
            tx,
            tokio::sync::watch::channel(Sighting::Down).0,
        );
        let mut seen = Vec::new();
        while let Ok(event) = rx.try_recv() {
            seen.push(event);
        }
        assert_eq!(
            seen,
            vec![HostEvent::Pause, HostEvent::Panic, HostEvent::Pause],
            "the presses arrived as something other than what was pressed"
        );
    }

    /// The thread does what it is told and reports what happened, checked with
    /// no display in the way.
    #[test]
    fn the_thread_shows_hides_and_reports_a_panic() {
        let mut presence = NullPresence::new();
        presence.panic_key();
        let (to_thread, commands) = channel();
        let (panic_tx, mut panic_rx) = tokio::sync::mpsc::channel(4);

        to_thread.send(Command::Show(watching(true))).expect("send");
        to_thread.send(Command::Hide).expect("send");
        to_thread.send(Command::Stop).expect("send");

        // Run it to completion on this thread: the commands are already
        // queued, so there is nothing to wait for.
        run(
            presence,
            commands,
            panic_tx,
            tokio::sync::watch::channel(Sighting::Down).0,
        );

        assert_eq!(
            panic_rx.try_recv(),
            Ok(HostEvent::Panic),
            "the press must have been reported"
        );
    }

    /// **Showing is not the same as being on the screen**, and the thread
    /// reports the difference.
    ///
    /// The whole point of the verifying half: `show` returning `Ok` was the
    /// only thing the engine ever knew, and a frame that never reached the
    /// screen looked exactly like one that did.
    #[test]
    fn the_thread_says_whether_the_screen_shows_the_frame() {
        let presence = NullPresence::new();
        let (to_thread, commands) = channel();
        let (panic_tx, _panic_rx) = tokio::sync::mpsc::channel(4);
        let (seen_tx, seen) = tokio::sync::watch::channel(Sighting::Down);

        to_thread.send(Command::Show(watching(true))).expect("send");
        to_thread.send(Command::Stop).expect("send");
        run(presence, commands, panic_tx, seen_tx);

        assert_eq!(
            *seen.borrow(),
            Sighting::OnScreen,
            "the screen was asked and said the frame is there"
        );
    }

    /// **Something drawn over the frame is reported, not smoothed over.**
    ///
    /// A cover cannot be rebuilt away, so the verdict has to survive the
    /// thread's own recovery attempt: a session decides what to do about it,
    /// and it can only do that if it is told.
    #[test]
    fn a_covered_frame_is_reported_as_missing() {
        let mut presence = NullPresence::new();
        presence.cover();
        let (to_thread, commands) = channel();
        let (panic_tx, _panic_rx) = tokio::sync::mpsc::channel(4);
        let (seen_tx, seen) = tokio::sync::watch::channel(Sighting::Down);

        to_thread
            .send(Command::Show(watching(false)))
            .expect("send");
        to_thread.send(Command::Stop).expect("send");
        run(presence, commands, panic_tx, seen_tx);

        match &*seen.borrow() {
            Sighting::Missing(Missing::Covered { .. }) => {}
            other => panic!("a covered frame was reported as {other:?}"),
        }
    }

    /// **Windows a compositor destroyed are put back, and the rebuild is what
    /// this asserts.**
    ///
    /// Aimed at `look` rather than at the thread, and the order matters:
    /// asking for the frame is itself the rebuild, so a test that makes the
    /// frame missing *before* asking never runs the recovery path at all and
    /// passes with that path deleted.
    #[test]
    fn windows_that_are_gone_are_rebuilt_and_then_confirmed() {
        let mut presence = NullPresence::new();
        let asked = watching(false);
        presence.show(&asked).expect("shown");
        // Now the screen loses them, the way a compositor restarting does:
        // after the frame was asked for, with nothing telling this process.
        presence.destroy();
        let (seen_tx, seen) = tokio::sync::watch::channel(Sighting::Down);

        look(&mut presence, &seen_tx, &Some(asked));

        assert_eq!(
            *seen.borrow(),
            Sighting::OnScreen,
            "the frame should have been rebuilt and then confirmed"
        );
        assert_eq!(
            presence.asked.len(),
            2,
            "and the rebuild is the second time it was asked for: {:?}",
            presence.asked
        );
    }

    /// **A frame with something over it is not rebuilt away.**
    ///
    /// Rebuilding would not fix it on a real display — the covering window is
    /// still there and still on top — so a backend that reported success
    /// after trying would be reporting the attempt rather than the screen.
    #[test]
    fn a_cover_is_not_something_a_rebuild_pretends_to_fix() {
        let mut presence = NullPresence::new();
        let asked = watching(true);
        presence.show(&asked).expect("shown");
        presence.cover();
        let (seen_tx, seen) = tokio::sync::watch::channel(Sighting::Down);

        look(&mut presence, &seen_tx, &Some(asked));

        match &*seen.borrow() {
            Sighting::Missing(Missing::Covered { .. }) => {}
            other => panic!("a covered frame was smoothed over as {other:?}"),
        }
        assert_eq!(
            presence.asked.len(),
            1,
            "and nothing tried to rebuild it: {:?}",
            presence.asked
        );
    }

    /// Hiding says the indicator is down, which is not a fault.
    ///
    /// A session that treated "down" as "missing" would refuse the next
    /// session on a host that had simply finished the last one.
    #[test]
    fn hiding_reports_down_rather_than_a_fault() {
        let presence = NullPresence::new();
        let (to_thread, commands) = channel();
        let (panic_tx, _panic_rx) = tokio::sync::mpsc::channel(4);
        let (seen_tx, seen) = tokio::sync::watch::channel(Sighting::Down);

        to_thread.send(Command::Show(watching(true))).expect("send");
        to_thread.send(Command::Hide).expect("send");
        to_thread.send(Command::Stop).expect("send");
        run(presence, commands, panic_tx, seen_tx);

        assert_eq!(*seen.borrow(), Sighting::Down);
    }

    /// **A session waits for the answer rather than assuming one.**
    ///
    /// `confirmed` is what stands between a controller and the first picture,
    /// so a no has to come back with a *reason* — a session refused with no
    /// explanation is a support call.
    #[tokio::test]
    async fn a_session_waits_for_the_screens_answer() {
        let (to_thread, commands) = channel();
        let (panic_tx, panic_rx) = tokio::sync::mpsc::channel(4);
        let (seen_tx, sightings) = tokio::sync::watch::channel(Sighting::Down);
        let indicator = Indicator {
            to_thread,
            sightings,
            backend: "null",
            armed: true,
            why_no_panic_key: None,
            why_cannot_show: None,
            can_show: true,
        };
        let mut presence = NullPresence::new();
        presence.cover();
        let handle = std::thread::spawn(move || run(presence, commands, panic_tx, seen_tx));

        indicator.show(watching(true));
        let answer = confirmed(&indicator, Duration::from_secs(2)).await;
        let why = answer.expect_err("a covered frame is not a confirmation");
        assert!(
            why.contains("drawn over"),
            "the refusal has to say what is wrong: {why}"
        );

        drop(indicator);
        handle.join().expect("the thread ends when the handle goes");
        drop(panic_rx);
    }

    /// **A screen that is a moment behind is not a screen saying no.**
    ///
    /// The frame is asked for, mapped and raised, and then read back. On a
    /// desktop those steps take long enough that the first read can land
    /// before the server has painted it — and what comes back then is the
    /// wallpaper, reported honestly as "something is drawn over the
    /// indicator".
    ///
    /// Ending on that first no, whatever time is left, is what makes the same
    /// machine running the same command refuse a session, then refuse it
    /// differently, then hold one fine. The patience is spent on a no as well
    /// as on silence, and this is the case that says so.
    #[tokio::test]
    async fn a_screen_that_paints_late_is_still_a_screen_that_shows_it() {
        let (to_thread, _commands) = channel();
        let (seen_tx, sightings) = tokio::sync::watch::channel(Sighting::Down);
        let indicator = Indicator {
            to_thread,
            sightings,
            backend: "null",
            armed: true,
            why_no_panic_key: None,
            why_cannot_show: None,
            can_show: true,
        };

        // What a not-yet-painted frame reads as: the screen, honestly
        // reported, a moment before it shows the thing that was asked for.
        seen_tx
            .send(Sighting::Missing(esper_presence::Missing::Covered {
                wrong: 12,
                of: 12,
                saw: [0x13, 0x25, 0x33],
                at: (2, 300),
            }))
            .expect("a first verdict");

        let later = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            seen_tx.send(Sighting::OnScreen).expect("painted");
            // Held open: dropping the sender ends the watch.
            tokio::time::sleep(Duration::from_secs(2)).await;
            drop(seen_tx);
        });

        let looks = confirmed(&indicator, Duration::from_secs(2))
            .await
            .expect("a frame that arrived late is a frame on the screen");
        assert!(looks >= 2, "it should have looked more than once: {looks}");
        later.abort();
    }

    /// And when the screen never shows it, the reason is the *last* one.
    ///
    /// The state a screen settles in is what somebody has to act on. Reporting
    /// the first reading would name whatever the screen happened to be in the
    /// middle of.
    #[tokio::test]
    async fn a_screen_that_never_shows_it_is_refused_with_its_final_reason() {
        let (to_thread, _commands) = channel();
        let (seen_tx, sightings) = tokio::sync::watch::channel(Sighting::Down);
        let indicator = Indicator {
            to_thread,
            sightings,
            backend: "null",
            armed: true,
            why_no_panic_key: None,
            why_cannot_show: None,
            can_show: true,
        };

        seen_tx
            .send(Sighting::Missing(esper_presence::Missing::Covered {
                wrong: 1,
                of: 12,
                saw: [0x13, 0x25, 0x33],
                at: (2, 300),
            }))
            .expect("a first verdict");
        let later = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            seen_tx
                .send(Sighting::Missing(esper_presence::Missing::Unmapped))
                .expect("a second verdict");
            tokio::time::sleep(Duration::from_secs(2)).await;
            drop(seen_tx);
        });

        let why = confirmed(&indicator, Duration::from_millis(600))
            .await
            .expect_err("a frame that never appeared is not a confirmation");
        assert!(
            why.contains("not on screen"),
            "the refusal should carry the state the screen settled in, and says: {why}"
        );
        later.abort();
    }

    /// **One press is one event.** The first version latched a flag, so every
    /// loop waiting on it returned for ever: four and a half million panics in
    /// three seconds, a 1.5 GB log and a busy core. A queue is delivered once.
    #[test]
    fn a_press_is_delivered_once_and_not_for_ever() {
        let mut presence = NullPresence::new();
        presence.panic_key();
        let (to_thread, commands) = channel();
        let (panic_tx, mut panic_rx) = tokio::sync::mpsc::channel(4);
        to_thread.send(Command::Stop).expect("send");
        run(
            presence,
            commands,
            panic_tx,
            tokio::sync::watch::channel(Sighting::Down).0,
        );

        assert_eq!(
            panic_rx.try_recv(),
            Ok(HostEvent::Panic),
            "one press arrives"
        );
        assert!(
            panic_rx.try_recv().is_err(),
            "and there is not a second one waiting"
        );
    }

    /// Two presses are two events, so a person who hits it again during the
    /// next session is heard again.
    #[test]
    fn two_presses_are_two_events() {
        let mut presence = NullPresence::new();
        presence.panic_key();
        presence.panic_key();
        let (to_thread, commands) = channel();
        let (panic_tx, mut panic_rx) = tokio::sync::mpsc::channel(4);
        to_thread.send(Command::Stop).expect("send");
        run(
            presence,
            commands,
            panic_tx,
            tokio::sync::watch::channel(Sighting::Down).0,
        );
        assert_eq!(panic_rx.try_recv(), Ok(HostEvent::Panic));
        assert_eq!(panic_rx.try_recv(), Ok(HostEvent::Panic));
        assert!(panic_rx.try_recv().is_err());
    }

    /// **A panic is reported even if nothing was ever shown.** The key works
    /// from the moment the host is running, including while a session is only
    /// being offered.
    #[test]
    fn a_panic_with_no_session_is_still_reported() {
        let mut presence = NullPresence::new();
        presence.panic_key();
        let (to_thread, commands) = channel();
        let (panic_tx, mut panic_rx) = tokio::sync::mpsc::channel(4);
        to_thread.send(Command::Stop).expect("send");
        run(
            presence,
            commands,
            panic_tx,
            tokio::sync::watch::channel(Sighting::Down).0,
        );
        assert_eq!(panic_rx.try_recv(), Ok(HostEvent::Panic));
    }

    /// Dropping the handle stops the thread, so a host that finishes does not
    /// leave a frame on somebody's screen.
    #[test]
    fn the_indicator_comes_down_when_the_thread_stops() {
        let presence = NullPresence::new();
        let (to_thread, commands) = channel();
        let (panic_tx, _panic_rx) = tokio::sync::mpsc::channel(4);
        to_thread
            .send(Command::Show(watching(false)))
            .expect("send");
        drop(to_thread);
        // A disconnected channel ends the thread, which hides on the way out.
        run(
            presence,
            commands,
            panic_tx,
            tokio::sync::watch::channel(Sighting::Down).0,
        );
        // The recording presence is consumed by `run`, so what is checked here
        // is that it terminated at all rather than looping for ever — a
        // disconnected command channel is how the process going away looks
        // from in here.
    }

    /// What the recording presence saw, when it is kept rather than moved into
    /// the thread.
    #[test]
    fn showing_twice_with_different_grants_says_both_things() {
        let mut presence = NullPresence::new();
        Presence::show(&mut presence, &watching(false)).expect("shown");
        Presence::show(&mut presence, &watching(true)).expect("shown");
        Presence::hide(&mut presence).expect("hidden");
        assert_eq!(
            presence.asked,
            vec![
                Shown::Up(watching(false)),
                Shown::Up(watching(true)),
                Shown::Down
            ]
        );
        assert!(!presence.is_showing());
    }
}
