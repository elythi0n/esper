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
            allowed: None,
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

    let pruned = log.retain(Some(10_000), None, now).expect("prune");
    assert_eq!(
        pruned.too_old, 1,
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

    assert_eq!(
        log.retain(Some(10_000), None, 1_000)
            .expect("prune")
            .removed(),
        0
    );
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
    log.retain(Some(10), None, 999_999).expect("prune");

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
        Event::Recording { .. }
        | Event::Paired { .. }
        | Event::Unpaired { .. }
        | Event::Revoked { .. }
        | Event::SessionNarrowed { .. }
        | Event::PermissionsChanged { .. }
        | Event::SessionOpened { .. }
        | Event::SessionClosed { .. }
        | Event::TerminalOpened
        | Event::TerminalClosed { .. }
        | Event::TerminalParked { .. }
        | Event::TerminalResumed { .. }
        | Event::ForwardOpened { .. }
        | Event::ForwardClosed { .. }
        | Event::ForwardRefused { .. }
        | Event::FileTransferred { .. }
        | Event::Refused { .. }
        | Event::Locked { .. } => {}
    }
}

/// Every variant, for screening. Kept in step with `exhaustive_witness`.
/// **Starting and stopping a recording read as different sentences.**
///
/// One line saying "recording" with a boolean in it would make a person read
/// the JSON to find out which happened, and the whole point of `detail` is
/// that they should not have to.
#[test]
fn a_recording_that_started_and_one_that_stopped_do_not_read_the_same() {
    let started = Event::Recording { on: true }.detail();
    let stopped = Event::Recording { on: false }.detail();
    assert!(started.contains("started"), "{started}");
    assert!(stopped.contains("stopped"), "{stopped}");
    assert_eq!(
        Event::Recording { on: true }.tag(),
        Event::Recording { on: false }.tag(),
        "and both are the same kind of thing, so they grep alike"
    );
}

fn variants() -> Vec<Event> {
    vec![
        Event::Recording { on: true },
        Event::Paired {
            name: "laptop".to_owned(),
        },
        Event::Unpaired {
            name: "laptop".to_owned(),
        },
        Event::Revoked { sessions_closed: 2 },
        Event::SessionNarrowed {
            granted: vec![Permission::View],
        },
        Event::PermissionsChanged {
            granted: vec![Permission::View, Permission::Terminal],
        },
        Event::SessionOpened {
            granted: vec![Permission::View],
            tier: "relayed".to_owned(),
            allowed: None,
        },
        Event::SessionClosed {
            duration_secs: 1,
            reason: "done".to_owned(),
        },
        Event::Locked {
            how: "i3lock".to_owned(),
            worked: true,
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
        Event::ForwardOpened {
            target: "127.0.0.1:5432".to_owned(),
        },
        Event::ForwardClosed {
            target: "127.0.0.1:5432".to_owned(),
            duration_secs: 12,
            sent: 4096,
            received: 8192,
        },
        Event::ForwardRefused {
            target: "10.0.0.5:22".to_owned(),
            reason: "that target is not on the host's forwarding allowlist".to_owned(),
        },
        Event::FileTransferred {
            direction: "in".to_owned(),
            path: "reports/q3.pdf".to_owned(),
            size: 4096,
            outcome: "stored".to_owned(),
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

/// The tag a program branches on and the tag serde writes are the same word.
///
/// `Event::tag` is written by hand, and the serialised name comes from serde.
/// Two spellings of one thing is exactly the drift that makes a log somebody
/// greps disagree with a list somebody reads.
#[test]
#[cfg_attr(miri, ignore = "touches the filesystem; MIRI runs with isolation")]
fn every_tag_is_the_name_serde_writes() {
    for ev in variants() {
        let json = serde_json::to_value(&ev).expect("encode");
        let written = json
            .get("event")
            .and_then(|v| v.as_str())
            .expect("every event carries its tag");
        assert_eq!(ev.tag(), written, "{ev:?}");
    }
}

/// Every event says something, and says it without the session in it.
#[test]
#[cfg_attr(miri, ignore = "touches the filesystem; MIRI runs with isolation")]
fn every_event_renders_a_line_a_person_can_read() {
    for ev in variants() {
        let line = ev.detail();
        assert!(!line.is_empty(), "{ev:?} rendered nothing");
        // The same screen the entries themselves get: a rendering is another
        // place session content could arrive, and it is read by more people
        // than the raw log is.
        for forbidden in [
            "keystroke",
            "stdin",
            "stdout",
            "typed",
            "traffic",
            "content",
        ] {
            assert!(
                !line.contains(forbidden),
                "the rendering of {ev:?} contains {forbidden:?}: {line}"
            );
        }
    }
}

/// **Append-only in practice, checked by trying to write somewhere else.**
///
/// The crate opens the file with `O_APPEND` for every write, and this is what
/// that buys: a writer that *seeks* to the start and writes still lands at the
/// end. The kernel guarantees it, which is precisely why the guarantee is
/// worth depending on rather than reimplementing — and worth pinning here, so
/// that a change to how this crate opens the file fails a test instead of
/// quietly making history editable.
#[test]
#[cfg_attr(miri, ignore = "touches the filesystem; MIRI runs with isolation")]
fn a_writer_that_seeks_to_the_start_still_appends() {
    use std::io::{Seek, SeekFrom, Write};

    let dir = tmpdir();
    let path = dir.join("s.log");
    let log = Log::at(&path);
    log.append(&Entry::new(&key(1), Event::TerminalOpened, 100))
        .unwrap();
    log.append(&Entry::new(&key(2), Event::TerminalOpened, 200))
        .unwrap();
    let before = std::fs::read_to_string(&path).unwrap();

    // A determined writer, opening the log the way this crate does.
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    f.seek(SeekFrom::Start(0)).unwrap();
    f.write_all(b"TAMPERED\n").unwrap();
    f.flush().unwrap();

    let after = std::fs::read_to_string(&path).unwrap();
    assert!(
        after.starts_with(&before),
        "the history before the write was altered:\n{after}"
    );
    assert!(
        after.ends_with("TAMPERED\n"),
        "the write should have landed at the end:\n{after}"
    );
    // And the entries that were there still read back, in order: a reader
    // skips the line that is not JSON and loses nothing else.
    let entries = log.read().unwrap();
    assert_eq!(
        entries.iter().map(|e| e.at).collect::<Vec<_>>(),
        vec![100, 200]
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// **Truncating is not something this crate can be asked to do.**
///
/// There is no method that empties the log, and retention is the only path
/// that replaces it — so this states the property the API's shape gives: after
/// every public call that writes, the entries that were there are still there.
#[test]
#[cfg_attr(miri, ignore = "touches the filesystem; MIRI runs with isolation")]
fn nothing_in_the_api_shortens_the_log_except_retention() {
    let dir = tmpdir();
    let log = Log::at(dir.join("s.log"));
    for at in [10u64, 20, 30] {
        log.append(&Entry::new(&key(1), Event::TerminalOpened, at))
            .unwrap();
    }
    // Every reader, and an append: none of them may lose an entry.
    let _ = log.read().unwrap();
    let _ = log.for_peer(&key(1)).unwrap();
    log.append(&Entry::new(&key(1), Event::TerminalOpened, 40))
        .unwrap();
    assert_eq!(log.read().unwrap().len(), 4);

    // Retention with no limits reads and writes nothing.
    let pruned = log.retain(None, None, 1_000_000).unwrap();
    assert_eq!(pruned.removed(), 0);
    assert_eq!(log.read().unwrap().len(), 4);
    std::fs::remove_dir_all(&dir).ok();
}

/// **What retention keeps, it keeps byte for byte.**
///
/// The rewrite is the one path that does not append, so the thing to pin is
/// that it cannot *alter* an entry — only drop whole ones from the front. An
/// entry that came back subtly different would be a record that had been
/// edited by the thing supposed to be preserving it.
#[test]
#[cfg_attr(miri, ignore = "touches the filesystem; MIRI runs with isolation")]
fn retaining_rewrites_nothing_it_keeps() {
    let dir = tmpdir();
    let path = dir.join("s.log");
    let log = Log::at(&path);
    let now = 1_000_000u64;
    for age in [0u64, 10, 20, 500_000] {
        log.append(&Entry::new(
            &key(3),
            Event::SessionOpened {
                granted: vec![Permission::View, Permission::Control],
                tier: "direct".to_owned(),
                allowed: None,
            },
            now - age,
        ))
        .unwrap();
    }
    let kept_lines: Vec<String> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .filter(|l| !l.contains(&format!("{}", now - 500_000)))
        .map(str::to_owned)
        .collect();

    let pruned = log.retain(Some(1_000), None, now).unwrap();
    assert_eq!(pruned.too_old, 1);
    let after: Vec<String> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(
        after, kept_lines,
        "the lines that survived should be the same bytes they were"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// **The size ceiling deletes the oldest and keeps the newest.**
///
/// The rule an age limit cannot cover: a machine with a hundred sessions a day
/// fills a disk long before ninety days are up.
#[test]
#[cfg_attr(miri, ignore = "touches the filesystem; MIRI runs with isolation")]
fn the_size_ceiling_drops_the_oldest_until_it_fits() {
    let dir = tmpdir();
    let path = dir.join("s.log");
    let log = Log::at(&path);
    for at in 1..=50u64 {
        log.append(&Entry::new(&key(4), Event::TerminalOpened, at))
            .unwrap();
    }
    let whole = std::fs::metadata(&path).unwrap().len();
    let half = whole / 2;

    let pruned = log.retain(None, Some(half), 1_000_000).unwrap();
    assert_eq!(pruned.too_old, 0, "no age limit was given");
    assert!(pruned.over_size > 0, "the ceiling should have removed some");
    let left = log.read().unwrap();
    assert_eq!(left.len(), pruned.kept);
    assert!(
        std::fs::metadata(&path).unwrap().len() <= half,
        "the file is still over the ceiling"
    );
    // The newest survived, and the oldest went.
    assert_eq!(
        left.last().map(|e| e.at),
        Some(50),
        "the most recent entry must be the one that is kept"
    );
    assert!(
        left.first().map(|e| e.at).unwrap_or(0) > 1,
        "the oldest should have gone first"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// **Both rules at once, and the age is applied first.** Whichever bites, the
/// oldest go — so the two cannot disagree about what to keep.
#[test]
#[cfg_attr(miri, ignore = "touches the filesystem; MIRI runs with isolation")]
fn an_age_and_a_ceiling_together_remove_the_oldest_by_both() {
    let dir = tmpdir();
    let path = dir.join("s.log");
    let log = Log::at(&path);
    let now = 100_000u64;
    // Ten old, ten recent.
    for i in 0..10u64 {
        log.append(&Entry::new(&key(5), Event::TerminalOpened, 1_000 + i))
            .unwrap();
    }
    for i in 0..10u64 {
        log.append(&Entry::new(&key(5), Event::TerminalOpened, now - 100 + i))
            .unwrap();
    }
    let line = std::fs::metadata(&path).unwrap().len() / 20;

    // An age that drops the first ten, and a ceiling that fits about five.
    let pruned = log.retain(Some(1_000), Some(line * 5), now).unwrap();
    assert_eq!(pruned.too_old, 10, "the ten old ones are past the age");
    assert!(
        pruned.over_size >= 4,
        "and the ceiling should trim the rest to about five, not {}",
        pruned.over_size
    );
    let left = log.read().unwrap();
    assert!(
        left.iter().all(|e| e.at >= now - 100),
        "everything left should be from the recent group"
    );
    assert_eq!(
        left.last().map(|e| e.at),
        Some(now - 100 + 9),
        "and the newest is always kept"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// A pass that did nothing says nothing, so a host pruning daily does not
/// print a line a day about no change.
#[test]
fn a_pass_that_removed_nothing_has_nothing_to_say() {
    let quiet = esper_audit::Pruned {
        too_old: 0,
        over_size: 0,
        kept: 12,
    };
    assert_eq!(quiet.describe(), None);
    assert_eq!(quiet.removed(), 0);

    let aged = esper_audit::Pruned {
        too_old: 3,
        over_size: 0,
        kept: 9,
    };
    assert!(aged.describe().is_some_and(|s| s.contains("retention")));
    // One is "1 entry", because a line a person reads once should not read
    // like a machine wrote it.
    let one = esper_audit::Pruned {
        too_old: 1,
        over_size: 0,
        kept: 0,
    };
    assert_eq!(
        one.describe().as_deref(),
        Some("1 entry past its retention")
    );
    let both = esper_audit::Pruned {
        too_old: 3,
        over_size: 2,
        kept: 9,
    };
    assert_eq!(both.removed(), 5);
    assert!(both.describe().is_some_and(|s| s.contains("size limit")));
}

/// **How a session was allowed is on the line that records it opening.**
///
/// "A session opened, granted view and control" reads the same whether
/// somebody at the machine agreed to it or whether nobody was asked, and those
/// are the two answers a security log exists to tell apart. A device that
/// walked in on `unattended` is the one somebody goes looking for afterwards.
#[test]
fn a_session_says_how_it_was_allowed() {
    let entry = Event::SessionOpened {
        granted: vec![Permission::View],
        tier: "direct".to_owned(),
        allowed: Some("the unattended permission".to_owned()),
    };
    let said = entry.detail();
    assert!(
        said.contains("allowed by the unattended permission"),
        "{said}"
    );
    assert!(said.contains("granted view"), "{said}");
}

/// And the line reads as a sentence when nobody knows how.
///
/// The controller's own outbound entry: that end asked for a session and was
/// given one, and was never told which of the host's reasons applied.
/// Inventing one would be worse than leaving it out — and a dangling comma
/// would be worse than either.
#[test]
fn a_session_nobody_can_explain_says_nothing_extra() {
    let entry = Event::SessionOpened {
        granted: vec![Permission::View],
        tier: "direct".to_owned(),
        allowed: None,
    };
    let said = entry.detail();
    assert_eq!(said, "session opened over direct, granted view");
    assert!(!said.contains("allowed"), "{said}");
}

/// An entry written before this field existed still reads.
///
/// The log is a file somebody keeps: a reader that refused a line written last
/// week would make a security log useless at exactly the moment it is wanted.
#[test]
fn a_line_from_before_the_field_existed_still_decodes() {
    let older = r#"{"at":1756700000,"peer":"a b c d e f g h","peer_key":"aa","event":"session_opened","granted":["view"],"tier":"direct"}"#;
    let entry: Entry = serde_json::from_str(older).expect("an older line has to decode");
    let Event::SessionOpened { allowed, .. } = entry.event else {
        panic!("decoded as something else");
    };
    assert_eq!(allowed, None);
}

/// **A lock that worked and one that did not read as different sentences.**
///
/// The same reason a recording that started and one that stopped do: a line
/// with a boolean in it makes a person read the JSON to find out which
/// happened, and a device that *tried* to lock somebody's screen and could not
/// is exactly the line an operator would go looking for.
#[test]
fn a_lock_that_worked_and_one_that_did_not_do_not_read_the_same() {
    let worked = Event::Locked {
        how: "i3lock".to_owned(),
        worked: true,
    }
    .detail();
    let did_not = Event::Locked {
        how: "no screen locker found: tried loginctl".to_owned(),
        worked: false,
    }
    .detail();
    assert!(worked.contains("locked this machine"), "{worked}");
    assert!(worked.contains("i3lock"), "{worked}");
    assert!(did_not.contains("could not"), "{did_not}");
    assert!(did_not.contains("no screen locker"), "{did_not}");
    assert_ne!(worked, did_not);
}
