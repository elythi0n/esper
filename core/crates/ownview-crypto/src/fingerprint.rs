//! Rendering a device key as something a person can say out loud.

use sha2::{Digest, Sha256};

use crate::wordlist::{FINGERPRINT_WORDS, WORDS};

/// A device fingerprint: 64 bits derived from a public key, rendered as eight
/// words.
///
/// The fingerprint is **not** the key. It is a short, human-comparable digest
/// of it, used when two people need to confirm out of band that they are
/// looking at the same device. Authentication always uses the full key.
///
/// 64 bits is the bar for a value a human compares: finding a second key with
/// the same fingerprint costs an attacker roughly 2^32 work by the birthday
/// bound, which is enough to make it not the easy way in — and the easy way in
/// is not attacked here anyway, because pairing establishes the key directly
/// rather than by fingerprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fingerprint([u8; FINGERPRINT_WORDS]);

impl Fingerprint {
    /// Derive a fingerprint from a public key.
    ///
    /// The key is hashed rather than truncated. Truncating would tie the
    /// fingerprint to the key's internal encoding, so a future key type would
    /// silently change every fingerprint; a hash of the bytes with a domain
    /// separator does not.
    #[must_use]
    pub fn of(public_key: &[u8]) -> Self {
        let mut h = Sha256::new();
        // Domain separation: this digest must never collide with any other use
        // of SHA-256 over the same key material elsewhere in the protocol.
        h.update(b"ownview-device-fingerprint-v1");
        h.update(public_key);
        let digest = h.finalize();

        // SHA-256 is 32 bytes and FINGERPRINT_WORDS is 8, so this cannot be
        // short — but the workspace forbids indexing, and a compile-time proof
        // is better than a comment claiming one.
        let mut out = [0u8; FINGERPRINT_WORDS];
        for (slot, byte) in out.iter_mut().zip(digest.iter()) {
            *slot = *byte;
        }
        Self(out)
    }

    /// The raw bytes, for comparison and storage.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; FINGERPRINT_WORDS] {
        &self.0
    }

    /// The words, in order.
    #[must_use]
    pub fn words(&self) -> [&'static str; FINGERPRINT_WORDS] {
        let mut out = [""; FINGERPRINT_WORDS];
        for (slot, byte) in out.iter_mut().zip(self.0.iter()) {
            // `WORDS` has exactly 256 entries and a byte indexes it totally, so
            // this cannot be out of range. `get` rather than `[]` because the
            // workspace forbids indexing — and the compiler cannot prove the
            // bound here even though it holds.
            *slot = WORDS.get(*byte as usize).copied().unwrap_or("invalid");
        }
        out
    }

    /// Render for reading aloud: `absurd zinc maple ... ` in two lines of four.
    ///
    /// Grouped rather than run together because eight words in one line is read
    /// as a sentence and skimmed; four and four is read as a code and checked.
    #[must_use]
    pub fn display(&self) -> String {
        let w = self.words();
        let (first, second) = w.split_at(FINGERPRINT_WORDS / 2);
        format!("{}\n{}", first.join(" "), second.join(" "))
    }

    /// Render on one line, for logs and narrow UI.
    #[must_use]
    pub fn inline(&self) -> String {
        self.words().join(" ")
    }

    /// Hex, for machine comparison and bug reports. Never shown as the primary
    /// representation.
    #[must_use]
    pub fn hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}

impl std::fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.inline())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn the_same_key_always_gives_the_same_fingerprint() {
        let k = [0x11u8; 32];
        assert_eq!(Fingerprint::of(&k), Fingerprint::of(&k));
    }

    #[test]
    fn different_keys_give_different_fingerprints() {
        let a = Fingerprint::of(&[0x11u8; 32]);
        let b = Fingerprint::of(&[0x12u8; 32]);
        assert_ne!(a, b);
    }

    /// A one-bit change anywhere in the key must change the fingerprint. If it
    /// did not, two devices could look identical to a person comparing them.
    #[test]
    fn a_single_bit_flip_changes_the_fingerprint() {
        let base = [0u8; 32];
        let original = Fingerprint::of(&base);
        for byte in 0..32 {
            for bit in 0..8 {
                let mut k = base;
                if let Some(b) = k.get_mut(byte) {
                    *b ^= 1 << bit;
                }
                assert_ne!(
                    Fingerprint::of(&k),
                    original,
                    "flipping bit {bit} of byte {byte} did not change the fingerprint"
                );
            }
        }
    }

    #[test]
    fn renders_exactly_eight_words() {
        let f = Fingerprint::of(&[0xABu8; 32]);
        assert_eq!(f.words().len(), FINGERPRINT_WORDS);
        assert_eq!(f.inline().split(' ').count(), FINGERPRINT_WORDS);
    }

    #[test]
    fn every_rendered_word_comes_from_the_list() {
        // Sweep enough keys to hit a wide spread of bytes.
        for i in 0u8..=255 {
            let f = Fingerprint::of(&[i; 32]);
            for w in f.words() {
                assert!(WORDS.contains(&w), "{w:?} is not in the wordlist");
            }
        }
    }

    #[test]
    fn display_is_two_lines_of_four() {
        let d = Fingerprint::of(&[1u8; 32]).display();
        let lines: Vec<&str> = d.lines().collect();
        assert_eq!(lines.len(), 2);
        for l in lines {
            assert_eq!(l.split(' ').count(), 4);
        }
    }

    #[test]
    fn hex_is_sixteen_characters() {
        assert_eq!(Fingerprint::of(&[7u8; 32]).hex().len(), FINGERPRINT_WORDS * 2);
    }

    /// The domain separator must actually separate: a fingerprint must not equal
    /// a plain SHA-256 prefix of the same key, or an attacker could reuse a
    /// digest computed elsewhere in the protocol.
    #[test]
    fn the_digest_is_domain_separated() {
        let key = [0x5Au8; 32];
        let plain = Sha256::digest(key);
        assert_ne!(
            Fingerprint::of(&key).as_bytes().as_slice(),
            plain.get(..FINGERPRINT_WORDS).expect("sha256 is 32 bytes"),
            "fingerprint is a bare SHA-256 prefix; the domain separator is not applied"
        );
    }

    /// A weak spread would mean many keys sharing a fingerprint, which defeats
    /// visual comparison. 512 distinct keys should give 512 distinct results.
    #[test]
    fn fingerprints_are_well_spread() {
        let mut seen = HashSet::new();
        for i in 0u16..512 {
            let mut k = [0u8; 32];
            k[..2].copy_from_slice(&i.to_le_bytes());
            seen.insert(Fingerprint::of(&k));
        }
        assert_eq!(seen.len(), 512, "fingerprints collided across 512 distinct keys");
    }
}
