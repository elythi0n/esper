#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The audit log, and the two things it must never do.

use esper_audit::{Entry, Event, Log, terminal_closed};
use esper_proto::{Permission, TerminalExit};

fn tmpdir() -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    let mut n = [0u8; 8];
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut n);
    p.push(format!("esper-audit-{}", u64::from_le_bytes(n)));
    std::fs::create_dir_all(&p).expect("mkdir");
    p
}

fn key(n: u8) -> [u8; 32] {
    ed25519_dalek::SigningKey::from_bytes(&[n; 32])
        .verifying_key()
        .to_bytes()
}

#[test]
#[cfg_attr(miri, ignore = "touches the filesystem; MIRI runs with isolation")]
fn a_missing_log_reads_as_empty_rather_than_failing() {
    let dir = tmpdir();
    let log = Log::at(dir.join("absent.log"));
    assert!(
        log.read()
            .expect("a missing log is not an error")
            .is_empty()
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
#[cfg_attr(miri, ignore = "touches the filesystem; MIRI runs with isolation")]
fn entries_round_trip_and_keep_their_order() {
    let dir = tmpdir();
    let log = Log::at(dir.join("sessions.log"));

    for (i, ev) in [
        Event::Paired {
            name: "laptop".to_owned(),
        },
        Event::SessionOpened {
            granted: vec![Permission::View, Permission::Control],
            tier: "lan".to_owned(),
        },
        Event::TerminalOpened,
        terminal_closed(
            42,
            Some(&TerminalExit {
                code: Some(0),
                signal: None,
            }),
        ),
        Event::SessionClosed {
            duration_secs: 90,
            reason: "user closed".to_owned(),
        },
    ]
    .into_iter()
    .enumerate()
    {
        log.append(&Entry::new(&key(1), ev, 1_000 + i as u64))
            .expect("append");
    }

    let back = log.read().expect("read");
    assert_eq!(back.len(), 5, "all five entries should be present");
    // Append-only means chronological, and an investigation reads it in order.
    for w in back.windows(2) {
        let (a, b) = (w.first(), w.get(1));
        assert!(
            a.zip(b).is_none_or(|(a, b)| a.at <= b.at),
            "entries are out of order"
        );
    }
    assert!(matches!(
        back.first().map(|e| &e.event),
        Some(Event::Paired { .. })
    ));
    std::fs::remove_dir_all(&dir).ok();
}

/// A fingerprint is 64 bits, so it is for a human to compare, not for matching.
/// The hex key is what ties an entry to a store record exactly.
#[test]
#[cfg_attr(miri, ignore = "touches the filesystem; MIRI runs with isolation")]
fn an_entry_carries_both_a_readable_fingerprint_and_an_exact_key() {
    let dir = tmpdir();
    let log = Log::at(dir.join("s.log"));
    log.append(&Entry::new(&key(7), Event::TerminalOpened, 5))
        .expect("append");

    let entries = log.read().expect("read");
    let e = entries.first().expect("one entry");
    assert_eq!(e.peer_key.len(), 64, "the key should be full hex");
    assert!(
        e.peer.split(' ').count() >= 4,
        "the fingerprint should be readable words, got {:?}",
        e.peer
    );
    assert_ne!(e.peer, e.peer_key, "they are different representations");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
#[cfg_attr(miri, ignore = "touches the filesystem; MIRI runs with isolation")]
fn entries_can_be_filtered_to_one_peer() {
    let dir = tmpdir();
    let log = Log::at(dir.join("s.log"));
    log.append(&Entry::new(&key(1), Event::TerminalOpened, 1))
        .unwrap();
    log.append(&Entry::new(&key(2), Event::TerminalOpened, 2))
        .unwrap();
    log.append(&Entry::new(&key(1), Event::TerminalOpened, 3))
        .unwrap();

    assert_eq!(log.for_peer(&key(1)).expect("filter").len(), 2);
    assert_eq!(log.for_peer(&key(2)).expect("filter").len(), 1);
    assert_eq!(log.for_peer(&key(9)).expect("filter").len(), 0);
    std::fs::remove_dir_all(&dir).ok();
}

/// The last line of an append-only log can be a partial write after a crash.
/// Refusing to show a user their history because of one truncated line would
/// be the wrong trade.
#[test]
#[cfg_attr(miri, ignore = "touches the filesystem; MIRI runs with isolation")]
fn a_truncated_final_line_does_not_hide_the_history() {
    let dir = tmpdir();
    let path = dir.join("s.log");
    let log = Log::at(&path);
    for i in 0..3u64 {
        log.append(&Entry::new(&key(1), Event::TerminalOpened, i))
            .unwrap();
    }
    // Simulate a crash mid-write.
    let mut raw = std::fs::read_to_string(&path).unwrap();
    raw.push_str("{\"at\":9,\"peer\":\"trunc");
    std::fs::write(&path, raw).unwrap();

    let back = log.read().expect("read");
    assert_eq!(
        back.len(),
        3,
        "the three complete entries must still be readable"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// A log that grows without bound on a server nobody looks at fills the disk,
/// and a retention policy that does not delete is a lie.
#[test]
#[cfg_attr(miri, ignore = "touches the filesystem; MIRI runs with isolation")]
fn pruning_removes_only_what_is_older_than_the_window() {
    let dir = tmpdir();
    let log = Log::at(dir.join("s.log"));
    let now = 1_000_000u64;
    for age in [0u64, 100, 5_000, 100_000] {
        log.append(&Entry::new(&key(1), Event::TerminalOpened, now - age))
            .unwrap();
    }

    let removed = log.prune(10_000, now).expect("prune");
    assert_eq!(
        removed, 1,
        "only the 100_000s-old entry is past a 10_000s window"
    );
    let left = log.read().expect("read");
    assert_eq!(left.len(), 3);
    assert!(left.iter().all(|e| e.at >= now - 10_000));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
#[cfg_attr(miri, ignore = "touches the filesystem; MIRI runs with isolation")]
fn pruning_nothing_leaves_the_file_untouched() {
    let dir = tmpdir();
    let path = dir.join("s.log");
    let log = Log::at(&path);
    log.append(&Entry::new(&key(1), Event::TerminalOpened, 500))
        .unwrap();
    let before = std::fs::read_to_string(&path).unwrap();

    assert_eq!(log.prune(10_000, 1_000).expect("prune"), 0);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
#[cfg_attr(miri, ignore = "touches the filesystem; MIRI runs with isolation")]
fn pruning_leaves_no_temporary_file() {
    let dir = tmpdir();
    let log = Log::at(dir.join("s.log"));
    log.append(&Entry::new(&key(1), Event::TerminalOpened, 1))
        .unwrap();
    log.append(&Entry::new(&key(1), Event::TerminalOpened, 999_999))
        .unwrap();
    log.prune(10, 999_999).expect("prune");

    let strays: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n != "s.log")
        .collect();
    assert!(strays.is_empty(), "prune left {strays:?} behind");
    std::fs::remove_dir_all(&dir).ok();
}

/// The log is what this machine trusts about its own past. A file another user
/// can write is a file another user can rewrite the history in.
#[cfg(unix)]
#[test]
#[cfg_attr(miri, ignore = "touches the filesystem; MIRI runs with isolation")]
fn the_log_is_owner_only_from_the_moment_it_exists() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tmpdir();
    let path = dir.join("s.log");
    Log::at(&path)
        .append(&Entry::new(&key(1), Event::TerminalOpened, 1))
        .expect("append");
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "the log has mode {mode:o}");
    std::fs::remove_dir_all(&dir).ok();
}

// ── the two things it must never do ─────────────────────────────────────────

/// **No session content, ever.** What a person types into a shell includes
/// every password they enter through it.
///
/// # How this is made to actually catch a future addition
///
/// The first version of this test iterated a hardcoded list of variants and
/// checked their serialised form for content-shaped field names. It passed
/// with a `TerminalTraffic { bytes: String }` variant added — because the new
/// variant was not in the list. A test whose whole purpose is catching a future
/// addition, that a future addition slips past, is worse than none: it is
/// reassurance.
///
/// So there are now two halves:
///
/// 1. [`exhaustive_witness`] matches on `Event` **exhaustively**. Adding a
///    variant stops this file compiling until someone touches it.
/// 2. The list below is what gets serialised and screened. The compile error
///    from (1) is what makes them add their variant to (2).
///
/// This forces *attention* rather than guaranteeing *coverage*, and that is
/// the honest limit of what Rust can enforce here without macros. Someone
/// determined can still add a variant, fix the match, and not extend the list
/// — but they cannot do it without reading this comment.
fn exhaustive_witness(e: &Event) {
    // An exhaustive match, deliberately without a `_` arm.
    //
    // If you are here because you added a variant to `Event`: add it to
    // `variants()` below too, and make sure it carries no session content.
    match e {
        Event::Paired { .. }
        | Event::Revoked { .. }
        | Event::SessionOpened { .. }
        | Event::SessionClosed { .. }
        | Event::TerminalOpened
        | Event::TerminalClosed { .. }
        | Event::TerminalParked { .. }
        | Event::TerminalResumed { .. }
        | Event::Refused { .. } => {}
    }
}

/// Every variant, for screening. Kept in step with `exhaustive_witness`.
fn variants() -> Vec<Event> {
    vec![
        Event::Paired {
            name: "laptop".to_owned(),
        },
        Event::Revoked { sessions_closed: 2 },
        Event::SessionOpened {
            granted: vec![Permission::View],
            tier: "relayed".to_owned(),
        },
        Event::SessionClosed {
            duration_secs: 1,
            reason: "done".to_owned(),
        },
        Event::TerminalOpened,
        terminal_closed(
            1,
            Some(&TerminalExit {
                code: Some(1),
                signal: None,
            }),
        ),
        Event::TerminalParked {
            session: "0f1e2d3c4b5a69788796a5b4c3d2e1f0".to_owned(),
            deliberate: true,
        },
        Event::TerminalResumed {
            session: "0f1e2d3c4b5a69788796a5b4c3d2e1f0".to_owned(),
            parked_secs: 42,
            lost: 0,
        },
        Event::Refused {
            reason: "unknown key".to_owned(),
        },
    ]
}

#[test]
#[cfg_attr(miri, ignore = "touches the filesystem; MIRI runs with isolation")]
fn no_event_variant_can_carry_session_content() {
    for ev in variants() {
        exhaustive_witness(&ev);
        let json = serde_json::to_string(&ev).expect("encode");
        for forbidden in [
            "bytes",
            "content",
            "output",
            "input",
            "keystroke",
            "data",
            "stdin",
            "stdout",
            "screen",
            "frame",
            "buffer",
            "typed",
            "traffic",
        ] {
            // A blunt substring screen, deliberately. It has one known false
            // positive shape: a *count* of bytes is not content, but a field
            // named for bytes reads like one. The answer is the name — the
            // resume event's byte count is called `lost` — not a cleverer
            // screen, because a screen with exceptions is a screen someone
            // will add an exception to.
            assert!(
                !json.contains(forbidden),
                "the event {json} has a {forbidden:?} field — the audit log records \
                 THAT a session happened, never what happened in it. A log of \
                 terminal traffic is a keylogger, and being local does not change \
                 that: a local file is what an attacker who gets in reads first."
            );
        }
    }
}

/// The list and the match must stay in step, so the screening actually covers
/// every variant rather than an arbitrary subset of them.
#[test]
#[cfg_attr(miri, ignore = "touches the filesystem; MIRI runs with isolation")]
fn the_variant_list_covers_every_event_kind() {
    let names: std::collections::BTreeSet<String> = variants()
        .iter()
        .map(|e| {
            serde_json::to_value(e)
                .ok()
                .and_then(|v| v.get("event").and_then(|t| t.as_str()).map(str::to_owned))
                .unwrap_or_default()
        })
        .collect();
    assert_eq!(
        names.len(),
        variants().len(),
        "two entries in `variants()` are the same kind, so one kind is unscreened: {names:?}"
    );
}

/// The exit *status* is recorded, because a script needs it. The exit status is
/// not content.
#[test]
#[cfg_attr(miri, ignore = "touches the filesystem; MIRI runs with isolation")]
fn a_terminal_entry_records_its_status_and_duration_only() {
    let ev = terminal_closed(
        3600,
        Some(&TerminalExit {
            code: None,
            signal: Some(9),
        }),
    );
    match ev {
        Event::TerminalClosed {
            duration_secs,
            exit_code,
            exit_signal,
        } => {
            assert_eq!(duration_secs, 3600);
            assert_eq!(exit_code, None);
            assert_eq!(exit_signal, Some(9));
        }
        other => panic!("expected TerminalClosed, got {other:?}"),
    }
}

/// Refusals matter as much as acceptances: a run of them is what an attack
/// looks like from this side.
#[test]
#[cfg_attr(miri, ignore = "touches the filesystem; MIRI runs with isolation")]
fn refusals_are_recorded_too() {
    let dir = tmpdir();
    let log = Log::at(dir.join("s.log"));
    for i in 0..5u64 {
        log.append(&Entry::new(
            &key(3),
            Event::Refused {
                reason: "unknown key".to_owned(),
            },
            i,
        ))
        .unwrap();
    }
    let refusals = log
        .read()
        .unwrap()
        .into_iter()
        .filter(|e| matches!(e.event, Event::Refused { .. }))
        .count();
    assert_eq!(
        refusals, 5,
        "a burst of refusals must be visible in the record"
    );
    std::fs::remove_dir_all(&dir).ok();
}
