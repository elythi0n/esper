//! Listing paired devices, and changing what they may do.
//!
//! Pairing establishes identity and grants **view and control, nothing else**.
//! Anything beyond that — a terminal, the clipboard, files, unattended access
//! — is a deliberate act by somebody at this machine, which means there has to
//! be a way to perform it. Without one, the permissions exist in the store and
//! in the protocol and are unreachable, and a separate permission that can
//! never be granted is the same as a feature that does not work.
//!
//! # Why a fingerprint prefix
//!
//! Devices are named by key, and a key is 64 hex characters nobody will type.
//! A prefix is matched instead, and an **ambiguous prefix is refused rather
//! than resolved** — picking one of two devices for somebody who was clearly
//! not sure which they meant is how a permission ends up on the wrong machine.

use esper_crypto::{Device, DeviceStore};
use esper_proto::Permission;

use crate::paths;

/// What to do to a device's permissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Add them.
    Grant,
    /// Take them away.
    Revoke,
}

/// Print every paired device, with what it may do.
pub fn list() -> Result<(), String> {
    let (store, dropped) = paths::devices()?;
    for line in &dropped {
        eprintln!("  · {line}");
    }

    println!("📇 esper devices");
    println!();
    if store.is_empty() {
        println!("  no paired devices — run `esper-engine pair` on both machines");
        println!();
        return Ok(());
    }

    for device in store.iter() {
        println!("  {}  {}", device.fingerprint().hex(), device.name);
        println!("      may  {}", crate::net::permissions(device));
    }
    println!();
    println!("  grant more with:  esper-engine grant <fingerprint> <permission>...");
    println!("  permissions:      {}", every_permission());
    Ok(())
}

/// Every permission, for help text.
fn every_permission() -> String {
    Permission::ALL
        .iter()
        .map(|p| p.name())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Apply a change to one device's permissions.
///
/// The arguments are a fingerprint prefix and one or more permission names.
pub fn change(args: &[String], what: Change) -> Result<(), String> {
    let verb = match what {
        Change::Grant => "grant",
        Change::Revoke => "revoke",
    };
    let [prefix, names @ ..] = args else {
        return Err(format!(
            "usage: esper-engine {verb} <fingerprint> <permission>...\n\
             permissions: {}",
            every_permission()
        ));
    };
    if names.is_empty() {
        return Err(format!(
            "esper-engine {verb} needs at least one permission\npermissions: {}",
            every_permission()
        ));
    }

    // Parsed before anything is written, so a typo in the third name does not
    // leave the first two applied.
    let mut wanted = Vec::new();
    for name in names {
        let p = Permission::parse(name).ok_or_else(|| {
            format!(
                "unknown permission {name:?}\npermissions: {}",
                every_permission()
            )
        })?;
        if !wanted.contains(&p) {
            wanted.push(p);
        }
    }

    let (mut store, dropped) = paths::devices()?;
    for line in &dropped {
        eprintln!("  · {line}");
    }
    let key = *find(&store, prefix)?;
    let Some(device) = store.get_mut(&key) else {
        return Err("the device disappeared between finding it and changing it".to_owned());
    };

    for p in &wanted {
        match what {
            Change::Grant => device.grant(*p),
            Change::Revoke => device.revoke(*p),
        }
    }
    let name = device.name.clone();
    let now = crate::net::permissions(device);
    paths::save_devices(&store)?;

    let done = match what {
        Change::Grant => "granted",
        Change::Revoke => "revoked",
    };
    println!(
        "  {done} {} on {name}",
        wanted
            .iter()
            .map(|p| p.name())
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!("  may     {now}");
    if what == Change::Revoke {
        // Said out loud because it is the difference between this and what a
        // person may think they just did: a session already open keeps the
        // permissions it was granted until it ends.
        println!("  note    a session already open is not changed by this");
    }
    Ok(())
}

/// The one device whose fingerprint starts with `prefix`.
fn find<'a>(store: &'a DeviceStore, prefix: &str) -> Result<&'a [u8; 32], String> {
    let wanted = prefix.trim().to_ascii_lowercase().replace([' ', ':'], "");
    if wanted.is_empty() {
        return Err("no fingerprint given".to_owned());
    }
    let matches: Vec<&Device> = store
        .iter()
        .filter(|d| d.fingerprint().hex().starts_with(&wanted))
        .collect();
    match matches.as_slice() {
        [only] => Ok(&only.public_key),
        [] => Err(format!(
            "no paired device's fingerprint starts with {wanted:?} — `esper-engine devices` lists them"
        )),
        several => Err(format!(
            "{} paired devices start with {wanted:?}; give more of it:\n{}",
            several.len(),
            several
                .iter()
                .map(|d| format!("      {}  {}", d.fingerprint().hex(), d.name))
                .collect::<Vec<_>>()
                .join("\n")
        )),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    /// A device with a real key.
    ///
    /// Real because the store checks what it is given: a key that is not a
    /// point on the curve is refused on the way in, which is the Ed25519
    /// weak-key finding doing its job.
    fn device(name: &str) -> Device {
        let id = esper_crypto::Identity::generate().expect("a key");
        Device::new(id.public_key(), name.to_owned(), 0)
    }

    fn store_with(names: &[&str]) -> DeviceStore {
        let mut s = DeviceStore::new();
        for name in names {
            s.insert(device(name)).unwrap();
        }
        s
    }

    #[test]
    fn a_prefix_finds_the_one_device_it_names() {
        let s = store_with(&["laptop", "phone"]);
        let laptop = s.iter().find(|d| d.name == "laptop").unwrap();
        let hex = laptop.fingerprint().hex();
        let eight: String = hex.chars().take(8).collect();
        assert_eq!(find(&s, &eight).unwrap(), &laptop.public_key);
        // And the whole thing, which is what a script would paste.
        assert_eq!(find(&s, &hex).unwrap(), &laptop.public_key);
    }

    #[test]
    fn an_empty_prefix_matches_nothing_rather_than_everything() {
        // `starts_with("")` is true of every string, so without the check a
        // bare fingerprint argument would pick whichever device came first.
        let s = store_with(&["laptop"]);
        assert!(find(&s, "").is_err());
        assert!(find(&s, "   ").is_err());
    }

    #[test]
    fn an_ambiguous_prefix_is_refused_and_lists_the_candidates() {
        // A fingerprint is a hash, so a shared prefix cannot be constructed —
        // it is found. Sixteen possible first characters means a handful of
        // keys is plenty, and the loop is bounded so a failure fails rather
        // than hangs.
        let mut s = DeviceStore::new();
        let mut shared = None;
        for n in 0..64 {
            let d = device(&format!("device-{n}"));
            let hex = d.fingerprint().hex();
            let first: String = hex.chars().take(1).collect();
            if s.iter()
                .any(|other| other.fingerprint().hex().starts_with(&first))
            {
                shared = Some(first);
                s.insert(d).unwrap();
                break;
            }
            s.insert(d).unwrap();
        }
        let shared = shared.expect("64 random fingerprints without a shared first character");

        let e = find(&s, &shared).unwrap_err();
        assert!(e.contains("give more of it"), "{e}");
        // The candidates are listed, because "ambiguous" without them leaves
        // somebody guessing which two devices they meant.
        assert!(e.contains(&shared), "{e}");
    }

    #[test]
    fn an_unknown_prefix_says_how_to_find_the_right_one() {
        let s = store_with(&["laptop"]);
        // 'z' is not a hex digit, so no fingerprint can start with it.
        let e = find(&s, "zzzz").unwrap_err();
        assert!(e.contains("esper-engine devices"), "{e}");
    }
}
