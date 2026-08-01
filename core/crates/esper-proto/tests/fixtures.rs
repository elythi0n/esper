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

/// The `Status` report, on the control plane's channel to the engine.
///
/// A fixture for a message that never leaves the machine, for the same reason
/// as the ones that do: two implementations of one shape drift silently, and
/// this one is read by Go and written by Rust.
fn expected_status() -> esper_proto::Report {
    esper_proto::Report::Status(esper_proto::Status {
        engine: "0.0.0".to_owned(),
        capture: esper_proto::Capability::working("x11"),
        encoder: esper_proto::Capability::missing("openh264 starts and cannot encode"),
        input: esper_proto::Capability::working("x11 xtest: in use"),
        overlays: vec!["wg0".to_owned(), "tailscale0".to_owned()],
        mdns: true,
        broker: Some("broker.example.com:7443".to_owned()),
        paired: 3,
    })
}

#[test]
#[cfg_attr(miri, ignore = "reads fixture files; MIRI has no filesystem")]
fn status_report_decodes_to_the_expected_value() {
    let bytes = fixture("status_report.cbor");
    let got: esper_proto::Report =
        read_frame(&mut bytes.as_slice()).expect("the fixture must decode");
    assert_eq!(got, expected_status());
}

#[test]
#[cfg_attr(miri, ignore = "reads fixture files; MIRI has no filesystem")]
fn status_report_re_encodes_byte_identically() {
    let bytes = fixture("status_report.cbor");
    let decoded: esper_proto::Report =
        read_frame(&mut bytes.as_slice()).expect("the fixture must decode");
    let mut out = Vec::new();
    write_frame(&mut out, &decoded).expect("re-encode");
    assert_eq!(out, bytes, "the encoders have drifted");
}

/// Generate the fixture. Ignored, and run by hand when a message is added.
///
/// Committed bytes, never bytes made on the fly: a fixture generated by the
/// test that checks it proves only that the code agrees with itself.
#[test]
#[ignore = "writes a fixture; run deliberately with --ignored"]
fn write_status_fixture() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("crate is at <root>/core/crates/<name>")
        .join("testdata/fixtures/status_report.cbor");
    let mut out = Vec::new();
    write_frame(&mut out, &expected_status()).expect("encode");
    std::fs::write(&path, &out).unwrap_or_else(|e| panic!("writing {}: {e}", path.display()));
    println!("wrote {} bytes to {}", out.len(), path.display());
}

/// The `Sessions` report: a list of what happened, which Go renders.
fn expected_sessions() -> esper_proto::Report {
    esper_proto::Report::Sessions(vec![
        esper_proto::Happening {
            at: 1_756_700_000,
            peer: "giraffe tapestry geyser rifle jersey domain razor oboe".to_owned(),
            key: "be41344cf00e55422005b232a03d3a0d192df96c55b0fe9ba39f4a0791376a4c".to_owned(),
            device: "laptop".to_owned(),
            event: "session_opened".to_owned(),
            detail: "session opened over direct, granted view, control".to_owned(),
            direction: "in".to_owned(),
        },
        esper_proto::Happening {
            at: 1_756_700_042,
            peer: "giraffe tapestry geyser rifle jersey domain razor oboe".to_owned(),
            key: "be41344cf00e55422005b232a03d3a0d192df96c55b0fe9ba39f4a0791376a4c".to_owned(),
            // Unpaired since: the name is empty rather than a name that no
            // longer means anything.
            device: String::new(),
            event: "session_closed".to_owned(),
            detail: "session ended after 42 seconds, the session ended".to_owned(),
            // **The other direction, on purpose.** A fixture with one value of
            // a field checks half of it, and which half is not obvious to
            // whoever reads the file next.
            direction: "out".to_owned(),
        },
    ])
}

#[test]
#[cfg_attr(miri, ignore = "reads fixture files; MIRI has no filesystem")]
fn sessions_report_decodes_to_the_expected_value() {
    let bytes = fixture("sessions_report.cbor");
    let got: esper_proto::Report =
        read_frame(&mut bytes.as_slice()).expect("the fixture must decode");
    assert_eq!(got, expected_sessions());
}

#[test]
#[cfg_attr(miri, ignore = "reads fixture files; MIRI has no filesystem")]
fn sessions_report_re_encodes_byte_identically() {
    let bytes = fixture("sessions_report.cbor");
    let decoded: esper_proto::Report =
        read_frame(&mut bytes.as_slice()).expect("the fixture must decode");
    let mut out = Vec::new();
    write_frame(&mut out, &decoded).expect("re-encode");
    assert_eq!(out, bytes, "the encoders have drifted");
}

#[test]
#[ignore = "writes a fixture; run deliberately with --ignored"]
fn write_sessions_fixture() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("crate is at <root>/core/crates/<name>")
        .join("testdata/fixtures/sessions_report.cbor");
    let mut out = Vec::new();
    write_frame(&mut out, &expected_sessions()).expect("encode");
    std::fs::write(&path, &out).unwrap_or_else(|e| panic!("writing {}: {e}", path.display()));
    println!("wrote {} bytes to {}", out.len(), path.display());
}
