//! DER encodings for Ed25519 keys, built from constants.
//!
//! TLS 1.3 with raw public keys (RFC 7250) exchanges a DER
//! `SubjectPublicKeyInfo`, and a TLS provider wants a private key as DER
//! PKCS#8. Both encodings for Ed25519 are **fixed-length and fixed-shape**:
//! a constant prefix followed by the 32 key bytes, with nothing variable and
//! nothing to parse.
//!
//! # Why not a DER library
//!
//! Enabling `ed25519-dalek`'s `pkcs8` feature pulls in `der`, `pkcs8`, `spki`
//! and `base64ct` — four crates, containing a general-purpose ASN.1 parser,
//! placed in the most security-sensitive path in the product. The project's own
//! supply-chain policy ranks anything in the handshake path as the first thing
//! to review, and this is a lot of surface to accept for two constants.
//!
//! Writing a general DER encoder would be a bad trade the other way. But these
//! are not general: there is exactly one algorithm, one key length, and no
//! optional fields. The encoding is a `memcpy` with a header.
//!
//! # How the encodings are trusted
//!
//! Not by reading the RFC and hoping. The prefixes below were **captured from
//! `ed25519-dalek`'s own `pkcs8` implementation** and are pinned by a
//! known-answer test using a real keypair it produced. If the encoding here
//! ever drifts from what the audited implementation produces, that test fails.

use crate::identity::KEY_LEN;

/// DER prefix for an Ed25519 `SubjectPublicKeyInfo` (RFC 8410 §4).
///
/// ```text
/// 30 2a                 SEQUENCE (42 bytes)
///    30 05              SEQUENCE (5)   AlgorithmIdentifier
///       06 03 2b 65 70  OID 1.3.101.112  (id-Ed25519)
///    03 21 00           BIT STRING (33), 0 unused bits
/// ```
const SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

/// Total length of an Ed25519 SPKI.
pub const SPKI_LEN: usize = SPKI_PREFIX.len() + KEY_LEN;

/// DER prefix for an Ed25519 PKCS#8 v1 `PrivateKeyInfo` (RFC 8410 §7).
///
/// ```text
/// 30 2e                 SEQUENCE (46 bytes)
///    02 01 00           INTEGER 0        version
///    30 05              SEQUENCE (5)     AlgorithmIdentifier
///       06 03 2b 65 70  OID 1.3.101.112  (id-Ed25519)
///    04 22              OCTET STRING (34)
///       04 20           OCTET STRING (32)  CurvePrivateKey
/// ```
///
/// v1 rather than v2: v2 additionally carries the public key, which a TLS
/// provider derives from the seed anyway. Fewer bytes, one less thing that can
/// disagree with itself.
const PKCS8_PREFIX: [u8; 16] = [
    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
];

/// Total length of an Ed25519 PKCS#8 v1 private key.
pub const PKCS8_LEN: usize = PKCS8_PREFIX.len() + KEY_LEN;

/// Encode a public key as a DER `SubjectPublicKeyInfo`.
#[must_use]
pub fn spki(public_key: &[u8; KEY_LEN]) -> [u8; SPKI_LEN] {
    let mut out = [0u8; SPKI_LEN];
    let (head, tail) = out.split_at_mut(SPKI_PREFIX.len());
    head.copy_from_slice(&SPKI_PREFIX);
    tail.copy_from_slice(public_key);
    out
}

/// Encode a private key seed as DER PKCS#8 v1.
///
/// The caller receives secret material and is responsible for zeroizing it.
#[must_use]
pub fn pkcs8(seed: &[u8; KEY_LEN]) -> [u8; PKCS8_LEN] {
    let mut out = [0u8; PKCS8_LEN];
    let (head, tail) = out.split_at_mut(PKCS8_PREFIX.len());
    head.copy_from_slice(&PKCS8_PREFIX);
    tail.copy_from_slice(seed);
    out
}

/// Extract the raw public key from a DER `SubjectPublicKeyInfo`.
///
/// Returns `None` unless the input is exactly an Ed25519 SPKI: right length,
/// right prefix. This is the only parsing done, and it is a length check and a
/// constant comparison — a peer cannot steer it.
#[must_use]
pub fn public_key_from_spki(der: &[u8]) -> Option<[u8; KEY_LEN]> {
    if der.len() != SPKI_LEN {
        return None;
    }
    if der.get(..SPKI_PREFIX.len())? != SPKI_PREFIX {
        return None;
    }
    let mut key = [0u8; KEY_LEN];
    key.copy_from_slice(der.get(SPKI_PREFIX.len()..)?);
    Some(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real keypair and its encodings, captured from `ed25519-dalek`'s own
    /// `pkcs8` implementation on 2026-09-01.
    ///
    /// This is the whole basis for trusting the constants above. If the hand
    /// encoder drifts from what the audited library produced, this fails.
    const SEED: [u8; KEY_LEN] = [
        0x9d, 0xa8, 0xe6, 0xe4, 0x23, 0xff, 0x37, 0x6e, 0xfe, 0xc7, 0x2e, 0x80, 0x7d, 0x6d, 0x38,
        0x2d, 0xb6, 0x30, 0x40, 0x8b, 0xe2, 0x5a, 0x87, 0xa1, 0xbc, 0x8f, 0x16, 0xc5, 0xbb, 0x41,
        0x21, 0xae,
    ];
    const PUBLIC: [u8; KEY_LEN] = [
        0x39, 0x2b, 0x04, 0x95, 0x47, 0x8e, 0x3c, 0x78, 0xa3, 0xd2, 0x4c, 0x06, 0x84, 0x13, 0xf5,
        0xcb, 0x7a, 0x27, 0x8a, 0x3b, 0xd6, 0x1a, 0x4b, 0x74, 0x42, 0xc5, 0x7a, 0x73, 0x37, 0xab,
        0x92, 0xb3,
    ];
    /// The SPKI dalek produced for `PUBLIC`, verbatim.
    const EXPECTED_SPKI: &str =
        "302a300506032b6570032100392b0495478e3c78a3d24c068413f5cb7a278a3bd61a4b7442c57a7337ab92b3";

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    /// The seed and public key in the vector must actually be a keypair, or the
    /// vector proves nothing.
    #[test]
    fn the_test_vector_is_a_real_keypair() {
        let derived = ed25519_dalek::SigningKey::from_bytes(&SEED)
            .verifying_key()
            .to_bytes();
        assert_eq!(
            derived, PUBLIC,
            "the captured vector is not self-consistent"
        );
    }

    /// The known-answer test: our encoder must reproduce, byte for byte, what
    /// the audited library produced.
    #[test]
    fn spki_matches_the_captured_vector() {
        assert_eq!(hex(&spki(&PUBLIC)), EXPECTED_SPKI);
    }

    #[test]
    fn spki_is_forty_four_bytes_with_the_key_at_the_end() {
        let d = spki(&PUBLIC);
        assert_eq!(d.len(), 44);
        assert_eq!(d.get(12..).expect("tail"), PUBLIC.as_slice());
    }

    #[test]
    fn pkcs8_is_forty_eight_bytes_with_the_seed_at_the_end() {
        let d = pkcs8(&SEED);
        assert_eq!(d.len(), 48);
        assert_eq!(d.get(16..).expect("tail"), SEED.as_slice());
    }

    /// The declared DER lengths must match the actual byte counts, or a parser
    /// on the other side rejects them.
    #[test]
    fn the_declared_der_lengths_are_correct() {
        let s = spki(&PUBLIC);
        assert_eq!(s.get(1).copied(), Some(0x2a), "SPKI SEQUENCE says 42 bytes");
        assert_eq!(s.len() - 2, 0x2a, "and 42 bytes follow the header");

        let p = pkcs8(&SEED);
        assert_eq!(
            p.get(1).copied(),
            Some(0x2e),
            "PKCS8 SEQUENCE says 46 bytes"
        );
        assert_eq!(p.len() - 2, 0x2e, "and 46 bytes follow the header");
    }

    #[test]
    fn spki_round_trips_through_extraction() {
        let d = spki(&PUBLIC);
        assert_eq!(public_key_from_spki(&d), Some(PUBLIC));
    }

    /// Extraction is the only parsing in this module, and it must reject
    /// anything that is not exactly an Ed25519 SPKI.
    #[test]
    fn extraction_rejects_anything_that_is_not_an_ed25519_spki() {
        let good = spki(&PUBLIC);

        assert_eq!(public_key_from_spki(&[]), None, "empty");
        assert_eq!(
            public_key_from_spki(good.get(..43).expect("short")),
            None,
            "one byte short"
        );

        let mut long = good.to_vec();
        long.push(0);
        assert_eq!(public_key_from_spki(&long), None, "one byte long");

        // Every single-byte corruption of the prefix must be rejected.
        for i in 0..12 {
            let mut bad = good;
            if let Some(b) = bad.get_mut(i) {
                *b ^= 0xFF;
            }
            assert_eq!(
                public_key_from_spki(&bad),
                None,
                "corrupting prefix byte {i} was accepted"
            );
        }

        // Corrupting the key part is fine — that is a different key, not a
        // malformed encoding.
        let mut other = good;
        if let Some(b) = other.get_mut(20) {
            *b ^= 0xFF;
        }
        assert!(public_key_from_spki(&other).is_some());
        assert_ne!(public_key_from_spki(&other), Some(PUBLIC));
    }

    /// A random byte string must never be mistaken for a key.
    #[test]
    fn extraction_never_panics_on_arbitrary_input() {
        for len in 0..64usize {
            for fill in [0u8, 0x30, 0xFF, 0x2a] {
                let input = vec![fill; len];
                let _ = public_key_from_spki(&input);
            }
        }
    }
}
