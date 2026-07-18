// This whole file is test code. Clippy's `allow-*-in-tests` settings only
// recognise `#[cfg(test)]` modules, so a helper in an integration test that is
// not itself `#[test]` reads as production code. The no-panic rule exists
// because a panic reachable from the network is a denial of service; a fixture
// loader that cannot find its fixture should fail loudly.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Cross-language wire compatibility.
//!
//! Every fixture in `testdata/fixtures` must decode to the expected value and
//! re-encode to byte-identical output. The Go implementation runs the same
//! assertions over the same files.
//!
//! If this fails, the two implementations have drifted and the protocol is
//! split. Fix the encoders — never regenerate the fixture to match whichever
//! side you happened to run.
//!
//! These tests read files, so they are skipped under MIRI, which interprets
//! code rather than executing syscalls. Nothing is lost: MIRI exists to find
//! undefined behavior in computation, and the decode path it would check is
//! covered by the crate's unit tests, which do run under it.

use esper_proto::{Hello, Role, read_frame, write_frame};

/// Locate `testdata/fixtures` from the crate directory.
fn fixture(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("crate is at <root>/core/crates/<name>")
        .join("testdata/fixtures")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

fn expected_hello() -> Hello {
    let mut h = Hello::new(Role::Host, [0xABu8; 32], "fixture-host");
    h.engine = "0.0.0".to_owned();
    h.caps = vec!["audio".to_owned(), "clipboard".to_owned()];
    h
}

#[test]
#[cfg_attr(miri, ignore = "reads fixture files; MIRI has no filesystem")]
fn hello_host_decodes_to_the_expected_value() {
    let bytes = fixture("hello_host.cbor");
    let got: Hello = read_frame(&mut bytes.as_slice()).expect("decode fixture");
    assert_eq!(got, expected_hello());
}

#[test]
#[cfg_attr(miri, ignore = "reads fixture files; MIRI has no filesystem")]
fn hello_host_re_encodes_byte_identically() {
    let bytes = fixture("hello_host.cbor");
    let decoded: Hello = read_frame(&mut bytes.as_slice()).expect("decode fixture");

    let mut out = Vec::new();
    write_frame(&mut out, &decoded).expect("re-encode");

    assert_eq!(
        hex(&out),
        hex(&bytes),
        "re-encoding the fixture produced different bytes — the Rust and Go \
         encoders have drifted, or a struct field was reordered"
    );
}

/// Field order is part of the wire format, because neither implementation sorts
/// map keys. Reordering a struct field on either side silently changes the
/// bytes, so the order is asserted explicitly here.
#[test]
#[cfg_attr(miri, ignore = "reads fixture files; MIRI has no filesystem")]
fn hello_field_order_is_locked() {
    let bytes = fixture("hello_host.cbor");
    let payload = bytes.get(4..).expect("frame has a payload");
    let text = String::from_utf8_lossy(payload);

    let order = ["proto", "engine", "device", "name", "role", "caps"];
    let mut last = 0usize;
    for key in order {
        let at = text
            .find(key)
            .unwrap_or_else(|| panic!("key {key:?} missing from the fixture"));
        assert!(
            at >= last,
            "field {key:?} is out of order — the Go and Rust structs must \
             declare fields in the same order"
        );
        last = at;
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
