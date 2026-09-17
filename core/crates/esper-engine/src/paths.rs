//! Where this device keeps its identity and its paired devices.
//!
//! One place, so the three commands that need them cannot disagree about
//! where they are — which is the kind of bug that presents as "pairing
//! worked but the host does not recognise me".

use std::path::PathBuf;

use esper_crypto::{DeviceStore, Identity};

/// The directory holding this device's state.
///
/// `ESPER_HOME` overrides it, which is what makes two engines on one machine
/// possible — and two engines on one machine is how a session can be tried
/// before there are two machines to try it on.
pub fn home() -> Result<PathBuf, String> {
    home_from(std::env::var("ESPER_HOME").ok(), std::env::var("HOME").ok())
}

/// The rule itself, apart from the environment that answers it.
///
/// Separated because the environment is process-global: setting a variable to
/// check what happens leaks into every other test in the binary, and
/// `set_var` is `unsafe` in this edition besides. The alternative this repo
/// uses elsewhere is a child process, which is the right answer when the thing
/// under test *is* a process — and this is four comparisons.
fn home_from(esper_home: Option<String>, unix_home: Option<String>) -> Result<PathBuf, String> {
    match esper_home {
        // **Set to nothing is refused, not treated as unset.** An empty value
        // makes every path below it relative, so the identity key and the
        // device list land in whatever directory the process happened to
        // start in — a fresh identity, nobody paired, and a trust store
        // somewhere nobody will look for it.
        //
        // Falling back to `$HOME/.esper` would be worse than refusing.
        // Somebody who set the variable at all plainly meant an isolated
        // home, and quietly using the *real* one instead is the dangerous
        // half of the two wrong answers: a test, a packaging script or a
        // service unit with an unset variable in it would write to the
        // machine owner's own store.
        Some(dir) if dir.trim().is_empty() => {
            return Err(
                "ESPER_HOME is set to nothing. Unset it to use the default, or give it a \
                 directory — an empty value would put this machine's identity key and its \
                 paired-device list in the current working directory"
                    .to_owned(),
            );
        }
        Some(dir) => return Ok(PathBuf::from(dir)),
        None => {}
    }
    let home = unix_home.ok_or_else(|| "neither ESPER_HOME nor HOME is set".to_owned())?;
    Ok(PathBuf::from(home).join(".esper"))
}

/// This device's long-term identity, created on first use.
pub fn identity() -> Result<Identity, String> {
    let dir = home()?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let path = dir.join("identity.key");
    Identity::load_or_create(&path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Where paired devices are recorded.
pub fn devices_path() -> Result<PathBuf, String> {
    Ok(home()?.join("devices.json"))
}

/// The paired devices, and anything that had to be dropped while reading them.
///
/// A hand-edited file that has gone wrong is *corrected and reported* rather
/// than refused: refusing means a device that cannot start because one line of
/// its device list is malformed, and the device list is not the part somebody
/// is trying to use.
pub fn devices() -> Result<(DeviceStore, Vec<String>), String> {
    let path = devices_path()?;
    if !path.exists() {
        return Ok((DeviceStore::new(), Vec::new()));
    }
    DeviceStore::load(&path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Write the paired devices back.
pub fn save_devices(store: &DeviceStore) -> Result<(), String> {
    let path = devices_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    }
    store
        .save(&path)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Where this machine's settings live.
///
/// Next to the identity and the device list, because it is the same directory
/// somebody backs up or deletes when they mean "this machine's Esper".
pub fn config_file() -> Result<PathBuf, String> {
    Ok(home()?.join("config.toml"))
}

/// Where this machine records what happened.
///
/// One file next to the identity and the device list, because an audit trail
/// somebody has to go looking for is an audit trail nobody reads. **Session
/// content is never in it** — see `esper-pty`'s own note on that.
pub fn audit_log() -> Result<PathBuf, String> {
    let dir = home()?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    Ok(dir.join("sessions.log"))
}

/// The directory file transfers happen inside.
///
/// **Not the home directory and not `~/.esper`.** Everything a peer can name
/// is relative to this and cannot leave it, so what this points at is the
/// whole of what a file permission reaches — and pointing it at a directory
/// that also holds the identity key would make a file permission a way to read
/// one.
///
/// `~/Esper` by default: a place a person can find, outside the dot-directory
/// the machine's own state lives in, and one they can move somewhere else in
/// the settings.
pub fn files_root() -> Result<PathBuf, String> {
    let named = crate::settings::text("files.root");
    if !named.trim().is_empty() {
        return Ok(expand(named.trim()));
    }
    let base = std::env::var_os("ESPER_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .ok_or_else(|| "no home directory to put a file directory in".to_owned())?;
    Ok(base.join("Esper"))
}

/// Where a session's file box lives.
///
/// **Inside the machine's own state directory, not the file directory.** The
/// file directory is somewhere a person chose and can browse; a box is
/// scaffolding for one session, and putting it there would leave a stranger's
/// files sitting in a folder somebody keeps their own work in.
///
/// Named for the session so two of them cannot collide, and so a box left
/// behind by a host that was killed can be told from one in use.
///
/// # Errors
///
/// If there is no home directory to put it in.
pub fn file_box(session: &str) -> Result<PathBuf, String> {
    Ok(boxes_root()?.join(session))
}

/// Where every session's box lives, which is what gets swept at startup.
///
/// # Errors
///
/// If there is no home directory to put it in.
pub fn boxes_root() -> Result<PathBuf, String> {
    Ok(home()?.join("boxes"))
}

/// Empty a session's box and take it away.
///
/// **Called when the session ends, however it ended.** A box that outlived its
/// session would be a directory of somebody else's files on this machine that
/// nothing owns and nobody agreed to keep.
///
/// # Errors
///
/// If the directory is there and will not go.
pub fn empty_the_box(session: &str) -> Result<(), String> {
    let path = file_box(session)?;
    match std::fs::remove_dir_all(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("emptying {}: {e}", path.display())),
    }
}

/// Take away every box a previous run left behind.
///
/// Called at startup, for the same reason the resolution and the port mapping
/// are put back there: a host that is killed runs no teardown, and what it
/// leaves is somebody else's files on this machine.
///
/// Returns how many were swept, so a host can say so rather than doing it
/// silently — a machine quietly deleting files is a machine nobody trusts with
/// files.
///
/// # Errors
///
/// Never fatally: a sweep that cannot read its own directory reports nothing
/// swept rather than refusing to start.
pub fn sweep_old_boxes() -> Result<usize, String> {
    let root = boxes_root()?;
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Ok(0);
    };
    let mut swept = 0;
    for entry in entries.flatten() {
        if std::fs::remove_dir_all(entry.path()).is_ok() {
            swept += 1;
        }
    }
    // The empty parent goes too, so a machine that never uses a box has no
    // directory for one.
    let _ = std::fs::remove_dir(&root);
    Ok(swept)
}

/// `~` at the start of a configured path, which a config file is written with
/// and a shell does not expand for us.
fn expand(text: &str) -> PathBuf {
    match text.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join(rest),
        None => PathBuf::from(text),
    }
}

/// Seconds since the epoch, for a `paired_at` stamp.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **An `ESPER_HOME` set to nothing is refused, not treated as unset.**
    ///
    /// The two wrong answers here are not equally wrong. Treating an empty
    /// value as "relative to here" puts the identity key and the paired-device
    /// list in whatever directory the process happened to start in: a fresh
    /// identity, nobody paired, and a trust store somewhere nobody will look
    /// for it. Falling back to the real `$HOME/.esper` is worse — somebody who
    /// set the variable at all plainly meant an isolated home, and a packaging
    /// script or a service unit with an unset variable in it would then write
    /// to the machine owner's own store.
    ///
    /// So it refuses, and the message says which of the two it was protecting
    /// against.
    #[test]
    fn an_empty_esper_home_is_refused_rather_than_guessed_at() {
        for empty in ["", " ", "\t", "  \n "] {
            let refused = home_from(Some(empty.to_owned()), Some("/home/somebody".to_owned()));
            let Err(why) = refused else {
                panic!("ESPER_HOME={empty:?} was accepted as {refused:?}");
            };
            assert!(
                why.contains("identity key") && why.contains("current working directory"),
                "the refusal does not say what it is protecting against: {why}"
            );
        }
    }

    #[test]
    fn esper_home_is_where_it_says() {
        assert_eq!(
            home_from(
                Some("/tmp/one".to_owned()),
                Some("/home/somebody".to_owned())
            ),
            Ok(PathBuf::from("/tmp/one")),
            "an isolated home is the one that was named"
        );
        // Two engines on one machine is how a session is tried before there
        // are two machines to try it on, so this outranks the real home.
        assert_eq!(
            home_from(Some("/tmp/two".to_owned()), None),
            Ok(PathBuf::from("/tmp/two")),
            "an isolated home does not need a real one beside it"
        );
        // Not trimmed: a directory whose name has a space in it is a
        // directory, and silently renaming it would be a different bug.
        assert_eq!(
            home_from(Some("/tmp/a home ".to_owned()), None),
            Ok(PathBuf::from("/tmp/a home ")),
            "a name with spaces in it is the name"
        );
    }

    #[test]
    fn with_no_esper_home_it_is_beside_the_users_own() {
        assert_eq!(
            home_from(None, Some("/home/somebody".to_owned())),
            Ok(PathBuf::from("/home/somebody/.esper"))
        );
    }

    /// With neither, there is nowhere — and saying so beats inventing one.
    #[test]
    fn with_no_home_at_all_it_refuses_and_names_both() {
        let Err(why) = home_from(None, None) else {
            panic!("a home was invented with nothing to go on");
        };
        assert!(
            why.contains("ESPER_HOME") && why.contains("HOME"),
            "the refusal does not name what would fix it: {why}"
        );
    }

    /// The store and the key sit under whichever home was chosen, so the three
    /// commands that need them cannot disagree about where they are — the bug
    /// that presents as "pairing worked but the host does not recognise me".
    #[test]
    fn the_store_is_under_the_home_that_was_chosen() {
        let dir = home_from(Some("/tmp/isolated".to_owned()), None).expect("a home");
        assert_eq!(
            dir.join("devices.json"),
            PathBuf::from("/tmp/isolated/devices.json")
        );
        assert_eq!(
            dir.join("identity.key"),
            PathBuf::from("/tmp/isolated/identity.key")
        );
    }
}
