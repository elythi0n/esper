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
    if let Ok(dir) = std::env::var("ESPER_HOME") {
        return Ok(PathBuf::from(dir));
    }
    let home =
        std::env::var("HOME").map_err(|_| "neither ESPER_HOME nor HOME is set".to_owned())?;
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

/// Seconds since the epoch, for a `paired_at` stamp.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
