//! Fuzz the multicast DNS reader.
//!
//! This parses packets that arrive **unauthenticated, from anyone on the
//! segment**, before any handshake and with nothing in the way. It is the
//! widest attack surface in the product: an attacker does not need to be
//! paired, or even to be talking to us on purpose.
//!
//! Four contracts, and only the first is "does not crash":
//!
//! 1. **Arbitrary bytes must return.** No panic, whatever is on the wire.
//!
//! 2. **And they must return promptly.** A name may end in a compression
//!    pointer to another name, and a pointer that points at a pointer is an
//!    infinite loop unless the reader forbids it. There is nothing for a
//!    length limit to catch, because a loop of pointers accumulates no labels.
//!    A hang here would be a denial of service any machine on the network
//!    could cause, so the fuzzer's own timeout is the check.
//!
//! 3. **What comes out has to obey the protocol's limits.** A name that
//!    decoded to more than 255 bytes, or a label over 63, would mean the
//!    bounds were checked somewhere that a crafted packet can bypass.
//!
//! 4. **Re-encoding is stable, where it fits at all.** Whatever is read,
//!    writing it and reading it again gives the same thing — so a peer cannot
//!    produce a message that means one thing to the reader and another after
//!    it has been passed on.
//!
//!    "Where it fits" is not a hedge, it is the shape of the protocol: an
//!    incoming packet may use compression pointers and this writer
//!    deliberately does not, so a legal packet inside the size limit can
//!    expand past it when written back out. The fuzzer found that in two
//!    minutes. Nothing re-emits a decoded message — a responder builds its own
//!    records — so the case is stated and bounded rather than made
//!    impossible.
//!
//! Run with `make fuzz FUZZ_TARGETS=mdns_packets`.

#![no_main]

use libfuzzer_sys::fuzz_target;
use ownview_discovery::name::{MAX_LABEL, MAX_NAME};
use ownview_discovery::wire::{Data, Message};

fuzz_target!(|data: &[u8]| {
    let Ok(message) = Message::decode(data) else {
        return;
    };

    for question in &message.questions {
        check_name(&question.name);
    }
    for record in message.answers.iter().chain(message.additional.iter()) {
        check_name(&record.name);
        match &record.data {
            Data::Ptr(target) => check_name(target),
            Data::Srv { target, .. } => check_name(target),
            Data::Txt(strings) => {
                for s in strings {
                    // A TXT string's length is one byte on the wire, so
                    // nothing longer can have come from a packet.
                    assert!(s.len() <= 255, "a text string of {} bytes", s.len());
                }
            }
            Data::A(_) => {}
        }
    }

    // Contract 4: writing what was read and reading it back is the same
    // message. A difference would mean the reader accepted something its own
    // writer cannot express, which is how one implementation ends up
    // disagreeing with another about what a packet said.
    let again = message.encode();
    if again.len() <= ownview_discovery::wire::MAX_PACKET {
        match Message::decode(&again) {
            Ok(second) => assert_eq!(second, message, "re-encoding changed the message"),
            Err(e) => panic!("our own encoding could not be read back: {e}"),
        }
    }
});

fn check_name(name: &ownview_discovery::name::Name) {
    assert!(
        name.wire_len() <= MAX_NAME,
        "a name of {} bytes came out of a packet",
        name.wire_len()
    );
    for label in name.labels() {
        assert!(
            !label.is_empty() && label.len() <= MAX_LABEL,
            "a label of {} bytes came out of a packet",
            label.len()
        );
    }
}
