// This whole file is test code. Clippy's `allow-*-in-tests` settings only
// reach `#[cfg(test)]` modules, not integration tests, which are compiled as
// their own crates and so are held to the workspace's production lints.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Holding the Go reader of the device store to this one.
//!
//! Two implementations of one file format drift silently, and this file is the
//! one that says who a machine trusts — a disagreement about it is a device
//! shown that cannot connect, or worse, one hidden that can. So the corpus in
//! `testdata/fixtures` carries a store written by this side and the
//! fingerprints derived from its keys, and both languages read it.
//!
//! Regenerate with `ESPER_WRITE_FIXTURES=1 cargo test -p esper-crypto
//! --test store_fixtures`. Never hand-edit: a fixture edited to match a
//! reader is a fixture that has stopped checking anything.

use std::path::{Path, PathBuf};

use esper_crypto::{Device, DeviceStore, Fingerprint, LockAfter, Permission};

/// Where the shared corpus lives.
fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join("testdata/fixtures")
}

/// A key that is a usable Ed25519 point, derived so the fixture is stable.
///
/// Real keys rather than counting bytes: the Go side is checked against what
/// this file *would* hold, and the store refuses a key that is not a point on
/// the curve, so a made-up one could not be written at all.
fn key(seed: u8) -> [u8; 32] {
    use ed25519_dalek::SigningKey;
    SigningKey::from_bytes(&[seed; 32])
        .verifying_key()
        .to_bytes()
}

/// The store the fixture holds: three devices covering every optional field.
fn the_store() -> DeviceStore {
    let mut store = DeviceStore::new();

    // A device paired in person and kept, with nothing optional set: what a
    // freshly paired machine looks like.
    let plain = Device::new(key(1), "elythion".to_owned(), 1_700_000_000);
    store.insert(plain).expect("a usable key");

    // A device with every optional field set, so a reader that skips one is
    // caught by this fixture rather than by somebody's device list.
    let mut furnished = Device::new(key(2), "rack".to_owned(), 1_700_000_100);
    furnished.grant(Permission::Unattended);
    furnished.grant(Permission::Audio);
    furnished.last_seen = Some(1_700_000_200);
    furnished.last_address = Some("192.0.2.10:7420".to_owned());
    furnished.overlay_address = Some("[100::2]:7420".to_owned());
    furnished.unattended_when =
        Some(esper_clock::Schedule::parse("weekdays 09:00-17:00").expect("a schedule"));
    furnished.lock_after = Some(LockAfter::Always);
    furnished.last_mac = Some("02:00:5e:10:00:00".to_owned());
    furnished.join("ops").expect("a group name");
    store.insert(furnished).expect("a usable key");

    // A one-session invitation, unspent: the other shape `pairing` takes.
    let invited = Device::invited(key(3), "laptop".to_owned(), 1_700_000_300, 1_700_003_900);
    store.insert(invited).expect("a usable key");

    store
}

#[test]
fn the_store_fixture_is_what_this_build_writes() {
    let path = fixtures().join("device_store.json");
    let store = the_store();
    let written = serde_json::to_string_pretty(&store).expect("serialised");

    if std::env::var_os("ESPER_WRITE_FIXTURES").is_some() {
        std::fs::write(&path, format!("{written}\n")).expect("wrote the fixture");
    }

    let committed = std::fs::read_to_string(&path).expect("the fixture is committed");
    assert_eq!(
        committed.trim_end(),
        written,
        "the committed store fixture is not what this build writes"
    );
}

#[test]
fn the_store_fixture_reads_back_to_the_same_devices() {
    let path = fixtures().join("device_store.json");
    let (store, removed) = DeviceStore::load(&path).expect("the fixture loads");
    assert!(
        removed.is_empty(),
        "nothing in the fixture should be dropped"
    );
    assert_eq!(store.len(), 3);

    let elythion = store.get(&key(1)).expect("elythion is in the fixture");
    assert_eq!(elythion.permissions, Permission::defaults());
    assert_eq!(elythion.pairing, esper_crypto::Pairing::Kept);
    assert_eq!(elythion.last_seen, None);

    let rack = store.get(&key(2)).expect("rack is in the fixture");
    assert!(rack.may(Permission::Unattended));
    assert_eq!(rack.lock_after, Some(LockAfter::Always));
    assert_eq!(rack.groups, vec!["ops".to_owned()]);
    assert_eq!(rack.last_mac.as_deref(), Some("02:00:5e:10:00:00"));

    let laptop = store.get(&key(3)).expect("laptop is in the fixture");
    assert!(laptop.pairing.is_once());
    assert!(laptop.admits(1_700_000_400));
    assert!(!laptop.admits(1_700_004_000));
}

#[test]
fn the_fingerprint_fixture_is_what_this_build_derives() {
    let path = fixtures().join("device_fingerprints.json");

    // Keys chosen to exercise the whole byte range of the word list rather
    // than only the three the store holds.
    let derived: Vec<_> = (0u8..16)
        .map(|seed| {
            let k = key(seed);
            let fp = Fingerprint::of(&k);
            serde_json::json!({
                "public_key": k.iter().map(|b| format!("{b:02x}")).collect::<String>(),
                "hex": fp.hex(),
                "words": fp.words(),
                "inline": fp.inline(),
                "display": fp.display(),
            })
        })
        .collect();
    let written = serde_json::to_string_pretty(&serde_json::json!(derived)).expect("serialised");

    if std::env::var_os("ESPER_WRITE_FIXTURES").is_some() {
        std::fs::write(&path, format!("{written}\n")).expect("wrote the fixture");
    }

    let committed = std::fs::read_to_string(&path).expect("the fixture is committed");
    assert_eq!(
        committed.trim_end(),
        written,
        "the committed fingerprint fixture is not what this build derives"
    );
}

#[test]
fn the_fingerprint_fixture_is_not_a_truncated_key() {
    // The whole reason the derivation hashes rather than truncates. If this
    // ever passes by accident, the fixture is checking nothing.
    let committed = std::fs::read_to_string(fixtures().join("device_fingerprints.json"))
        .expect("the fixture is committed");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&committed).expect("parsed");
    for row in &rows {
        let k = row["public_key"].as_str().expect("a key");
        let hex = row["hex"].as_str().expect("a fingerprint");
        assert!(
            !k.starts_with(hex),
            "the fingerprint of {k} is the front of the key: it is being truncated, not hashed"
        );
    }
}

/// Two entries naming one key: what the map key actually decides.
///
/// Everything else about a misfiled entry is corrected by re-keying, so this
/// is the one case where the index changes the answer rather than only the
/// spelling. Pinned here because the Go reader has to give the same one, and
/// "whichever the map happened to hold" is not an answer either side can
/// reproduce.
#[test]
fn two_entries_naming_one_key_leave_the_later_one() {
    let dir = scratch();
    let path = dir.join("devices.json");
    let real = key(1);
    let hex: String = real.iter().map(|b| format!("{b:02x}")).collect();

    // Both entries are the same device; they differ only in the name and in
    // the key they are filed under.
    let text = format!(
        r#"{{
  "version": 1,
  "devices": {{
    "0000000000000000000000000000000000000000000000000000000000000000": {{
      "public_key": "{hex}",
      "name": "first",
      "permissions": ["view"],
      "paired_at": 1,
      "last_seen": null,
      "pairing": "kept"
    }},
    "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff": {{
      "public_key": "{hex}",
      "name": "second",
      "permissions": ["view"],
      "paired_at": 2,
      "last_seen": null,
      "pairing": "kept"
    }}
  }}
}}"#
    );
    std::fs::write(&path, text).expect("wrote the store");

    let (store, _) = DeviceStore::load(&path).expect("loads");
    assert_eq!(store.len(), 1, "one key is one device");
    assert_eq!(
        store.get(&real).expect("the device").name,
        "second",
        "entries are re-keyed in map order, so the later index wins"
    );
}

/// Two entries naming one key, filed under **differently cased** hex.
///
/// The store's map is a `BTreeMap<String, _>`, so its order is the order of
/// the text as written — and uppercase hex sorts before lowercase. A reader
/// that parsed the index before ordering it would normalise that away and pick
/// the other entry. Pinned because the Go reader keys its map on the raw text
/// for exactly this reason, and a comment claiming a reason is not a check.
#[test]
fn the_index_is_ordered_as_the_file_wrote_it() {
    let dir = scratch();
    let path = dir.join("cased.json");
    let real = key(1);
    let lower: String = real.iter().map(|b| format!("{b:02x}")).collect();
    let upper = lower.to_uppercase();

    let text = format!(
        r#"{{
  "version": 1,
  "devices": {{
    "{upper}": {{
      "public_key": "{lower}",
      "name": "filed in uppercase",
      "permissions": ["view"],
      "paired_at": 1,
      "last_seen": null,
      "pairing": "kept"
    }},
    "{lower}": {{
      "public_key": "{lower}",
      "name": "filed in lowercase",
      "permissions": ["view"],
      "paired_at": 2,
      "last_seen": null,
      "pairing": "kept"
    }}
  }}
}}"#
    );
    std::fs::write(&path, text).expect("wrote the store");

    let (store, _) = DeviceStore::load(&path).expect("loads");
    assert_eq!(store.len(), 1, "one key is one device");
    assert_eq!(
        store.get(&real).expect("the device").name,
        "filed in lowercase",
        "uppercase hex sorts first, so the lowercase index is the later one"
    );
}

/// A scratch directory of this test's own, without a dependency for it.
fn scratch() -> PathBuf {
    let path = std::env::temp_dir().join(format!("esper-store-fixtures-{}", std::process::id()));
    std::fs::create_dir_all(&path).expect("a scratch directory");
    path
}
