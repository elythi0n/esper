//! The device identity key.
//!
//! Identity in OwnView is an Ed25519 keypair on a machine. There is no account,
//! no username, and no server-side record of any kind. Two devices know each
//! other because they hold each other's public key, and for no other reason.
//!
//! Consequences accepted deliberately:
//!
//! - **Losing the key file is losing the identity.** Restore from backup or
//!   re-pair. There is no recovery flow, because a recovery flow is an account
//!   system wearing a hat.
//! - **Identity does not follow you between machines.** A new laptop is a new
//!   device. This is correct: the thing being authorized is the machine.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use zeroize::Zeroize;

use crate::Fingerprint;

/// Length of an Ed25519 secret scalar and of a public key.
pub const KEY_LEN: usize = 32;

/// Required permissions on the key file: owner read/write only.
#[cfg(unix)]
const REQUIRED_MODE: u32 = 0o600;

/// What can go wrong loading or creating an identity.
#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    /// The key file is readable by someone other than its owner.
    ///
    /// Refused rather than warned about: the identity key is the entire trust
    /// model, and a warning is something a user scrolls past.
    #[error(
        "{path} is readable by others (mode {mode:o}); the device identity key must be {required:o}.\n\
         Fix with: chmod 600 {path}"
    )]
    Permissions {
        path: PathBuf,
        mode: u32,
        required: u32,
    },

    /// The file exists but does not contain a usable key.
    #[error("{path} is not a valid identity key: {reason}")]
    Malformed { path: PathBuf, reason: String },

    /// The key is a small-order point.
    ///
    /// Such a key decompresses successfully, so it looks valid, but it cannot
    /// serve as an identity: a weak key admits a signature valid for almost any
    /// message. Refused at the boundary rather than stored and puzzled over.
    #[error("the key is a small-order point and cannot be used as an identity")]
    WeakKey,

    /// Key material could not be encoded to or decoded from DER.
    #[error("key encoding failed: {0}")]
    Encoding(String),

    /// The operating system's random source was unavailable.
    ///
    /// Rare, and never recoverable by retrying in a tight loop: it means
    /// something is wrong with the machine (a seccomp filter, an exhausted file
    /// descriptor table, a container with no `/dev/urandom`). Generating a key
    /// from a degraded source would be far worse than failing, so this is an
    /// error rather than a fallback.
    #[error("the operating system random source is unavailable: {0}")]
    Entropy(String),

    /// Filesystem failure.
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// A device's own identity: a signing key and the public key derived from it.
///
/// The secret is zeroized on drop by `ed25519_dalek::SigningKey`, which carries
/// the `zeroize` feature. That is the reason for the feature rather than a
/// nice-to-have: a key left in freed memory is recoverable from a core dump or
/// a swap file.
pub struct Identity {
    signing: SigningKey,
}

impl std::fmt::Debug for Identity {
    /// Never print the secret. A `Debug` that leaks a key into a log is a
    /// classic way to lose one, so the impl is written rather than derived.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("fingerprint", &self.fingerprint().inline())
            .finish_non_exhaustive()
    }
}

impl Identity {
    /// Generate a fresh identity from the operating system's RNG.
    ///
    /// Fallible because a key generated from a failed entropy source is worse
    /// than no key at all — it would look like a working identity while being
    /// predictable. The seed is zeroized as soon as the key is derived from it.
    pub fn generate() -> Result<Self, IdentityError> {
        let mut seed = [0u8; KEY_LEN];
        let filled = getrandom::fill(&mut seed);
        if let Err(e) = filled {
            seed.zeroize();
            return Err(IdentityError::Entropy(e.to_string()));
        }
        let signing = SigningKey::from_bytes(&seed);
        seed.zeroize();
        Ok(Self { signing })
    }

    /// This device's public key.
    #[must_use]
    pub fn public_key(&self) -> [u8; KEY_LEN] {
        self.signing.verifying_key().to_bytes()
    }

    /// This device's fingerprint, for a human to read aloud.
    #[must_use]
    pub fn fingerprint(&self) -> Fingerprint {
        Fingerprint::of(&self.public_key())
    }

    /// Sign a message with the device key.
    #[must_use]
    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.signing.sign(message).to_bytes()
    }

    /// Verify a signature against a peer's public key.
    ///
    /// Uses **`verify_strict`**, not `verify`. The difference is not cosmetic.
    /// The ordinary `verify` accepts *weak* public keys — small-order points —
    /// and dalek's own documentation notes that a weak key can produce a single
    /// signature valid for almost any message. `verify_strict` refuses them, and
    /// additionally rejects non-canonical `R` values, closing the malleability
    /// gap that lets one signature be re-encoded into a different-looking valid
    /// one.
    ///
    /// Returns `false` for a malformed key, a weak key, or a bad signature
    /// without distinguishing them: to a caller, "this did not verify" is the
    /// only actionable outcome, and separate error paths invite treating one
    /// case as less severe than another.
    #[must_use]
    pub fn verify(public_key: &[u8; KEY_LEN], message: &[u8], signature: &[u8; 64]) -> bool {
        let Ok(key) = VerifyingKey::from_bytes(public_key) else {
            return false;
        };
        key.verify_strict(message, &Signature::from_bytes(signature))
            .is_ok()
    }

    /// Whether a public key is usable as a peer identity.
    ///
    /// A key must decompress to a curve point **and not be of small order**.
    /// `VerifyingKey::from_bytes` performs only the first check, so a
    /// small-order point is accepted by it and stores perfectly well — then
    /// silently authenticates nothing, or, if any code path ever used
    /// non-strict verification, authenticates everything.
    ///
    /// Checked where a key enters the device store, so a useless or dangerous
    /// key is refused when offered rather than discovered at 3am when a session
    /// will not open.
    #[must_use]
    pub fn public_key_is_usable(public_key: &[u8; KEY_LEN]) -> bool {
        VerifyingKey::from_bytes(public_key).is_ok_and(|k| !k.is_weak())
    }

    /// Load an identity from disk, creating one if the file does not exist.
    ///
    /// Creation is atomic: the key is written to a temporary file with the
    /// right mode and then renamed, so a crash mid-write cannot leave a
    /// truncated key that would silently become a different device.
    pub fn load_or_create(path: &Path) -> Result<Self, IdentityError> {
        match Self::load(path) {
            Ok(id) => Ok(id),
            Err(IdentityError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
                let id = Self::generate()?;
                id.save(path)?;
                Ok(id)
            }
            Err(e) => Err(e),
        }
    }

    /// Load an identity, refusing a file that others can read.
    pub fn load(path: &Path) -> Result<Self, IdentityError> {
        let meta = fs::metadata(path).map_err(|source| IdentityError::Io {
            path: path.to_owned(),
            source,
        })?;
        check_permissions(path, &meta)?;

        let mut bytes = fs::read(path).map_err(|source| IdentityError::Io {
            path: path.to_owned(),
            source,
        })?;

        let result = <[u8; KEY_LEN]>::try_from(bytes.as_slice())
            .map(|seed| Self {
                signing: SigningKey::from_bytes(&seed),
            })
            .map_err(|_| IdentityError::Malformed {
                path: path.to_owned(),
                reason: format!("expected {KEY_LEN} bytes, found {}", bytes.len()),
            });

        bytes.zeroize();
        result
    }

    /// Write the identity to disk with owner-only permissions, atomically.
    pub fn save(&self, path: &Path) -> Result<(), IdentityError> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|source| IdentityError::Io {
                path: dir.to_owned(),
                source,
            })?;
        }

        let tmp = path.with_extension("tmp");
        let mut secret = self.signing.to_bytes();
        let write = write_private(&tmp, &secret);
        secret.zeroize();

        write.map_err(|source| IdentityError::Io {
            path: tmp.clone(),
            source,
        })?;

        fs::rename(&tmp, path).map_err(|source| {
            let _ = fs::remove_file(&tmp);
            IdentityError::Io {
                path: path.to_owned(),
                source,
            }
        })
    }
}

/// Write `bytes` to `path` with owner-only permissions, set at creation time.
///
/// The mode is applied by `OpenOptions` rather than by a `chmod` afterwards:
/// creating a world-readable file and then narrowing it leaves a window in
/// which the key is exposed, and on a shared machine that window is enough.
#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(REQUIRED_MODE)
        .open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    // Windows ACLs are not mode bits. Until the platform work lands in Phase 6
    // this writes normally; `doctor` reports that the key is not protected.
    fs::write(path, bytes)
}

/// Refuse a key file that group or others can read.
#[cfg(unix)]
fn check_permissions(path: &Path, meta: &fs::Metadata) -> Result<(), IdentityError> {
    use std::os::unix::fs::PermissionsExt;
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(IdentityError::Permissions {
            path: path.to_owned(),
            mode,
            required: REQUIRED_MODE,
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_permissions(_path: &Path, _meta: &fs::Metadata) -> Result<(), IdentityError> {
    Ok(())
}

impl Identity {
    /// This device's public key as a DER `SubjectPublicKeyInfo`.
    ///
    /// This is the exact byte string a peer presents in a TLS 1.3 handshake
    /// under RFC 7250, so it is what a stored peer key is compared against.
    /// Producing it here rather than in the transport keeps every encoding of
    /// key material in one crate.
    pub fn public_key_spki(&self) -> Result<Vec<u8>, IdentityError> {
        use ed25519_dalek::pkcs8::EncodePublicKey;
        self.signing
            .verifying_key()
            .to_public_key_der()
            .map(|d| d.as_bytes().to_vec())
            .map_err(|e| IdentityError::Encoding(e.to_string()))
    }

    /// This device's private key as DER PKCS#8, for handing to a TLS provider.
    ///
    /// The caller receives secret material and is responsible for zeroizing it.
    /// It is returned rather than held because rustls wants ownership, and
    /// keeping a second copy alive here would be a second thing to wipe.
    pub fn private_key_pkcs8(&self) -> Result<Vec<u8>, IdentityError> {
        use ed25519_dalek::pkcs8::EncodePrivateKey;
        self.signing
            .to_pkcs8_der()
            .map(|d| d.as_bytes().to_vec())
            .map_err(|e| IdentityError::Encoding(e.to_string()))
    }

    /// The DER `SubjectPublicKeyInfo` for a peer's raw public key.
    ///
    /// Used to turn a stored 32-byte peer key into the form presented on the
    /// wire, so the comparison happens on identical encodings rather than on
    /// something reconstructed differently at each call site.
    pub fn spki_for(public_key: &[u8; KEY_LEN]) -> Result<Vec<u8>, IdentityError> {
        use ed25519_dalek::pkcs8::EncodePublicKey;
        let vk = VerifyingKey::from_bytes(public_key)
            .map_err(|e| IdentityError::Encoding(format!("not a valid Ed25519 key: {e}")))?;
        // A small-order point decompresses fine and is useless or dangerous as
        // an identity. Refuse it here so it can never reach a device store.
        if vk.is_weak() {
            return Err(IdentityError::WeakKey);
        }
        vk.to_public_key_der()
            .map(|d| d.as_bytes().to_vec())
            .map_err(|e| IdentityError::Encoding(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> PathBuf {
        let mut p = std::env::temp_dir();
        let mut n = [0u8; 8];
        getrandom::fill(&mut n).expect("test entropy");
        p.push(format!("ownview-test-{}", u64::from_le_bytes(n)));
        fs::create_dir_all(&p).expect("mkdir");
        p
    }

    #[test]
    fn generates_a_usable_keypair() {
        let id = Identity::generate().expect("generate");
        assert_eq!(id.public_key().len(), KEY_LEN);
        assert_ne!(
            id.public_key(),
            [0u8; KEY_LEN],
            "public key must not be zero"
        );
    }

    #[test]
    fn two_generated_identities_differ() {
        let a = Identity::generate().expect("a");
        let b = Identity::generate().expect("b");
        assert_ne!(a.public_key(), b.public_key());
        assert_ne!(a.fingerprint(), b.fingerprint());
    }

    #[test]
    fn signs_and_verifies() {
        let id = Identity::generate().expect("generate");
        let msg = b"the quick brown fox";
        let sig = id.sign(msg);
        assert!(Identity::verify(&id.public_key(), msg, &sig));
    }

    #[test]
    fn a_tampered_message_does_not_verify() {
        let id = Identity::generate().expect("generate");
        let sig = id.sign(b"transfer 10");
        assert!(!Identity::verify(&id.public_key(), b"transfer 20", &sig));
    }

    #[test]
    fn a_tampered_signature_does_not_verify() {
        let id = Identity::generate().expect("generate");
        let msg = b"hello";
        let mut sig = id.sign(msg);
        for i in 0..sig.len() {
            let mut bad = sig;
            if let Some(b) = bad.get_mut(i) {
                *b ^= 0x01;
            }
            assert!(
                !Identity::verify(&id.public_key(), msg, &bad),
                "flipping a bit in signature byte {i} still verified"
            );
        }
        sig.zeroize();
    }

    #[test]
    fn another_devices_key_does_not_verify() {
        let a = Identity::generate().expect("a");
        let b = Identity::generate().expect("b");
        let sig = a.sign(b"x");
        assert!(!Identity::verify(&b.public_key(), b"x", &sig));
    }

    #[test]
    fn a_malformed_public_key_is_rejected_rather_than_panicking() {
        let id = Identity::generate().expect("generate");
        let sig = id.sign(b"x");
        // Not a valid curve point.
        assert!(!Identity::verify(&[0xFFu8; KEY_LEN], b"x", &sig));
        assert!(!Identity::verify(&[0u8; KEY_LEN], b"x", &sig));
    }

    #[cfg_attr(miri, ignore = "touches the filesystem; MIRI runs with isolation")]
    #[test]
    fn round_trips_through_disk() {
        let dir = tmpdir();
        let path = dir.join("identity.key");
        let original = Identity::generate().expect("generate");
        original.save(&path).expect("save");

        let loaded = Identity::load(&path).expect("load");
        assert_eq!(loaded.public_key(), original.public_key());
        assert_eq!(loaded.fingerprint(), original.fingerprint());

        // The signing key survives too, not just the public half.
        let sig = loaded.sign(b"proof");
        assert!(Identity::verify(&original.public_key(), b"proof", &sig));
        fs::remove_dir_all(&dir).ok();
    }

    #[cfg_attr(miri, ignore = "touches the filesystem; MIRI runs with isolation")]
    #[test]
    fn load_or_create_is_stable_across_calls() {
        let dir = tmpdir();
        let path = dir.join("identity.key");
        let first = Identity::load_or_create(&path).expect("first");
        let second = Identity::load_or_create(&path).expect("second");
        assert_eq!(
            first.public_key(),
            second.public_key(),
            "load_or_create must not mint a new identity when one exists"
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[cfg_attr(miri, ignore = "touches the filesystem; MIRI runs with isolation")]
    #[test]
    fn load_or_create_makes_the_parent_directory() {
        let dir = tmpdir();
        let path = dir.join("nested").join("deeper").join("identity.key");
        Identity::load_or_create(&path).expect("create");
        assert!(path.exists());
        fs::remove_dir_all(&dir).ok();
    }

    /// The identity key is the entire trust model. A file others can read is
    /// refused, not warned about — a warning is something a user scrolls past.
    #[cfg(unix)]
    #[cfg_attr(miri, ignore = "touches the filesystem; MIRI runs with isolation")]
    #[test]
    fn a_group_or_world_readable_key_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmpdir();
        let path = dir.join("identity.key");
        Identity::generate()
            .expect("generate")
            .save(&path)
            .expect("save");

        for bad_mode in [0o644, 0o640, 0o604, 0o666, 0o777] {
            fs::set_permissions(&path, fs::Permissions::from_mode(bad_mode)).expect("chmod");
            match Identity::load(&path) {
                Err(IdentityError::Permissions { mode, .. }) => {
                    assert_eq!(mode, bad_mode);
                }
                other => panic!("mode {bad_mode:o} should be refused, got {other:?}"),
            }
        }
        fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[cfg_attr(miri, ignore = "touches the filesystem; MIRI runs with isolation")]
    #[test]
    fn owner_only_modes_are_accepted() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmpdir();
        let path = dir.join("identity.key");
        Identity::generate()
            .expect("generate")
            .save(&path)
            .expect("save");
        for ok_mode in [0o600, 0o400] {
            fs::set_permissions(&path, fs::Permissions::from_mode(ok_mode)).expect("chmod");
            assert!(
                Identity::load(&path).is_ok(),
                "mode {ok_mode:o} should load"
            );
        }
        fs::remove_dir_all(&dir).ok();
    }

    /// A key created by `save` must already be owner-only. Creating it wide and
    /// narrowing afterwards would leave a window in which it is exposed.
    #[cfg(unix)]
    #[cfg_attr(miri, ignore = "touches the filesystem; MIRI runs with isolation")]
    #[test]
    fn a_saved_key_is_owner_only_from_the_moment_it_exists() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmpdir();
        let path = dir.join("identity.key");
        Identity::generate()
            .expect("generate")
            .save(&path)
            .expect("save");
        let mode = fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, REQUIRED_MODE, "saved key has mode {mode:o}");
        fs::remove_dir_all(&dir).ok();
    }

    #[cfg_attr(miri, ignore = "touches the filesystem; MIRI runs with isolation")]
    #[test]
    fn a_truncated_key_file_is_reported_not_silently_accepted() {
        let dir = tmpdir();
        let path = dir.join("identity.key");
        Identity::generate()
            .expect("generate")
            .save(&path)
            .expect("save");

        let full = fs::read(&path).expect("read");
        let short = full.get(..16).expect("key is 32 bytes").to_vec();
        write_private(&path, &short).expect("write short");

        match Identity::load(&path) {
            Err(IdentityError::Malformed { reason, .. }) => {
                assert!(
                    reason.contains("16"),
                    "reason should name the length: {reason}"
                );
            }
            other => panic!("a truncated key must be reported, got {other:?}"),
        }
        fs::remove_dir_all(&dir).ok();
    }

    #[cfg_attr(miri, ignore = "touches the filesystem; MIRI runs with isolation")]
    #[test]
    fn an_empty_key_file_is_reported() {
        let dir = tmpdir();
        let path = dir.join("identity.key");
        write_private(&path, &[]).expect("write empty");
        assert!(matches!(
            Identity::load(&path),
            Err(IdentityError::Malformed { .. })
        ));
        fs::remove_dir_all(&dir).ok();
    }

    #[cfg_attr(miri, ignore = "touches the filesystem; MIRI runs with isolation")]
    #[test]
    fn a_missing_key_reports_not_found_rather_than_something_confusing() {
        let dir = tmpdir();
        match Identity::load(&dir.join("absent.key")) {
            Err(IdentityError::Io { source, .. }) => {
                assert_eq!(source.kind(), io::ErrorKind::NotFound);
            }
            other => panic!("expected NotFound, got {other:?}"),
        }
        fs::remove_dir_all(&dir).ok();
    }

    /// A save must not leave a stray temporary file behind holding the key.
    #[cfg_attr(miri, ignore = "touches the filesystem; MIRI runs with isolation")]
    #[test]
    fn save_leaves_no_temporary_file() {
        let dir = tmpdir();
        let path = dir.join("identity.key");
        Identity::generate()
            .expect("generate")
            .save(&path)
            .expect("save");
        let strays: Vec<_> = fs::read_dir(&dir)
            .expect("readdir")
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != "identity.key")
            .collect();
        assert!(strays.is_empty(), "save left {strays:?} behind");
        fs::remove_dir_all(&dir).ok();
    }

    /// The SPKI a peer sees must be the SPKI we would compare against. If
    /// these ever diverge, every connection fails with a confusing certificate
    /// error rather than a key mismatch.
    #[test]
    fn our_spki_matches_what_spki_for_produces_from_our_public_key() {
        let id = Identity::generate().expect("generate");
        let ours = id.public_key_spki().expect("spki");
        let derived = Identity::spki_for(&id.public_key()).expect("spki_for");
        assert_eq!(ours, derived);
    }

    /// An Ed25519 SPKI is 44 bytes: a 12-byte header plus the 32-byte key. This
    /// pins the encoding so a dependency change that alters it is caught.
    #[test]
    fn the_spki_is_the_expected_shape() {
        let id = Identity::generate().expect("generate");
        let spki = id.public_key_spki().expect("spki");
        assert_eq!(spki.len(), 44, "Ed25519 SPKI should be 44 bytes");
        assert_eq!(
            spki.get(..12).expect("header"),
            &[
                0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00
            ],
            "SPKI algorithm identifier changed"
        );
        assert_eq!(
            spki.get(12..).expect("key"),
            id.public_key().as_slice(),
            "the raw key must appear verbatim after the header"
        );
    }

    #[test]
    fn two_devices_have_different_spkis() {
        let a = Identity::generate()
            .expect("a")
            .public_key_spki()
            .expect("spki");
        let b = Identity::generate()
            .expect("b")
            .public_key_spki()
            .expect("spki");
        assert_ne!(a, b);
    }

    /// The weak keys dalek accepts from `from_bytes` but that must never become
    /// an identity. A weak key admits a signature valid for almost any message.
    ///
    /// These byte strings were confirmed weak against `VerifyingKey::is_weak`
    /// rather than copied from memory.
    fn weak_keys() -> Vec<(&'static str, [u8; KEY_LEN])> {
        let mut identity_point = [0u8; KEY_LEN];
        identity_point[0] = 1;
        let mut high_bit = [0u8; KEY_LEN];
        high_bit[KEY_LEN - 1] = 0x80;
        vec![
            ("all zeros (order 4)", [0u8; KEY_LEN]),
            ("identity point", identity_point),
            ("zeros with high bit set", high_bit),
        ]
    }

    #[test]
    fn weak_keys_are_refused_as_identities() {
        for (label, key) in weak_keys() {
            assert!(
                !Identity::public_key_is_usable(&key),
                "{label} should be refused as a peer identity"
            );
            assert!(
                Identity::spki_for(&key).is_err(),
                "{label} should not produce an SPKI"
            );
        }
    }

    #[test]
    fn a_real_generated_key_is_usable() {
        for _ in 0..16 {
            let id = Identity::generate().expect("generate");
            assert!(Identity::public_key_is_usable(&id.public_key()));
        }
    }

    /// The security property that motivated `verify_strict`: a weak key must not
    /// authenticate anything, even a signature that the non-strict path would
    /// have accepted.
    #[test]
    fn a_weak_key_verifies_nothing() {
        for (label, key) in weak_keys() {
            for sig in [[0u8; 64], [0xFFu8; 64], [0x5Au8; 64]] {
                assert!(
                    !Identity::verify(&key, b"anything", &sig),
                    "{label} accepted a signature"
                );
            }
        }
    }

    #[test]
    fn the_pkcs8_key_round_trips_to_the_same_identity() {
        use ed25519_dalek::pkcs8::DecodePrivateKey;
        let id = Identity::generate().expect("generate");
        let der = id.private_key_pkcs8().expect("pkcs8");
        let back = SigningKey::from_pkcs8_der(&der).expect("decode");
        assert_eq!(back.verifying_key().to_bytes(), id.public_key());
    }

    /// Debug must never print key material — a leaked key in a log is a classic
    /// way to lose one, so the impl is written rather than derived.
    #[test]
    fn debug_does_not_leak_the_secret() {
        let id = Identity::generate().expect("generate");
        let rendered = format!("{id:?}");
        let secret_hex: String = id
            .signing
            .to_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert!(
            !rendered.contains(&secret_hex),
            "Debug leaked the signing key"
        );
        assert!(
            rendered.contains("fingerprint"),
            "Debug should show the fingerprint"
        );
    }
}
