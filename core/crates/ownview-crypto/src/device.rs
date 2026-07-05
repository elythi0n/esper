//! The paired-device store: who this machine trusts, and what each may do.
//!
//! Pairing establishes **identity**. It grants nothing. What a paired device is
//! permitted to do is a policy this machine holds, defaulting to the minimum,
//! and changing it is a deliberate act at the host.
//!
//! The store is deliberately separate from configuration. Config is what this
//! machine does; the store is who it trusts. Mixing them makes both harder to
//! reason about, and only one of them is safe to copy between machines.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{Fingerprint, identity::KEY_LEN};

/// What a paired device is allowed to do.
///
/// Each is granted separately, and each direction of a two-way capability is
/// its own permission: a device permitted to read this machine's clipboard is
/// not thereby permitted to write to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    /// See the screen.
    View,
    /// Inject keyboard and mouse.
    Control,
    /// Read this machine's clipboard.
    ClipboardOut,
    /// Write to this machine's clipboard.
    ClipboardIn,
    /// Receive files from this machine.
    FilesOut,
    /// Send files to this machine.
    FilesIn,
    /// Hear this machine's audio.
    Audio,
    /// Open a terminal session.
    Terminal,
    /// Connect with nobody present to approve.
    Unattended,
    /// Record the session locally.
    Record,
}

impl Permission {
    /// Permissions a freshly paired device receives.
    ///
    /// View and control, prompt on connect, nothing else — and emphatically not
    /// [`Permission::Unattended`], which is the dangerous one and is granted
    /// only by a human at the host.
    #[must_use]
    pub fn defaults() -> Vec<Self> {
        vec![Self::View, Self::Control]
    }

    /// Whether granting this permission should require explicit confirmation at
    /// the host rather than a checkbox.
    #[must_use]
    pub const fn is_sensitive(self) -> bool {
        matches!(self, Self::Unattended | Self::FilesIn | Self::Record)
    }
}

/// A device this machine has paired with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    /// The peer's Ed25519 public key. This *is* the device.
    #[serde(with = "key_bytes")]
    pub public_key: [u8; KEY_LEN],
    /// A human label. Peer-supplied at pairing, editable locally, untrusted.
    pub name: String,
    /// What this device is permitted to do.
    pub permissions: Vec<Permission>,
    /// Unix seconds when the pairing completed.
    pub paired_at: u64,
    /// Unix seconds of the last completed session, if any.
    pub last_seen: Option<u64>,
}

impl Device {
    /// Create a device record with default permissions.
    #[must_use]
    pub fn new(public_key: [u8; KEY_LEN], name: String, now: u64) -> Self {
        Self {
            public_key,
            name,
            permissions: Permission::defaults(),
            paired_at: now,
            last_seen: None,
        }
    }

    /// The fingerprint a human reads to confirm this is the right device.
    #[must_use]
    pub fn fingerprint(&self) -> Fingerprint {
        Fingerprint::of(&self.public_key)
    }

    /// Whether this device holds a permission.
    #[must_use]
    pub fn may(&self, p: Permission) -> bool {
        self.permissions.contains(&p)
    }

    /// Grant a permission. Idempotent.
    pub fn grant(&mut self, p: Permission) {
        if !self.may(p) {
            self.permissions.push(p);
            self.permissions.sort_unstable();
        }
    }

    /// Withdraw a permission. Idempotent.
    pub fn revoke(&mut self, p: Permission) {
        self.permissions.retain(|x| *x != p);
    }
}

/// Every device this machine trusts, keyed by public key.
///
/// An allowlist is the only mode: a key that is not in here cannot open a
/// session, and there is no anonymous or first-use-trusted path.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeviceStore {
    /// Schema version, so a future format change is detectable rather than
    /// producing a confusing parse error.
    #[serde(default = "one")]
    pub version: u32,
    /// Devices, keyed by lowercase hex of the public key. Hex rather than raw
    /// bytes because this file is meant to be readable by a human deciding
    /// whether to delete a line.
    #[serde(default)]
    devices: BTreeMap<String, Device>,
}

const fn one() -> u32 {
    1
}

impl DeviceStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self {
            version: 1,
            devices: BTreeMap::new(),
        }
    }

    /// Number of paired devices.
    #[must_use]
    pub fn len(&self) -> usize {
        self.devices.len()
    }

    /// Whether no devices are paired.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }

    /// Look up a device by public key.
    #[must_use]
    pub fn get(&self, public_key: &[u8; KEY_LEN]) -> Option<&Device> {
        self.devices.get(&hex_key(public_key))
    }

    /// Mutable lookup, for changing permissions or recording a session.
    pub fn get_mut(&mut self, public_key: &[u8; KEY_LEN]) -> Option<&mut Device> {
        self.devices.get_mut(&hex_key(public_key))
    }

    /// Add or replace a device.
    pub fn insert(&mut self, device: Device) {
        self.devices.insert(hex_key(&device.public_key), device);
    }

    /// Remove a device. Returns it if it was present.
    ///
    /// This is revocation, and it is complete: there is no server-side grant
    /// that could survive it. The caller is responsible for killing any live
    /// session from the same key, which must happen in the same operation.
    pub fn remove(&mut self, public_key: &[u8; KEY_LEN]) -> Option<Device> {
        self.devices.remove(&hex_key(public_key))
    }

    /// Every device, in a stable order.
    pub fn iter(&self) -> impl Iterator<Item = &Device> {
        self.devices.values()
    }

    /// Whether a key is paired at all. The first question every inbound
    /// connection asks, before any session state is allocated.
    #[must_use]
    pub fn is_paired(&self, public_key: &[u8; KEY_LEN]) -> bool {
        self.devices.contains_key(&hex_key(public_key))
    }

    /// Whether a key is paired *and* holds a permission.
    #[must_use]
    pub fn may(&self, public_key: &[u8; KEY_LEN], p: Permission) -> bool {
        self.get(public_key).is_some_and(|d| d.may(p))
    }
}

/// Lowercase hex of a public key, used as the store's map key.
fn hex_key(k: &[u8; KEY_LEN]) -> String {
    k.iter().map(|b| format!("{b:02x}")).collect()
}

/// Serialize a fixed-size key as hex, so the store is readable by a human
/// deciding whether to delete a line from it.
mod key_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    use crate::identity::KEY_LEN;

    pub fn serialize<S: Serializer>(v: &[u8; KEY_LEN], s: S) -> Result<S::Ok, S::Error> {
        let hex: String = v.iter().map(|b| format!("{b:02x}")).collect();
        s.serialize_str(&hex)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; KEY_LEN], D::Error> {
        let s = String::deserialize(d)?;
        if s.len() != KEY_LEN * 2 {
            return Err(serde::de::Error::invalid_length(
                s.len(),
                &"64 hex characters",
            ));
        }
        let mut out = [0u8; KEY_LEN];
        for (slot, pair) in out.iter_mut().zip(s.as_bytes().as_chunks::<2>().0) {
            let text = std::str::from_utf8(pair).map_err(serde::de::Error::custom)?;
            *slot = u8::from_str_radix(text, 16).map_err(serde::de::Error::custom)?;
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u8) -> [u8; KEY_LEN] {
        [n; KEY_LEN]
    }

    fn device(n: u8) -> Device {
        Device::new(key(n), format!("device-{n}"), 1_756_684_800)
    }

    /// The single most important default in the product. A freshly paired
    /// device must not be able to connect with nobody present.
    #[test]
    fn a_new_device_never_gets_unattended_access() {
        let d = device(1);
        assert!(!d.may(Permission::Unattended));
        assert!(d.may(Permission::View));
        assert!(d.may(Permission::Control));
    }

    /// Everything beyond view and control must be asked for.
    #[test]
    fn a_new_device_gets_nothing_beyond_view_and_control() {
        let d = device(1);
        for p in [
            Permission::ClipboardIn,
            Permission::ClipboardOut,
            Permission::FilesIn,
            Permission::FilesOut,
            Permission::Audio,
            Permission::Terminal,
            Permission::Unattended,
            Permission::Record,
        ] {
            assert!(!d.may(p), "a new device should not hold {p:?}");
        }
    }

    #[test]
    fn the_dangerous_permissions_are_marked_sensitive() {
        assert!(Permission::Unattended.is_sensitive());
        assert!(Permission::FilesIn.is_sensitive());
        assert!(Permission::Record.is_sensitive());
        assert!(!Permission::View.is_sensitive());
    }

    #[test]
    fn granting_is_idempotent() {
        let mut d = device(1);
        d.grant(Permission::Audio);
        d.grant(Permission::Audio);
        assert_eq!(
            d.permissions.iter().filter(|p| **p == Permission::Audio).count(),
            1
        );
    }

    #[test]
    fn revoking_is_idempotent_and_actually_removes() {
        let mut d = device(1);
        d.revoke(Permission::Control);
        d.revoke(Permission::Control);
        assert!(!d.may(Permission::Control));
        assert!(d.may(Permission::View), "revoking one must not remove others");
    }

    #[test]
    fn an_unpaired_key_is_not_paired_and_holds_nothing() {
        let store = DeviceStore::new();
        assert!(!store.is_paired(&key(9)));
        assert!(!store.may(&key(9), Permission::View));
        assert!(store.get(&key(9)).is_none());
    }

    #[test]
    fn insert_and_lookup_round_trip() {
        let mut store = DeviceStore::new();
        store.insert(device(3));
        assert!(store.is_paired(&key(3)));
        assert_eq!(store.get(&key(3)).map(|d| d.name.as_str()), Some("device-3"));
        assert_eq!(store.len(), 1);
    }

    /// Revocation must be complete and immediate — there is no server-side
    /// grant that could disagree with it.
    #[test]
    fn removing_a_device_revokes_everything_at_once() {
        let mut store = DeviceStore::new();
        let mut d = device(4);
        d.grant(Permission::Unattended);
        d.grant(Permission::FilesIn);
        store.insert(d);

        assert!(store.may(&key(4), Permission::Unattended));
        let removed = store.remove(&key(4));
        assert!(removed.is_some());

        assert!(!store.is_paired(&key(4)));
        for p in [Permission::View, Permission::Unattended, Permission::FilesIn] {
            assert!(!store.may(&key(4), p), "{p:?} survived revocation");
        }
    }

    #[test]
    fn removing_an_absent_device_is_not_an_error() {
        let mut store = DeviceStore::new();
        assert!(store.remove(&key(7)).is_none());
    }

    #[test]
    fn inserting_the_same_key_replaces_rather_than_duplicates() {
        let mut store = DeviceStore::new();
        store.insert(device(5));
        let mut updated = device(5);
        updated.name = "renamed".to_owned();
        store.insert(updated);
        assert_eq!(store.len(), 1);
        assert_eq!(store.get(&key(5)).map(|d| d.name.as_str()), Some("renamed"));
    }

    #[test]
    fn iteration_is_in_a_stable_order() {
        let mut store = DeviceStore::new();
        for n in [9u8, 1, 5, 3] {
            store.insert(device(n));
        }
        let first: Vec<_> = store.iter().map(|d| d.name.clone()).collect();
        let second: Vec<_> = store.iter().map(|d| d.name.clone()).collect();
        assert_eq!(first, second);
        assert_eq!(first.len(), 4);
    }

    #[test]
    fn a_device_reports_the_fingerprint_of_its_key() {
        let d = device(6);
        assert_eq!(d.fingerprint(), Fingerprint::of(&key(6)));
    }

    #[test]
    fn the_store_round_trips_through_json() {
        let mut store = DeviceStore::new();
        let mut d = device(8);
        d.grant(Permission::Audio);
        d.last_seen = Some(1_756_700_000);
        store.insert(d);

        let json = serde_json::to_string_pretty(&store).expect("encode");
        let back: DeviceStore = serde_json::from_str(&json).expect("decode");

        assert_eq!(back.len(), 1);
        let got = back.get(&key(8)).expect("device present");
        assert!(got.may(Permission::Audio));
        assert_eq!(got.last_seen, Some(1_756_700_000));
        assert_eq!(got.public_key, key(8));
    }

    /// The store is meant to be readable by a human deciding whether to delete
    /// a line, so keys are hex rather than an array of integers.
    #[test]
    fn keys_are_stored_as_readable_hex() {
        let mut store = DeviceStore::new();
        store.insert(device(0xAB));
        let json = serde_json::to_string(&store).expect("encode");
        assert!(
            json.contains(&"ab".repeat(KEY_LEN)),
            "public key is not readable hex in: {json}"
        );
    }

    #[test]
    fn a_malformed_key_in_the_file_is_rejected() {
        let bad = r#"{"version":1,"devices":{"x":{"public_key":"notthirtytwobytes",
                     "name":"n","permissions":[],"paired_at":0,"last_seen":null}}}"#;
        assert!(serde_json::from_str::<DeviceStore>(bad).is_err());
    }

    #[test]
    fn an_empty_file_deserializes_to_an_empty_store() {
        let store: DeviceStore = serde_json::from_str("{}").expect("decode");
        assert!(store.is_empty());
        assert_eq!(store.version, 1, "version should default rather than fail");
    }
}
