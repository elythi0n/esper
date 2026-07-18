// A property test. Test code throughout.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! No sequence of actions, valid or not, reaches an invalid state.
//!
//! The unit tests check the paths the protocol defines. This checks the ones it
//! does not: arbitrary orderings, repeated steps, mismatched inputs, expiry in
//! the middle of an exchange, and a clock that goes backwards. The invariants
//! below must hold after *every* action, in whatever order the actions arrive.
//!
//! Written as a deterministic randomised walk rather than with a property-test
//! framework. A framework would add a dependency to the crate whose whole point
//! is a minimal one, and its shrinking is worth less here than a seed that
//! reproduces a failure exactly — which every assertion message carries.

use std::time::{Duration, Instant};

use esper_crypto::identity::KEY_LEN;
use esper_crypto::pairing::{Exchange, Offer, OfferError, PairingError, Side};
use esper_crypto::{Identity, MAX_ATTEMPTS, PAIRING_TTL, PairingCode};

/// A tiny reproducible PRNG, so a failing seed replays the exact walk.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }
    fn chance(&mut self, one_in: u64) -> bool {
        self.below(one_in) == 0
    }
}

/// What the walk can do to an offer.
#[derive(Debug, Clone, Copy)]
enum Action {
    BeginCorrect,
    BeginWrong,
    BeginAndAbandon,
    AdvanceTime(u64),
    Inspect,
    BeginWithGarbage,
    BeginWithWeakKey,
}

fn key(n: u8) -> [u8; KEY_LEN] {
    ed25519_dalek::SigningKey::from_bytes(&[n; KEY_LEN])
        .verifying_key()
        .to_bytes()
}

/// Invariants that must hold after any action, in any order.
fn check_invariants(offer: &Offer, now: Instant, seed: u64, step: usize, action: Action) {
    let ctx = format!("seed {seed}, step {step}, action {action:?}");

    assert!(
        offer.attempts_remaining() <= MAX_ATTEMPTS,
        "attempts_remaining exceeded the budget - {ctx}"
    );

    // `is_live` must agree with its own components. A disagreement would let a
    // caller believe an offer is usable when `begin` will refuse it.
    let expected_live = offer.remaining(now).is_some() && offer.attempts_remaining() > 0;
    assert_eq!(
        offer.is_live(now),
        expected_live,
        "is_live disagreed with remaining()/attempts_remaining() - {ctx}"
    );

    // A clock that went backwards must not manufacture extra validity.
    if let Some(r) = offer.remaining(now) {
        assert!(r <= PAIRING_TTL, "remaining {r:?} exceeds the TTL - {ctx}");
    }
}

#[test]
fn no_sequence_of_actions_reaches_an_invalid_state() {
    // Fixed seeds, so a failure is reproducible and CI does not flake.
    for seed in [1u64, 7, 42, 1337, 0xDEAD_BEEF, 0x5EED, 999_983, 0xFFFF_FFFF] {
        let mut rng = Rng(seed);
        let created = Instant::now();
        let right = PairingCode::generate();
        let wrong = loop {
            let c = PairingCode::generate();
            if c != right {
                break c;
            }
        };

        let mut offer = Offer::with_created_at(right.clone(), created);
        let mut now = created;
        let mut prior_attempts = offer.attempts_remaining();

        for step in 0..60usize {
            let action = match rng.below(7) {
                0 => Action::BeginCorrect,
                1 => Action::BeginWrong,
                2 => Action::BeginAndAbandon,
                3 => Action::AdvanceTime(rng.below(40)),
                4 => Action::Inspect,
                5 => Action::BeginWithGarbage,
                _ => Action::BeginWithWeakKey,
            };

            let was_live = offer.is_live(now);

            match action {
                Action::AdvanceTime(secs) => now += Duration::from_secs(secs),
                Action::Inspect => {
                    let _ = offer.code();
                    let _ = offer.remaining(now);
                    let _ = offer.attempts_remaining();
                    let _ = offer.is_live(now);
                }
                Action::BeginCorrect | Action::BeginWrong => {
                    let correct = matches!(action, Action::BeginCorrect);
                    let typed = if correct { &right } else { &wrong };
                    if let Ok((host, host_msg)) = offer.begin(now, key(1)) {
                        let (ctrl, ctrl_msg) = Exchange::start(typed, Side::Controller, key(2));
                        let hc = host.confirm(&ctrl_msg, key(2), b"binding");
                        let cc = ctrl.confirm(&host_msg, key(1), b"binding");
                        if let (Ok(hc), Ok(cc)) = (hc, cc) {
                            let cm = *cc.our_mac();
                            assert_eq!(
                                hc.verify_peer(&cm).is_ok(),
                                correct,
                                "confirmation must succeed exactly when the codes agree - \
                                 seed {seed}, step {step}"
                            );
                        }
                    }
                }
                Action::BeginAndAbandon => {
                    if let Ok((exchange, _)) = offer.begin(now, key(1)) {
                        drop(exchange);
                    }
                }
                Action::BeginWithGarbage => {
                    if let Ok((host, _)) = offer.begin(now, key(1)) {
                        let n = rng.below(300) as usize;
                        let junk: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
                        // Must return, never panic. A completed exchange from
                        // junk would mean the SPAKE2 message was not validated.
                        let _ = host.confirm(&junk, key(2), b"binding");
                    }
                }
                Action::BeginWithWeakKey => {
                    if let Ok((host, _)) = offer.begin(now, key(1)) {
                        let (_, ctrl_msg) = Exchange::start(&right, Side::Controller, key(2));
                        let mut weak = [0u8; KEY_LEN];
                        if rng.chance(2) {
                            weak[0] = 1; // the identity point
                        }
                        assert_eq!(
                            host.confirm(&ctrl_msg, weak, b"binding").err(),
                            Some(PairingError::UnusableKey),
                            "a weak key must always be refused - seed {seed}, step {step}"
                        );
                    }
                }
            }

            let after = offer.attempts_remaining();
            assert!(
                after <= prior_attempts,
                "attempts_remaining went up - seed {seed}, step {step}, action {action:?}"
            );
            if matches!(action, Action::Inspect | Action::AdvanceTime(_)) {
                assert_eq!(
                    after, prior_attempts,
                    "a read or a clock change consumed an attempt - seed {seed}, step {step}"
                );
            } else {
                let expected = if was_live {
                    prior_attempts.saturating_sub(1)
                } else {
                    prior_attempts
                };
                assert_eq!(
                    after, expected,
                    "begin consumed the wrong number of attempts (was_live={was_live}) - \
                     seed {seed}, step {step}"
                );
            }
            prior_attempts = after;

            check_invariants(&offer, now, seed, step, action);
        }
    }
}

/// Once an offer is dead - by time or by attempts - nothing brings it back.
#[test]
fn a_dead_offer_can_never_be_revived() {
    for seed in [3u64, 11, 77, 0xC0FFEE] {
        let mut rng = Rng(seed);
        let created = Instant::now();
        let code = PairingCode::generate();

        let mut offer = Offer::with_created_at(code.clone(), created);
        let mut now = created;
        if rng.chance(2) {
            now += PAIRING_TTL + Duration::from_secs(1);
        } else {
            for _ in 0..MAX_ATTEMPTS {
                let _ = offer.begin(now, key(1));
            }
        }
        assert!(
            !offer.is_live(now),
            "the offer should be dead - seed {seed}"
        );

        for _ in 0..40 {
            match rng.below(4) {
                0 => now += Duration::from_secs(rng.below(10)),
                // A clock that goes backwards must not manufacture validity.
                1 => {
                    now = now
                        .checked_sub(Duration::from_secs(rng.below(120)))
                        .unwrap_or(created);
                }
                2 => {
                    let _ = offer.code();
                }
                _ => {}
            }
            let err = offer.begin(now, key(1)).err();
            assert!(
                matches!(err, Some(OfferError::Expired | OfferError::Exhausted)),
                "a dead offer accepted an attempt - seed {seed}, err {err:?}"
            );
        }
    }
}

/// A code is usable exactly as many times as the budget allows, and no more.
#[test]
fn a_code_cannot_be_reused_beyond_its_budget() {
    let now = Instant::now();
    let code = PairingCode::generate();
    let mut offer = Offer::with_created_at(code.clone(), now);
    let id = Identity::generate().expect("id");

    let mut successes = 0usize;
    for _ in 0..(MAX_ATTEMPTS + 2) {
        match offer.begin(now, id.public_key()) {
            Ok((host, host_msg)) => {
                let (ctrl, ctrl_msg) = Exchange::start(&code, Side::Controller, key(9));
                let hc = host.confirm(&ctrl_msg, key(9), b"b").expect("host spake");
                let cc = ctrl
                    .confirm(&host_msg, id.public_key(), b"b")
                    .expect("ctrl spake");
                let cm = *cc.our_mac();
                if hc.verify_peer(&cm).is_ok() {
                    successes += 1;
                }
            }
            Err(_) => break,
        }
    }
    assert_eq!(
        successes, MAX_ATTEMPTS as usize,
        "a correct code must succeed exactly as many times as the budget allows"
    );
    assert_eq!(offer.attempts_remaining(), 0);
}
