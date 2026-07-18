#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The whole media path over a simulated network.
//!
//! The unit tests use synthetic NAL units, which prove the framing. This uses
//! **openh264's actual output** and hands the reassembled bitstream to
//! openh264's actual decoder, which proves the thing that matters: that what
//! comes out of the far end is decodable.
//!
//! A packetizer that is subtly wrong still round-trips through its own
//! depacketizer — both halves agreeing on a mistake is the failure mode a
//! self-round-trip cannot see. A decoder does not agree with anything.

use esper_capture::pattern::Pattern;
use esper_color::{ColorSpace, I420};
use esper_decode::decoder::{DecoderConfig, VideoDecoder};
use esper_decode::openh264::OpenH264Decoder;
use esper_encode::budget::Budget;
use esper_encode::encoder::{EncoderConfig, VideoEncoder};
use esper_encode::openh264::OpenH264;
use esper_media::{Abandoned, Depacketizer, Packetizer, Reassembled};

const W: u32 = 320;
const H: u32 = 240;
const SSRC: u32 = 0x0BAD_F00D;
const MTU: usize = 1200;

/// A picture of the synthetic pattern, converted for the encoder.
fn frame(index: u64) -> I420 {
    let pattern = Pattern::new(W, H).expect("a valid size");
    let mut img = I420::new(W, H).expect("a valid size");
    let space = ColorSpace::default();
    let (ys, us) = (img.y_stride(), img.uv_stride());
    let (y, u, v) = img.planes_mut();
    for row in 0..H as usize {
        for col in 0..W as usize {
            let [r, g, b] = pattern.pixel(col as u32, row as u32, index);
            let (yy, uu, vv) = esper_color::convert::reference_yuv(r, g, b, space);
            if let Some(p) = y.get_mut(row * ys + col) {
                *p = yy;
            }
            if row % 2 == 0 && col % 2 == 0 {
                if let Some(p) = u.get_mut((row / 2) * us + col / 2) {
                    *p = uu;
                }
                if let Some(p) = v.get_mut((row / 2) * us + col / 2) {
                    *p = vv;
                }
            }
        }
    }
    img
}

fn encoder() -> OpenH264 {
    let config = EncoderConfig::for_budget(W, H, &Budget::of(100.0));
    OpenH264::new(&config).expect("an encoder")
}

/// **Every frame arrives and decodes.** No loss, which is the case that must
/// work before any other question is worth asking.
///
/// Emissions are matched against a queue of what was sent rather than against
/// the frame of the current iteration, because the reassembler lags by one
/// frame at the start of a session: the first frame has no previous marker to
/// say where it begins, so it waits for the second to bound it.
#[test]
fn real_frames_survive_packetization_and_decode() {
    let mut enc = encoder();
    let mut dec = OpenH264Decoder::new(&DecoderConfig::default()).expect("a decoder");
    let mut pack = Packetizer::new(SSRC, MTU).expect("valid");
    let mut depack = Depacketizer::new(SSRC);

    let mut expected: std::collections::VecDeque<(u32, Vec<u8>, bool)> =
        std::collections::VecDeque::new();
    let mut decoded = 0u32;
    let mut packets_total = 0usize;
    let mut matched = 0u32;

    let handle = |r: Reassembled,
                  expected: &mut std::collections::VecDeque<(u32, Vec<u8>, bool)>,
                  dec: &mut OpenH264Decoder,
                  decoded: &mut u32,
                  matched: &mut u32| match r {
        Reassembled::Frame {
            bitstream,
            timestamp,
            keyframe,
        } => {
            let (ts, want, key) = expected
                .pop_front()
                .expect("a frame arrived that was never sent");
            assert_eq!(timestamp, ts, "frames arrived out of order");
            assert_eq!(bitstream, want, "the frame at {ts} came back different");
            assert_eq!(keyframe, key, "the frame at {ts} disagrees about keyframes");
            *matched += 1;
            if dec.decode(&bitstream).expect("decodes").is_some() {
                *decoded += 1;
            }
        }
        Reassembled::Lost { timestamp, why } => {
            panic!("frame at {timestamp} lost with no loss induced: {why:?}")
        }
    };

    for i in 0..30u64 {
        let encoded = enc.encode(&frame(i), i == 0).expect("encodes");
        if encoded.bitstream.is_empty() {
            continue;
        }
        let ts = u32::try_from(i * 3000).expect("fits");
        expected.push_back((ts, encoded.bitstream.clone(), encoded.keyframe));
        let mut datagrams = Vec::new();
        pack.packetize(&encoded.bitstream, ts, &mut datagrams)
            .expect("packetizes");
        packets_total += datagrams.len();

        let mut out = Vec::new();
        for datagram in &datagrams {
            assert!(datagram.len() <= MTU, "a datagram exceeded the MTU");
            depack.push(datagram, &mut out).expect("accepted");
        }
        for r in out {
            handle(r, &mut expected, &mut dec, &mut decoded, &mut matched);
        }
    }
    // Nothing follows the last frame, so it has to be bounded explicitly.
    if let Some(r) = depack.flush() {
        handle(r, &mut expected, &mut dec, &mut decoded, &mut matched);
    }

    println!("{matched} frames, {packets_total} datagrams, {decoded} decoded pictures");
    assert!(matched >= 25, "only {matched} frames arrived");
    assert!(
        expected.is_empty(),
        "{} frames never arrived",
        expected.len()
    );
    assert_eq!(decoded, matched, "every frame should have decoded");
    assert_eq!(depack.stats().abandoned, 0);
    assert_eq!(depack.stats().frames, u64::from(matched));
}

/// **The keyframe flag is derived, and has to agree with the encoder.**
///
/// The encoder knows whether it wrote an IDR. The receiver is told nothing and
/// works it out from the NAL unit types. If those two ever disagree, a
/// controller either discards a picture it could have started from or tries to
/// start from one it cannot.
#[test]
fn the_derived_keyframe_flag_matches_what_the_encoder_did() {
    let mut enc = encoder();
    let mut pack = Packetizer::new(SSRC, MTU).expect("valid");
    let mut depack = Depacketizer::new(SSRC);
    let mut expected: std::collections::VecDeque<bool> = std::collections::VecDeque::new();
    let mut checked = 0;
    let mut both = [0u32; 2];

    let mut out = Vec::new();
    for i in 0..20u64 {
        // Ask for a keyframe every seventh frame, so both answers occur.
        let encoded = enc.encode(&frame(i), i % 7 == 0).expect("encodes");
        if encoded.bitstream.is_empty() {
            continue;
        }
        expected.push_back(encoded.keyframe);
        let mut datagrams = Vec::new();
        pack.packetize(
            &encoded.bitstream,
            u32::try_from(i * 3000).expect("fits"),
            &mut datagrams,
        )
        .expect("packetizes");
        for d in &datagrams {
            depack.push(d, &mut out).expect("accepted");
        }
    }
    out.extend(depack.flush());

    for r in out {
        if let Reassembled::Frame { keyframe, .. } = r {
            let want = expected.pop_front().expect("a frame nobody sent");
            assert_eq!(
                keyframe, want,
                "the encoder said {want} and the wire said {keyframe}"
            );
            if let Some(n) = both.get_mut(usize::from(keyframe)) {
                *n += 1;
            }
            checked += 1;
        }
    }
    assert!(checked >= 15, "only {checked} frames were compared");
    assert!(
        both[0] > 0 && both[1] > 0,
        "both answers must occur or this proves nothing: {both:?}"
    );
}

/// **Under loss, the stream recovers rather than degrading.**
///
/// Every dropped datagram costs the frame it belonged to, and the receiver has
/// to say so — that report is what makes a keyframe request happen, and the
/// keyframe is what brings the picture back. A receiver that quietly handed
/// broken frames to a decoder would produce a session that gets worse and
/// never gets better.
///
/// # Two different failures, kept apart
///
/// A reassembled frame is checked byte for byte against what was sent, because
/// that is this crate's job and getting it wrong must fail loudly.
///
/// A frame that reassembles perfectly can still fail to *decode*, and that is
/// not this crate's bug: a delta frame references pictures before it, and
/// under loss one of those is gone. The decoder is configured with error
/// concealment off and every failure fatal to the stream, deliberately — so it
/// refuses rather than inventing pixels. That refusal is another reason to ask
/// for a keyframe, which is exactly what a controller does with it.
#[test]
fn a_lossy_link_recovers_through_keyframe_requests() {
    let mut enc = encoder();
    let mut dec = OpenH264Decoder::new(&DecoderConfig::default()).expect("a decoder");
    let mut pack = Packetizer::new(SSRC, MTU).expect("valid");
    let mut depack = Depacketizer::new(SSRC);

    // A deterministic 5% drop, which is the figure the phase's exit criteria
    // name.
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut drop_it = || {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        (state >> 33) % 100 < 5
    };

    // What was sent, by timestamp, so a reassembled frame can be checked
    // against it even when the frames between were lost.
    let mut sent: std::collections::HashMap<u32, Vec<u8>> = std::collections::HashMap::new();
    let mut want_keyframe = true;
    let mut decoded = 0u32;
    let mut undecodable = 0u32;
    let mut lost = 0u32;
    let mut dropped = 0u32;
    let mut arrived = 0u32;

    for i in 0..120u64 {
        let encoded = enc.encode(&frame(i), want_keyframe).expect("encodes");
        want_keyframe = false;
        if encoded.bitstream.is_empty() {
            continue;
        }
        let ts = u32::try_from(i * 3000).expect("fits");
        sent.insert(ts, encoded.bitstream.clone());
        let mut datagrams = Vec::new();
        pack.packetize(&encoded.bitstream, ts, &mut datagrams)
            .expect("packetizes");

        let mut out = Vec::new();
        for d in &datagrams {
            if drop_it() {
                dropped += 1;
                continue;
            }
            depack.push(d, &mut out).expect("accepted");
        }
        for r in out {
            match r {
                Reassembled::Frame {
                    bitstream,
                    timestamp,
                    ..
                } => {
                    arrived += 1;
                    // Reassembly is this crate's job, and it must be exact.
                    let want = sent
                        .get(&timestamp)
                        .expect("a frame arrived that was never sent");
                    assert_eq!(
                        &bitstream, want,
                        "the frame at {timestamp} was reassembled incorrectly"
                    );
                    match dec.decode(&bitstream) {
                        Ok(Some(_)) => decoded += 1,
                        Ok(None) => {}
                        // Its references are gone. Ask for a keyframe.
                        Err(_) => {
                            undecodable += 1;
                            want_keyframe = true;
                        }
                    }
                }
                Reassembled::Lost { why, .. } => {
                    lost += 1;
                    assert!(
                        matches!(
                            why,
                            Abandoned::Incomplete
                                | Abandoned::TruncatedHead
                                | Abandoned::BrokenFragments
                        ),
                        "unexpected reason {why:?}"
                    );
                    want_keyframe = true;
                }
            }
        }
    }
    println!(
        "{dropped} datagrams dropped, {lost} frames abandoned, {arrived} reassembled, \
         {decoded} decoded, {undecodable} undecodable, stats {:?}",
        depack.stats()
    );
    assert!(dropped > 0, "the loss simulation dropped nothing");
    assert!(lost > 0, "loss was induced but no frame was reported lost");
    assert!(
        arrived > 60,
        "only {arrived} of ~120 frames reassembled under 5% loss"
    );
    assert!(
        decoded > 40,
        "only {decoded} frames decoded under 5% loss; the stream is not recovering"
    );
    assert_eq!(
        depack.stats().keyframes_needed,
        u64::from(lost),
        "every abandoned frame must be counted as a keyframe the receiver needs"
    );
}

/// Reordering is normal on a datagram link and must not cost frames.
#[test]
fn a_reordering_link_loses_nothing() {
    let mut enc = encoder();
    let mut pack = Packetizer::new(SSRC, MTU).expect("valid");
    let mut depack = Depacketizer::new(SSRC);
    let mut dec = OpenH264Decoder::new(&DecoderConfig::default()).expect("a decoder");
    let mut expected: std::collections::VecDeque<Vec<u8>> = std::collections::VecDeque::new();
    let mut out = Vec::new();

    for i in 0..25u64 {
        let encoded = enc.encode(&frame(i), i == 0).expect("encodes");
        if encoded.bitstream.is_empty() {
            continue;
        }
        expected.push_back(encoded.bitstream.clone());
        let mut datagrams = Vec::new();
        pack.packetize(
            &encoded.bitstream,
            u32::try_from(i * 3000).expect("fits"),
            &mut datagrams,
        )
        .expect("packetizes");
        // Each frame's packets arrive back to front, which is the worst
        // reordering one frame can suffer.
        datagrams.reverse();
        for d in &datagrams {
            depack.push(d, &mut out).expect("accepted");
        }
    }
    out.extend(depack.flush());

    let mut decoded = 0u32;
    for r in out {
        match r {
            Reassembled::Frame { bitstream, .. } => {
                let want = expected.pop_front().expect("a frame nobody sent");
                assert_eq!(bitstream, want, "a frame was corrupted by reordering");
                if dec.decode(&bitstream).expect("decodes").is_some() {
                    decoded += 1;
                }
            }
            Reassembled::Lost { why, .. } => panic!("a frame was lost to reordering: {why:?}"),
        }
    }
    assert!(decoded >= 20, "only {decoded} frames survived reordering");
    assert!(
        expected.is_empty(),
        "{} frames never arrived",
        expected.len()
    );
    assert_eq!(depack.stats().abandoned, 0);
    assert!(depack.stats().reordered > 0, "reordering was not counted");
}
