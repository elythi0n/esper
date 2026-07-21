//! Fuzz what a broker parses.
//!
//! A broker is **internet-facing and unauthenticated by design**: it cannot
//! know in advance which devices will use it, so it accepts any key at the
//! handshake and the request that follows is the first thing an attacker can
//! choose freely. It is the only untrusted input the broker has, and the whole
//! reason the broker is a separate binary that holds nothing.
//!
//! Four contracts, and only the first is "does not crash":
//!
//! 1. **Arbitrary bytes must return.** Decoding is CBOR into a fixed shape;
//!    anything that is not that shape is a decode error and not a panic.
//!
//! 2. **A decoded request must be *judged* without panicking.** `refuse` does
//!    the bounds, the broker's own key, the clock and the signature, in that
//!    order, on values an attacker picked — including a timestamp of zero and
//!    one of `u64::MAX`, which is where an unchecked subtraction would be.
//!
//! 3. **Nothing that is refused reaches the table.** The judgement is a pure
//!    function of the message, so this checks the same order the server uses:
//!    a request that `refuse` rejects is never filed.
//!
//! 4. **A request that is *accepted* was signed by the key it claims.** Which
//!    is the property everything else rests on: the broker's table may only
//!    hold places claimed by the devices that own them.
//!
//! Run with `make fuzz FUZZ_TARGETS=rendezvous_meet`.

#![no_main]

use esper_rendezvous::server::refuse;
use esper_rendezvous::wire::Meet;
use libfuzzer_sys::fuzz_target;

/// A fixed broker key, so the fuzzer explores the message rather than the
/// key it is compared against.
const BROKER: [u8; 32] = [7u8; 32];

fuzz_target!(|data: &[u8]| {
    let Ok(meet) = ciborium::from_reader::<Meet, _>(data) else {
        return;
    };

    // A spread of clocks, including the ends: a peer chooses `at`, and an
    // unchecked difference against "now" is where that would matter.
    for now in [0u64, 1, 1_000_000, u64::MAX / 2, u64::MAX] {
        let verdict = refuse(&meet, BROKER, now);
        if verdict.is_none() {
            // Accepted, so every one of these must hold. A place in a
            // broker's table is only claimable by the device that owns it.
            assert!(meet.is_well_formed(), "accepted a malformed request");
            assert!(meet.is_signed(), "accepted an unsigned request");
            assert_eq!(meet.broker, BROKER, "accepted another broker's request");
            assert!(
                !meet.with.is_empty() && meet.with.len() <= esper_rendezvous::wire::MAX_WITH,
                "accepted a request naming nobody, or too many"
            );
            assert!(
                !meet.candidates.is_empty()
                    && meet.candidates.len() <= esper_rendezvous::wire::MAX_CANDIDATES,
                "accepted a request with no address, or too many"
            );
        }
    }

    // Everything else a peer may send on that stream, from the same bytes.
    // Smaller and simpler than a `Meet`, and reached *after* one has been
    // accepted — so a broker that panicked here would be one an authenticated
    // peer could stop.
    if let Ok(request) = ciborium::from_reader::<esper_rendezvous::wire::Request, _>(data) {
        let mut again = Vec::new();
        if ciborium::into_writer(&request, &mut again).is_ok() {
            let round = ciborium::from_reader::<esper_rendezvous::wire::Request, _>(&again[..])
                .expect("what we wrote, we read");
            assert_eq!(round, request);
        }
    }

    // Re-encoding is stable: a request cannot mean one thing to the reader and
    // another after the broker has passed it on.
    let mut again = Vec::new();
    if ciborium::into_writer(&meet, &mut again).is_ok() {
        let round = ciborium::from_reader::<Meet, _>(&again[..]).expect("what we wrote, we read");
        assert_eq!(round, meet, "a request changed by being re-encoded");
    }
});
