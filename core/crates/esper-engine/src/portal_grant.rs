//! The portal grant this machine was given once, kept so it need not ask again.
//!
//! # Why a file at all
//!
//! Because unattended access rests on it. A compositor that implements the
//! `RemoteDesktop` portal asks a person before any client may inject
//! anything, and the only way past that dialog on a machine nobody is sitting
//! at is a token it handed back the last time somebody agreed. Without one, a
//! host can hold the `unattended` permission and still refuse every session —
//! a promise broken at three in the morning.
//!
//! # Why not in `config.toml`
//!
//! It is not a setting. Nobody types it, nobody should edit it, and it is a
//! credential of a kind: it is the compositor's word that *this machine's
//! Esper* may take the keyboard without asking. It lives beside the identity,
//! at the same 0600, and is deleted the moment the compositor stops honouring
//! it — a token that is refused is worse than none, because it is a reason to
//! believe unattended access will work.

use std::path::Path;

/// What the file is called, inside whichever home this machine is using.
const FILE: &str = "portal.token";

/// The grant this machine holds, if it holds one.
///
/// Absent is the ordinary answer on a machine that has never been asked, and
/// on every machine whose compositor will not persist one.
#[must_use]
pub fn held() -> Option<String> {
    held_in(&crate::paths::home().ok()?)
}

/// The same, in a home given rather than resolved.
///
/// The home is a parameter so this can be checked without `ESPER_HOME`, which
/// is process-wide: a test that set it would be a test fighting every other
/// test in the binary, which is how a suite acquires a flake nobody can
/// reproduce.
#[must_use]
pub fn held_in(home: &Path) -> Option<String> {
    let text = std::fs::read_to_string(home.join(FILE)).ok()?;
    let token = text.trim().to_owned();
    if token.is_empty() { None } else { Some(token) }
}

/// Keep a grant the portal handed back.
///
/// # Errors
///
/// If the home cannot be written. Said rather than swallowed: a token that
/// was not kept means the next session asks again, and somebody should be
/// able to find out why.
pub fn keep(token: &str) -> Result<(), String> {
    keep_in(&crate::paths::home()?, token)
}

/// The same, in a home given rather than resolved.
///
/// # Errors
///
/// As [`keep`].
pub fn keep_in(home: &Path, token: &str) -> Result<(), String> {
    std::fs::create_dir_all(home).map_err(|e| format!("creating {}: {e}", home.display()))?;
    let path = home.join(FILE);
    std::fs::write(&path, format!("{token}\n"))
        .map_err(|e| format!("writing {}: {e}", path.display()))?;
    restrict(&path)
}

/// Let go of a grant the compositor no longer honours.
///
/// # Errors
///
/// If the file exists and cannot be removed.
pub fn forget() -> Result<(), String> {
    forget_in(&crate::paths::home()?)
}

/// The same, in a home given rather than resolved.
///
/// # Errors
///
/// As [`forget`].
pub fn forget_in(home: &Path) -> Result<(), String> {
    let path = home.join(FILE);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        // Nothing to forget is the ordinary case, not a failure.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("removing {}: {e}", path.display())),
    }
}

/// 0600, like the identity beside it.
#[cfg(unix)]
fn restrict(path: &std::path::Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("restricting {}: {e}", path.display()))
}

#[cfg(not(unix))]
fn restrict(_path: &std::path::Path) -> Result<(), String> {
    // No portal off Unix, so nothing here is ever written.
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// A grant kept and read back, at permissions nobody else can read.
    ///
    /// The home is a parameter rather than an environment variable, so this
    /// races nothing: `ESPER_HOME` is process-wide, and a test that set it
    /// would be a test fighting every other test in the binary.
    #[test]
    fn a_grant_is_kept_privately_and_let_go_of() {
        let home = std::env::temp_dir().join(format!("esper-grant-{}", std::process::id()));
        std::fs::remove_dir_all(&home).ok();

        assert_eq!(
            held_in(&home),
            None,
            "a machine that has never been asked holds none"
        );

        keep_in(&home, "a-grant-somebody-gave-once").unwrap();
        assert_eq!(
            held_in(&home).as_deref(),
            Some("a-grant-somebody-gave-once")
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(home.join(FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o777,
                0o600,
                "it is the compositor's word about this machine"
            );
        }

        // A token the compositor no longer honours is worse than none: it is a
        // reason to believe unattended access will work.
        forget_in(&home).unwrap();
        assert_eq!(held_in(&home), None);
        forget_in(&home).expect("forgetting nothing is not a failure");

        // Whitespace a shell or an editor left behind is not part of a token.
        std::fs::write(home.join(FILE), "  spaced  \n\n").unwrap();
        assert_eq!(held_in(&home).as_deref(), Some("spaced"));
        // And a file with nothing in it is no grant rather than an empty one.
        std::fs::write(home.join(FILE), "\n").unwrap();
        assert_eq!(held_in(&home), None);

        std::fs::remove_dir_all(&home).ok();
    }
}
