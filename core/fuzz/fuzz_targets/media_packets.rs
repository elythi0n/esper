//! Fuzz the RTP depacketizer.
//!
//! This is the parser between a peer's datagrams and a large C decoder, which
//! makes it the most exposed thing in the media path. Three contracts, and
//! only the first is "does not crash":
//!
//! 1. **Arbitrary bytes must return.** No panic, no hang, whatever a peer
//!    sends. Overflow checks are on, sequence numbers wrap, offsets are signed
//!    and the reassembly keys are arithmetic on attacker-chosen values.
//!
//! 2. **Reassembly is bounded.** A peer must not be able to make the receiver
//!    hold memory by sending fragments that never complete. Whatever it sends,
//!    the frame in hand stays inside the byte and packet caps — checked after
//!    every packet rather than argued about.
//!
//! 3. **Nothing malformed comes out as a frame.** Anything the depacketizer
//!    hands back has to be a plausible Annex-B bitstream: it starts with a
//!    start code, and its NAL units are non-empty. A receiver that emitted
//!    something else would be handing rubbish to openh264, which is precisely
//!    the arrangement the sandbox exists to survive and which should not be
//!    relied on.
//!
//! Run with `make fuzz FUZZ_TARGETS=media_packets`.

#![no_main]

use libfuzzer_sys::fuzz_target;
use esper_media::{Depacketizer, Reassembled, MAX_FRAME_BYTES_EXPORT, MAX_PACKETS_EXPORT};

/// The stream the depacketizer is told to accept.
const SSRC: u32 = 0x1234_5678;

fuzz_target!(|data: &[u8]| {
    let mut d = Depacketizer::new(SSRC);
    let mut out = Vec::new();

    // The input is a series of length-prefixed datagrams, so one case can
    // exercise a *sequence* of packets rather than one. A single packet can
    // never test reassembly, which is where the interesting state is.
    let mut rest = data;
    while let Some((&len, tail)) = rest.split_first() {
        let take = usize::from(len).min(tail.len());
        let (packet, next) = tail.split_at(take);
        rest = next;

        out.clear();
        let _ = d.push(packet, &mut out);

        for r in &out {
            match r {
                Reassembled::Frame { bitstream, .. } => {
                    assert!(
                        bitstream.starts_with(&[0, 0, 0, 1]),
                        "a frame came back without a start code: {:?}",
                        bitstream.get(..8)
                    );
                    assert!(
                        bitstream.len() > 4,
                        "a frame came back with a start code and nothing else"
                    );
                    assert!(
                        bitstream.len() <= MAX_FRAME_BYTES_EXPORT + 64 * MAX_PACKETS_EXPORT,
                        "a frame of {} bytes escaped the cap",
                        bitstream.len()
                    );
                    // Every unit must be non-empty: an empty one is a start
                    // code followed by another, which no decoder should be
                    // asked to make sense of.
                    for unit in esper_media::NalUnits::new(bitstream) {
                        assert!(!unit.bytes.is_empty(), "an empty NAL unit was emitted");
                    }
                }
                Reassembled::Lost { .. } => {}
            }
        }

        // The bound a peer cannot spend past, checked after every packet.
        assert!(
            d.held_bytes() <= MAX_FRAME_BYTES_EXPORT,
            "{} bytes are being held, past the {MAX_FRAME_BYTES_EXPORT} cap",
            d.held_bytes()
        );
        assert!(
            d.held_packets() <= MAX_PACKETS_EXPORT,
            "{} packets are being held, past the {MAX_PACKETS_EXPORT} cap",
            d.held_packets()
        );
    }

    // Flushing at the end must also be safe, and must not emit rubbish.
    if let Some(Reassembled::Frame { bitstream, .. }) = d.flush() {
        assert!(bitstream.starts_with(&[0, 0, 0, 1]));
    }
});
