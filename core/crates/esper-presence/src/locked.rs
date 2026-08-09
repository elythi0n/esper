//! Whether the screen in front of this machine is locked.
//!
//! # Why anybody asks
//!
//! `unattended` means a device may walk in without anybody agreeing. That is
//! the right answer for a machine in a rack and a large claim for a machine on
//! a desk, and the operator who wants the second wants a bound on it: **let a
//! device in while I am here, and not while the screen is locked.** A locked
//! screen is the closest thing a machine has to "nobody is at this desk".
//!
//! # Why it takes three probes
//!
//! There is no single answer on Linux. Which one is available depends on the
//! session manager and on which screen locker somebody chose:
//!
//! 1. **`logind` knows**, when it is the session manager. `LockedHint` is set
//!    by `loginctl lock-session` and by GNOME and KDE when they lock, and it
//!    is the authoritative answer where it exists.
//! 2. **The screensaver extension knows**, for a locker that uses it —
//!    `xscreensaver` and `xdg-screensaver` do, and report themselves as an
//!    *external* saver.
//! 3. **A grab is evidence**, for a locker that does neither. `i3lock` and
//!    `swaylock` map a window and take the keyboard; nothing else on a desktop
//!    holds the keyboard for minutes at a time.
//!
//! Each says which one answered, because "the screen is locked" and "logind
//! says the screen is locked" are different amounts of evidence and an
//! operator reading a refusal deserves the second.
//!
//! # And why "cannot tell" is a third answer
//!
//! A machine with no `logind`, no screensaver extension and a locker that
//! grabs nothing cannot be asked. Reporting that as *unlocked* would turn a
//! policy somebody switched on into a policy that does nothing — the failure
//! mode where they find out at three in the morning. So it is its own answer,
//! and the caller decides. The host's policy treats it as locked *and says so
//! when it starts*, which fails closed and fails visibly — the two halves that
//! make failing closed acceptable.

use std::process::Command;

/// What could be established about the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locked {
    /// It is locked, and this is what said so.
    Yes(&'static str),
    /// It is not locked, and this is what said so.
    No(&'static str),
    /// Nothing here could tell.
    CannotTell,
}

impl Locked {
    /// What to print about this, for a person.
    #[must_use]
    pub fn describe(self) -> String {
        match self {
            Self::Yes(how) => format!("locked, according to {how}"),
            Self::No(how) => format!("not locked, according to {how}"),
            Self::CannotTell => "cannot be established on this machine".to_owned(),
        }
    }
}

/// The variable that names a file standing in for the screen's lock state.
///
/// # Why an override exists at all
///
/// A screen locker cannot be started from a shell, and the probes that can
/// answer while a session runs need either a session manager or a locker that
/// registers with one. So the half of this policy that matters most — a live
/// session ending because somebody locked up and walked away — has no way to
/// be exercised by a test that drives the product from outside. The
/// alternative to an override is that half never being tested, which for a
/// security control is the worse of the two.
///
/// # Why it names a file rather than holding a value
///
/// A value in the environment is fixed for the life of a process, and the
/// thing worth testing is a *transition*: a session that was allowed, and then
/// a screen that locked under it. A file can appear while the host is running.
///
/// # Why it is safe to ship
///
/// It can only be set by whoever starts the host — somebody who can already
/// edit the settings file, grant themselves every permission and read the
/// trail. It buys an attacker nothing. And every answer it produces says that
/// it produced it, so a machine running with it set can never look like a
/// machine reporting its own screen.
pub const OVERRIDE: &str = "ESPER_SCREEN_LOCKED";

/// What the override says, or `None` when it is not set.
#[must_use]
pub fn overridden() -> Option<Locked> {
    let named = std::env::var(OVERRIDE).ok()?;
    Some(by_the_file(std::path::Path::new(&named)))
}

/// Locked exactly while that file exists.
#[must_use]
pub fn by_the_file(path: &std::path::Path) -> Locked {
    if path.exists() {
        Locked::Yes(OVERRIDDEN)
    } else {
        Locked::No(OVERRIDDEN)
    }
}

/// What the override is called in a message.
///
/// **Said out loud wherever it decides anything**, so a machine running with
/// it set can never look like a machine reporting its own screen.
pub const OVERRIDDEN: &str = "the file ESPER_SCREEN_LOCKED names";

/// Ask `logind` whether the login session is locked.
///
/// **Shelled out rather than spoken over D-Bus**, for the same reason the
/// lockers are: this is the only question asked of the session manager in the
/// whole product, and a D-Bus client is a dependency, a connection to keep and
/// a second way for a headless machine to fail to start. The command is the
/// documented interface and costs a few milliseconds once per connection.
///
/// `ours` is the display the caller is actually serving — see [`ask_logind`]
/// for why that has to be compared rather than assumed.
#[must_use]
pub fn by_logind(ours: Option<&str>) -> Locked {
    ask_logind(
        |args| {
            Command::new("loginctl")
                .args(args)
                .output()
                .ok()
                .filter(|out| out.status.success())
                .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        },
        ours,
    )
}

/// The same, with the command's output supplied, so it can be tested.
///
/// # Why it asks which display `logind` means
///
/// `LockedHint` is about the *login session*. A host serving a second X
/// server, another seat or a scratch display is not serving that session, and
/// believing the answer there would report the lock state of a screen this
/// host is not showing — the same mistake [`crate::lock::Locks`] exists to
/// name in the other direction, where a host on a scratch display locked the
/// developer's desktop.
///
/// So both properties are asked for at once, and the answer counts only when
/// `logind`'s display is the one the caller is on. A session with no display
/// at all — a text console login — matches nothing, which is right: whether
/// somebody locked a `tty` says nothing about an X screen.
///
/// `ours` of `None` means the caller has no display to compare, which is a
/// platform where this is the only probe there is.
pub fn ask_logind<F>(run: F, ours: Option<&str>) -> Locked
where
    F: Fn(&[&str]) -> Option<String>,
{
    let Some(said) = run(&[
        "show-session",
        "self",
        "--property=LockedHint",
        "--property=Display",
        "--value",
    ]) else {
        return Locked::CannotTell;
    };
    let mut lines = said.lines().map(str::trim);
    let hint = lines.next().unwrap_or_default();
    let theirs = lines.next().unwrap_or_default();

    if let Some(ours) = ours
        && theirs != ours
    {
        return Locked::CannotTell;
    }
    match hint {
        "yes" => Locked::Yes(LOGIND),
        "no" => Locked::No(LOGIND),
        // An empty answer is a session with no such property, which is a
        // machine `logind` is not managing a seat for. Not an answer.
        _ => Locked::CannotTell,
    }
}

/// Where each probe's answer came from, for a message about it.
///
/// **Named as sources rather than as findings**, so the same word works in
/// both directions: "locked, according to the session manager" and "not
/// locked, according to the session manager" both read as English, where a
/// name like "a locker holding the keyboard" only reads one way and made the
/// host's own startup line say the opposite of what it meant.
pub const LOGIND: &str = "the session manager";

/// The screensaver probe's name.
pub const SCREENSAVER: &str = "the screensaver extension";

/// The grab probe's name.
pub const A_GRAB: &str = "this display's keyboard and windows";

/// The first probe that can answer, in order of how much it knows.
///
/// `probes` is the list to try; each returns [`Locked::CannotTell`] when it
/// cannot answer. Taking the list as an argument is what lets a test drive the
/// order rather than the platform.
pub fn first_answer(probes: &[fn() -> Locked]) -> Locked {
    for probe in probes {
        match probe() {
            Locked::CannotTell => {}
            answered => return answered,
        }
    }
    Locked::CannotTell
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `loginctl` prints one value per property, in the order asked.
    fn answering(hint: &str, display: &str) -> String {
        format!("{hint}\n{display}\n")
    }

    #[test]
    fn logind_saying_yes_is_locked_and_says_who_said_so() {
        assert_eq!(
            ask_logind(|_| Some(answering("yes", ":0")), Some(":0")),
            Locked::Yes(LOGIND)
        );
        assert_eq!(
            ask_logind(|_| Some(answering("no", ":0")), Some(":0")),
            Locked::No(LOGIND)
        );
    }

    /// A machine `logind` is not managing has no answer, which is not "no".
    ///
    /// The distinction this whole module exists for: an unanswerable question
    /// reported as "not locked" is a policy that quietly does nothing.
    #[test]
    fn a_machine_with_no_seat_cannot_tell() {
        assert_eq!(ask_logind(|_| None, Some(":0")), Locked::CannotTell);
        assert_eq!(
            ask_logind(|_| Some(String::new()), None),
            Locked::CannotTell
        );
        assert_eq!(
            ask_logind(|_| Some(answering("whatever this is", ":0")), Some(":0")),
            Locked::CannotTell
        );
    }

    /// **`logind`'s answer is about the login session's screen, not ours.**
    ///
    /// A host serving a second X server or a scratch display is not serving
    /// that session, so believing the answer there reports the lock state of a
    /// screen this host is not showing — and on a developer's machine running
    /// the suite, that screen is the developer's.
    #[test]
    fn logind_is_not_believed_about_a_display_it_does_not_mean() {
        assert_eq!(
            ask_logind(|_| Some(answering("yes", ":0")), Some(":90")),
            Locked::CannotTell,
            "a locked login session was read as the lock state of another display"
        );
        assert_eq!(
            ask_logind(|_| Some(answering("no", ":0")), Some(":90")),
            Locked::CannotTell
        );
        // A session with no display at all — a text console login — matches
        // nothing: whether somebody locked a tty says nothing about a screen.
        assert_eq!(
            ask_logind(|_| Some(answering("yes", "")), Some(":0")),
            Locked::CannotTell
        );
    }

    /// It asks about *this* session rather than about whatever is current, and
    /// asks which display it means in the same breath.
    ///
    /// `loginctl show-session` with no argument is an error, and
    /// `show-session ""` answers about the caller's session on some versions
    /// and refuses on others. `self` is the documented way to mean this one.
    #[test]
    fn it_asks_logind_about_this_session_and_which_screen() {
        let seen = std::cell::RefCell::new(Vec::new());
        let _ = ask_logind(
            |args| {
                seen.borrow_mut()
                    .extend(args.iter().map(|a| (*a).to_owned()));
                None
            },
            None,
        );
        let asked = seen.borrow().join(" ");
        assert!(asked.contains("self"), "{asked}");
        assert!(asked.contains("LockedHint"), "{asked}");
        assert!(asked.contains("Display"), "{asked}");
        // The order matters: the values come back in the order asked for, so
        // reading them the other way round would swap the answers.
        assert!(
            asked.find("LockedHint") < asked.find("Display"),
            "the properties are read in the order they are asked for: {asked}"
        );
    }

    #[test]
    fn the_first_probe_that_can_answer_is_the_one_used() {
        fn cannot() -> Locked {
            Locked::CannotTell
        }
        fn locked() -> Locked {
            Locked::Yes("the second")
        }
        fn unlocked() -> Locked {
            Locked::No("the third")
        }
        assert_eq!(
            first_answer(&[cannot, locked, unlocked]),
            Locked::Yes("the second")
        );
        assert_eq!(
            first_answer(&[cannot, unlocked, locked]),
            Locked::No("the third")
        );
        assert_eq!(first_answer(&[cannot, cannot]), Locked::CannotTell);
        assert_eq!(first_answer(&[]), Locked::CannotTell);
    }

    /// **The override tracks the file, both ways.**
    ///
    /// A transition is the whole reason it names a file rather than holding a
    /// value: a session that was allowed, and then a screen that locked under
    /// it. An override that only ever said "locked" would test half of that.
    #[test]
    fn the_override_follows_the_file_appearing_and_going() {
        let dir =
            std::env::temp_dir().join(format!("esper-locked-{}-{}", std::process::id(), line!()));
        std::fs::create_dir_all(&dir).expect("a directory");
        let named = dir.join("locked");

        assert_eq!(by_the_file(&named), Locked::No(OVERRIDDEN));
        std::fs::write(&named, b"").expect("a file");
        assert_eq!(by_the_file(&named), Locked::Yes(OVERRIDDEN));
        std::fs::remove_file(&named).expect("remove it");
        assert_eq!(by_the_file(&named), Locked::No(OVERRIDDEN));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// And whatever it decides says that it was the thing deciding.
    ///
    /// A machine running with the override set must never look like a machine
    /// reporting its own screen — the host prints where its answer came from,
    /// so this is the word that makes that visible.
    #[test]
    fn an_overridden_answer_names_the_override() {
        assert!(
            Locked::Yes(OVERRIDDEN)
                .describe()
                .contains("ESPER_SCREEN_LOCKED")
        );
        assert!(OVERRIDDEN.contains(OVERRIDE));
    }

    #[test]
    fn every_answer_says_something_a_person_can_read() {
        assert!(Locked::Yes(LOGIND).describe().contains("locked"));
        assert!(Locked::No(LOGIND).describe().contains("not locked"));
        assert!(Locked::CannotTell.describe().contains("cannot"));
    }
}
