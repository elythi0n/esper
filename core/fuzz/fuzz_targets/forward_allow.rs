//! Fuzz the decision that stands between a peer's string and a TCP dial.
//!
//! Two untrusted inputs meet here. The **allowlist** is written by the host's
//! owner, in a file, by hand — so it is a parser, and a parser that panics on
//! a typo is a host that will not start. The **target** comes off the wire from
//! an authenticated peer, which is not the same as a trusted one.
//!
//! The contract: deciding must return, never panic and never hang. Whether a
//! target is allowed is the unit tests' business; that a hostile pair of
//! strings cannot take the host down is this one's.
//!
//! Nothing here dials anything. `Allowlist::allows` is the pure half of
//! `Allowlist::dial` precisely so that it can be asked this question without a
//! network.
//!
//! Run:
//!     cargo +nightly fuzz run forward_allow

#![no_main]

use libfuzzer_sys::fuzz_target;
use esper_proto::ForwardOpen;
use esper_transport::forward::Allowlist;

fuzz_target!(|data: &[u8]| {
    // The bytes as somebody's config file. Split in two so one run exercises a
    // list and a target that are unrelated, which is the case that matters:
    // the interesting failures are in the matching, not in a lucky agreement.
    //
    // Split as **bytes** and converted afterwards. Splitting the string would
    // panic on a multi-byte character straddling the middle — which is a bug
    // in a fuzz harness rather than in what it tests, and this one had it.
    let (first, second) = data.split_at(data.len() / 2);
    let rules = String::from_utf8_lossy(first);
    let rest = String::from_utf8_lossy(second);
    let rest = rest.as_ref();

    let Ok(list) = Allowlist::parse(&rules) else {
        return;
    };
    let _ = list.describe();

    // Every port boundary, against whatever the first half named.
    for port in [1u16, 80, 5432, u16::MAX] {
        let _ = list.allows(rest, port);
    }

    // And the wire shape, since that is what actually reaches it.
    let open = ForwardOpen {
        host: rest.to_owned(),
        port: u16::try_from(data.len()).unwrap_or(u16::MAX),
    };
    if open.is_well_formed() {
        let _ = list.allows(&open.host, open.port);
        let _ = open.target();
    }
});
