//! Logging somebody out, and restarting a machine.
//!
//! # Why this shells out, like locking does
//!
//! Neither is something an application can do to itself. Ending a login
//! session belongs to the session manager, and restarting a machine belongs to
//! the init system — asking them is the only mechanism there is, and every
//! remote desktop does the same thing because there is nothing else to do.
//!
//! So: try the ones that exist, in order, and **say which one ran**. A reboot
//! that silently failed would leave somebody waiting for a machine to come
//! back that never went away.
//!
//! # Why these are two tables and not one
//!
//! They are different amounts of damage and they belong to different
//! permissions. Logging somebody out closes their applications; restarting
//! takes the machine away for a minute and takes every other session on it
//! with it. A device may be given one without the other, and the tables are
//! separate so a caller cannot ask for "power stuff" and get whichever was
//! first in a shared list.
//!
//! # What is *not* here
//!
//! Shutting down. A machine that is off cannot be woken by any of this
//! product's tiers except wake-on-LAN, and a remote helper who turns off a
//! machine they cannot turn back on has made things worse. It can be added the
//! day somebody wants it, with the same shape and its own permission.

use std::process::Command;

/// What a request is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// End the login session, closing whatever is running in it.
    Logoff,
    /// Restart the machine.
    Reboot,
}

impl Action {
    /// The word for it, for a log line.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Logoff => "log off",
            Self::Reboot => "restart",
        }
    }

    /// The programs that do it, in the order they are tried.
    #[must_use]
    pub const fn programs(self) -> &'static [(&'static str, &'static [&'static str])] {
        match self {
            // `loginctl` first because it asks the session manager rather than
            // a particular desktop, so it works whatever somebody is running.
            // `self` rather than a session id: the id is the one this process
            // belongs to, and naming any other would be ending somebody
            // else's session.
            Self::Logoff => &[
                ("loginctl", &["terminate-session", "self"]),
                ("gnome-session-quit", &["--logout", "--no-prompt"]),
                ("qdbus", &["org.kde.Shutdown", "/Shutdown", "logout"]),
            ],
            // **`systemctl reboot` and not `reboot`.** The bare command is a
            // symlink to `systemctl` on a systemd machine and a direct kernel
            // call on some others, and the direct one does not unmount
            // anything. Asking the init system is asking for a *clean*
            // restart, which is the only kind worth offering somebody
            // remotely.
            Self::Reboot => &[("systemctl", &["reboot"]), ("shutdown", &["-r", "now"])],
        }
    }
}

/// The first program on `path` that can carry `action` out.
///
/// Split out and taking the path as an argument so it can be tested: the
/// interesting cases are "several are present, the first wins" and "none is",
/// and neither should need a particular machine — nor a machine willing to be
/// restarted.
#[must_use]
pub fn program_on(path: &str, action: Action) -> Option<(&'static str, &'static [&'static str])> {
    action
        .programs()
        .iter()
        .find(|(program, _)| crate::lock::find_on(path, program).is_some())
        .copied()
}

/// Carry out an action, and say which program did it.
///
/// # Errors
///
/// If nothing that can do it is installed, or it will not start, or it fell
/// over at once.
pub fn ask_for(action: Action) -> Result<&'static str, String> {
    let path = std::env::var("PATH").unwrap_or_default();
    let Some((program, args)) = program_on(&path, action) else {
        return Err(format!(
            "nothing here can {}: tried {}",
            action.name(),
            action
                .programs()
                .iter()
                .map(|(p, _)| *p)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    };
    let child = Command::new(program)
        .args(args)
        .spawn()
        .map_err(|e| format!("{program}: {e}"))?;
    // **The same "started is not done" rule the locker has.** A program that
    // could not reach the session manager exits at once with a status, and
    // reporting a restart that never happened would leave somebody waiting
    // for a machine to come back that never went away.
    crate::lock::believe(child, program)
        .map_err(|e| format!("{program} could not {}: {e}", action.name()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of programs that exist and do nothing.
    fn fakes(names: &[&str]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "esper-power-{}-{}",
            std::process::id(),
            names.join("-")
        ));
        std::fs::create_dir_all(&dir).expect("a directory");
        for name in names {
            std::fs::write(dir.join(name), b"").expect("a file");
        }
        dir
    }

    /// **The order is the point.** `loginctl` asks the session manager and the
    /// others ask one particular desktop, so a machine with both should use
    /// the one that works whatever somebody is running.
    #[test]
    fn the_first_program_in_the_list_wins() {
        let dir = fakes(&["gnome-session-quit", "loginctl"]);
        let path = dir.display().to_string();
        assert_eq!(
            program_on(&path, Action::Logoff).map(|(p, _)| p),
            Some("loginctl")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// And a machine with none of them has no answer, rather than a guess.
    #[test]
    fn a_machine_with_nothing_installed_has_no_program() {
        let dir = fakes(&[]);
        let path = dir.display().to_string();
        assert_eq!(program_on(&path, Action::Logoff), None);
        assert_eq!(program_on(&path, Action::Reboot), None);
        assert_eq!(program_on("", Action::Reboot), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **The two actions are separate lists.** A caller asking to log
    /// somebody out must not get a program that restarts the machine because
    /// it happened to be first in a shared table.
    #[test]
    fn logging_off_and_restarting_do_not_share_a_program() {
        let logoff: Vec<&str> = Action::Logoff.programs().iter().map(|(p, _)| *p).collect();
        let reboot: Vec<&str> = Action::Reboot.programs().iter().map(|(p, _)| *p).collect();
        for program in &logoff {
            assert!(
                !reboot.contains(program),
                "{program} is in both lists, so one action could carry out the other"
            );
        }
        assert!(!logoff.is_empty() && !reboot.is_empty());
    }

    /// **A restart asks the init system.** The bare `reboot` command is a
    /// direct kernel call on some machines and does not unmount anything;
    /// asking `systemctl` is asking for a clean restart, which is the only
    /// kind worth offering somebody remotely.
    #[test]
    fn a_restart_asks_the_init_system_rather_than_the_kernel() {
        let first = Action::Reboot
            .programs()
            .first()
            .copied()
            .expect("a program");
        assert_eq!(first.0, "systemctl");
        assert_eq!(first.1, &["reboot"]);
        assert!(
            !Action::Reboot
                .programs()
                .iter()
                .any(|(p, a)| *p == "reboot" && a.is_empty()),
            "a bare `reboot` does not unmount anything"
        );
    }

    /// Logging off ends *this* session and never one named by anything else.
    #[test]
    fn logging_off_ends_this_session_and_no_other() {
        let (program, args) = Action::Logoff
            .programs()
            .first()
            .copied()
            .expect("a program");
        assert_eq!(program, "loginctl");
        assert!(args.contains(&"self"), "{args:?}");
    }
}
