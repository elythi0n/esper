//! Fuzz every control message, not only the framing.
//!
//! The framing target (`frame_decode`) proves the length-prefix and CBOR layer
//! survives arbitrary bytes. It says nothing about the *messages*: a payload
//! that is valid CBOR but a hostile shape — an absurd vector length, a string
//! with a length that disagrees with its content, a deeply nested map where a
//! flat one belongs — reaches the message layer intact.
//!
//! This target decodes the same bytes as **every** message type in turn, so a
//! shape that upsets any one of them is found. The contract is the same as the
//! framing target and just as narrow: **decoding must return, never panic and
//! never hang.** Whether the result is Ok or Err is not the point.
//!
//! Run:
//!     cargo +nightly fuzz run messages

#![no_main]

use libfuzzer_sys::fuzz_target;
use ownview_proto::{
    Hello, PairConfirm, PairResponse, PairResult, PairSpake, Permission, PermissionChange,
    SessionDeny, SessionGrant, SessionRequest, SessionResponse, decode,
};

fuzz_target!(|data: &[u8]| {
    // Every message that can arrive from a peer. Adding a message to the
    // protocol and not to this list is how a parser goes unfuzzed.
    let _ = decode::<Hello>(data);
    let _ = decode::<PairSpake>(data);
    let _ = decode::<PairConfirm>(data);
    let _ = decode::<PairResult>(data);
    let _ = decode::<PairResponse>(data);
    let _ = decode::<SessionRequest>(data);
    let _ = decode::<SessionGrant>(data);
    let _ = decode::<SessionDeny>(data);
    let _ = decode::<SessionResponse>(data);
    let _ = decode::<PermissionChange>(data);
    let _ = decode::<Permission>(data);

    // The validators are what a host actually calls on a decoded message, and
    // they must be as robust as the decoder. A validator that panics on a
    // hostile shape is the same denial of service as a decoder that does.
    if let Ok(h) = decode::<Hello>(data) {
        let _ = h.name_is_acceptable();
        let _ = h.version_matches();
        let _ = h.common_caps(&h);
    }
    if let Ok(s) = decode::<PairSpake>(data) {
        let _ = s.is_well_formed();
    }
    if let Ok(r) = decode::<SessionRequest>(data) {
        let _ = r.is_well_formed();
    }
    if let Ok(g) = decode::<SessionGrant>(data) {
        let _ = g.allows(Permission::View);
    }
});
