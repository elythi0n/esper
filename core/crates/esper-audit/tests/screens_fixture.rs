//! The trail the window's gates are shown, held to the type that writes one.
//!
//! # Why this exists
//!
//! `testdata/fixtures/session_log_screens.jsonl` is not a format fixture — the
//! wire fixtures next to it are that. It is a *scenario*: the lines the
//! Activity screen has to draw and an empty log cannot, so the browser gates
//! measure a screen with something on it.
//!
//! It was written by hand first, and the engine silently dropped **fourteen of
//! twenty-one lines**: a line whose event does not deserialise is skipped, so
//! the screen showed a third of the fixture and nothing anywhere said so. The
//! shapes were plausible and wrong — `permissions` where the type says
//! `granted`, `lock_after` where it says `after`, a `session_opened` with none
//! of the three fields it carries.
//!
//! So the fixture is generated from the real types, and this is what stops it
//! drifting back: every line has to parse, and the count has to be the count.

use std::path::PathBuf;

use esper_audit::Entry;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../testdata/fixtures/session_log_screens.jsonl")
}

#[test]
fn every_line_of_the_screens_trail_is_one_this_build_can_read() {
    let text = std::fs::read_to_string(fixture()).expect("the screens trail fixture");
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();

    // A floor: an empty file parses perfectly and proves nothing.
    assert!(
        lines.len() >= 20,
        "the fixture is {} lines, which is not a trail worth showing a screen",
        lines.len()
    );

    let mut refused = Vec::new();
    let mut entries = Vec::new();
    for (n, line) in lines.iter().enumerate() {
        match serde_json::from_str::<Entry>(line) {
            Ok(entry) => entries.push(entry),
            // The line number and the engine's own reason, because "a line did
            // not parse" sends somebody back to the whole file.
            Err(e) => refused.push(format!("line {}: {e}", n + 1)),
        }
    }
    assert!(
        refused.is_empty(),
        "the engine would skip {} of {} lines and the screen would never say so:\n  {}",
        refused.len(),
        lines.len(),
        refused.join("\n  ")
    );

    // In order, because the screen shows them in the order the file has them
    // and a fixture out of order would hide a sort that is wrong.
    let mut sorted = entries.clone();
    sorted.sort_by_key(|e| e.at);
    assert_eq!(
        entries.iter().map(|e| e.at).collect::<Vec<_>>(),
        sorted.iter().map(|e| e.at).collect::<Vec<_>>(),
        "the fixture is not in the order a log is written in"
    );

    // And the shapes the screen exists to draw. Each of these was absent from
    // the hand-written version and each is a different cell on that screen.
    let outward = entries
        .iter()
        .filter(|e| e.direction == esper_audit::Direction::Out)
        .count();
    assert!(
        outward >= 2,
        "no line in the fixture is a session this machine opened outward, so the arrow has one direction"
    );
    let refusals = entries
        .iter()
        .filter(|e| matches!(e.event, esper_audit::Event::Refused { .. }))
        .count();
    assert!(
        refusals >= 1,
        "no line is a refusal, which is what somebody opens this screen to find"
    );
    let devices: std::collections::BTreeSet<&str> =
        entries.iter().map(|e| e.peer_key.as_str()).collect();
    assert!(
        devices.len() >= 5,
        "the fixture names {} device(s), and a log about one machine is a log with no filtering to do",
        devices.len()
    );
}
