//! Every value the portals use, marshalled and read back.
//!
//! # Why this is the first thing
//!
//! The wire format is where a hand-written D-Bus implementation goes wrong,
//! and it goes wrong quietly: a padding byte too few and the *next* value is
//! read from the wrong offset, so the failure appears in a field nobody
//! touched. Round-tripping catches the symmetric mistakes — a writer and
//! reader that are wrong the same way agree with each other — so these tests
//! are paired with two things they cannot do on their own: a comparison
//! against a real bus (`scripts/check-dbus.sh`) and a fuzzer on the decoder.
//!
//! The offsets asserted below are from the specification rather than from a
//! previous run of this code, which is the difference between a test and a
//! photograph.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use esper_dbus::{Value, marshal_body, signature_of, unmarshal_body};

/// Marshal, read back, and insist it is the same thing.
fn survives(value: Value) {
    let signature = value.signature();
    let bytes = marshal_body(std::slice::from_ref(&value));
    let back = unmarshal_body(&signature, &bytes)
        .unwrap_or_else(|e| panic!("reading back a {signature}: {e}"));
    assert_eq!(
        back.len(),
        1,
        "a {signature} came back as {} values",
        back.len()
    );
    assert_eq!(back[0], value, "a {signature} changed on the way through");
}

#[test]
fn every_scalar_survives() {
    survives(Value::Bool(true));
    survives(Value::Bool(false));
    survives(Value::Int32(-1));
    survives(Value::Int32(i32::MIN));
    survives(Value::Uint32(0));
    survives(Value::Uint32(u32::MAX));
    survives(Value::Uint64(u64::MAX));
    survives(Value::Double(-0.5));
    survives(Value::Str(String::new()));
    survives(Value::Str("hello".to_owned()));
    survives(Value::Path("/org/freedesktop/portal/desktop".to_owned()));
    survives(Value::Signature("a{sv}".to_owned()));
}

/// Non-ASCII in a string, because the length on the wire is in **bytes** and
/// writing it in characters is a mistake that passes every ASCII test.
#[test]
fn a_string_is_measured_in_bytes() {
    let text = "räksmörgås — ✨";
    survives(Value::Str(text.to_owned()));

    let bytes = marshal_body(&[Value::Str(text.to_owned())]);
    let length = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    assert_eq!(length, text.len(), "the length is the byte count");
    assert_ne!(
        length,
        text.chars().count(),
        "this string exists so the two counts differ"
    );
}

#[test]
fn the_shapes_a_portal_actually_sends() {
    // The options argument of every portal call.
    survives(Value::no_options());
    survives(Value::dict(vec![
        ("handle_token", Value::Str("esper0".to_owned())),
        ("types", Value::Uint32(7)),
    ]));

    // What a RemoteDesktop Start returns: a{sv} with a nested array of
    // (u, a{sv}) streams — the deepest thing this crate has to read.
    survives(Value::dict(vec![
        ("devices", Value::Uint32(3)),
        (
            "streams",
            Value::Array {
                element: "(ua{sv})".to_owned(),
                items: vec![Value::Struct(vec![
                    Value::Uint32(42),
                    Value::dict(vec![(
                        "size",
                        Value::Struct(vec![Value::Int32(1280), Value::Int32(720)]),
                    )]),
                ])],
            },
        ),
    ]));
}

/// **An empty array still has a type.** It marshals to a length of zero and
/// nothing else, and reading it back has to produce an array of the right
/// element signature rather than an array of nothing.
#[test]
fn an_empty_array_keeps_its_element_type() {
    let empty = Value::Array {
        element: "s".to_owned(),
        items: Vec::new(),
    };
    assert_eq!(empty.signature(), "as");
    survives(empty.clone());

    let bytes = marshal_body(&[empty]);
    assert_eq!(bytes, vec![0, 0, 0, 0], "an empty array is four zero bytes");
}

/// Alignment between arguments, which is where an off-by-one hides.
///
/// A one-byte-payload string followed by a `u32`: the string occupies four
/// length bytes, one character and a nul — six — and the `u32` must start at
/// eight, not at six.
#[test]
fn the_next_argument_is_padded_to_its_own_alignment() {
    let args = vec![Value::Str("x".to_owned()), Value::Uint32(0xAABB_CCDD)];
    let bytes = marshal_body(&args);

    assert_eq!(
        u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        1,
        "the length is one"
    );
    assert_eq!(bytes[4], b'x');
    assert_eq!(bytes[5], 0, "the nul terminator");
    assert_eq!(
        &bytes[6..8],
        &[0, 0],
        "two bytes of padding to reach four-byte alignment"
    );
    assert_eq!(
        u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
        0xAABB_CCDD
    );

    let back = unmarshal_body(&signature_of(&args), &bytes).expect("reads back");
    assert_eq!(back, args);
}

/// A struct inside an array starts on an eight-byte boundary, and the array's
/// own length does **not** count the padding that precedes the first element.
#[test]
fn an_array_length_excludes_the_padding_before_it() {
    let value = Value::Array {
        element: "(u)".to_owned(),
        items: vec![Value::Struct(vec![Value::Uint32(1)])],
    };
    let bytes = marshal_body(std::slice::from_ref(&value));

    let length = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    assert_eq!(length, 4, "one struct of one u32 is four bytes, not twelve");
    assert_eq!(
        &bytes[4..8],
        &[0, 0, 0, 0],
        "padding to the struct's alignment"
    );
    assert_eq!(bytes.len(), 12);
    survives(value);
}

/// A variant carries its own signature, so the same bytes mean different
/// things depending on it — and reading one has to follow what it says rather
/// than what the caller expected.
#[test]
fn a_variant_is_read_by_its_own_signature() {
    survives(Value::Variant(Box::new(Value::Uint32(7))));
    survives(Value::Variant(Box::new(Value::Str("seven".to_owned()))));
    survives(Value::Variant(Box::new(Value::dict(vec![(
        "nested",
        Value::Variant(Box::new(Value::Bool(true))),
    )]))));
}

/// Truncation is refused rather than guessed at, because these bytes come from
/// another process and a short message must not become a panic.
#[test]
fn a_short_message_is_an_error_not_a_panic() {
    let bytes = marshal_body(&[Value::Str("hello".to_owned())]);
    for cut in 0..bytes.len() {
        let result = unmarshal_body("s", &bytes[..cut]);
        assert!(
            result.is_err(),
            "{cut} bytes of a 10-byte string should not parse"
        );
    }
}

/// A length field that claims more than the message holds is the obvious
/// hostile input, and it must not allocate on that number or run off the end.
#[test]
fn a_lying_length_is_refused() {
    // An array claiming four gigabytes of elements, with none.
    let mut bytes = u32::MAX.to_le_bytes().to_vec();
    bytes.extend_from_slice(&[0, 0, 0, 0]);
    assert!(unmarshal_body("au", &bytes).is_err());

    // A string claiming more than it has.
    let mut lying = 99u32.to_le_bytes().to_vec();
    lying.extend_from_slice(b"short\0");
    assert!(unmarshal_body("s", &lying).is_err());
}

/// Signatures split into arguments by nesting, not by character. `oa{sv}` is
/// two arguments; splitting on `}` would make it more.
#[test]
fn a_signature_splits_by_nesting() {
    let args = vec![
        Value::Path("/x".to_owned()),
        Value::dict(vec![("k", Value::Uint32(1))]),
    ];
    assert_eq!(signature_of(&args), "oa{sv}");

    let bytes = marshal_body(&args);
    let back = unmarshal_body("oa{sv}", &bytes).expect("two arguments");
    assert_eq!(back.len(), 2, "oa{{sv}} is two arguments");
    assert_eq!(back, args);
}

/// Reading a dictionary is how every portal result is used, so the helper that
/// does it is held to seeing through the variant it is wrapped in.
#[test]
fn a_result_dictionary_can_be_read_by_key() {
    let results = Value::dict(vec![
        ("session_handle", Value::Str("/org/x/session/1".to_owned())),
        ("devices", Value::Uint32(3)),
    ]);
    assert_eq!(
        results.get("session_handle").and_then(Value::as_str),
        Some("/org/x/session/1")
    );
    assert_eq!(results.get("devices").and_then(Value::as_u32), Some(3));
    assert_eq!(results.get("absent"), None);
}
