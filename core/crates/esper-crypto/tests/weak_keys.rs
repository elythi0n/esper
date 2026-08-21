//! The keys no device may be trusted under, and the file that says which.
//!
//! `Identity::public_key_is_usable` refuses Ed25519's small-order points: a key
//! of small order is one anybody can produce a valid-looking signature under,
//! so a device filed with one is a device this engine will never talk to.
//!
//! Go has no curve library and needs the same answer — the device list it
//! serves is read from the store directly, with no engine running. So the set
//! lives in `testdata/fixtures/weak_keys.json`, and this is the half that says
//! the file is right: every key in it is one *this* implementation refuses, and
//! an ordinary key is not.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a test that panics on its own fixture is a failing test, which is the point"
)]

use std::path::PathBuf;

use esper_crypto::Identity;

fn key_of(hex: &str) -> [u8; 32] {
    let mut key = [0u8; 32];
    for (i, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).expect("hex");
    }
    key
}

#[test]
fn every_key_the_shared_list_names_is_one_this_build_refuses() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../testdata/fixtures/weak_keys.json");
    let text = std::fs::read_to_string(&path).expect("the weak-key fixture");
    let doc: serde_json::Value = serde_json::from_str(&text).expect("the fixture is JSON");
    let keys: Vec<String> = doc["keys"]
        .as_array()
        .expect("a list of keys")
        .iter()
        .map(|k| k.as_str().expect("hex").to_owned())
        .collect();

    // A floor: an empty list agrees with everything.
    assert!(
        keys.len() >= 8,
        "the shared list names {} key(s), and Ed25519 has eight small-order points",
        keys.len()
    );

    for hex in &keys {
        assert_eq!(hex.len(), 64, "a key that is not 32 bytes: {hex}");
        assert_eq!(hex.to_lowercase(), *hex, "the list is lowercase hex: {hex}");
        assert!(
            !Identity::public_key_is_usable(&key_of(hex)),
            "the shared list names {hex} as unusable and this build accepts it — \
             Go would refuse a device the engine trusts"
        );
    }

    // And the other direction, or a list of nothing-in-particular would pass.
    let ordinary = Identity::generate().expect("an identity");
    let mine = ordinary.public_key();
    assert!(
        Identity::public_key_is_usable(&mine),
        "this build refuses its own key"
    );
    let mine_hex: String = mine.iter().map(|b| format!("{b:02x}")).collect();
    assert!(
        !keys.contains(&mine_hex),
        "the shared list names an ordinary key as weak"
    );
}
