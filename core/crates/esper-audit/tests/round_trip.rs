//! Every kind of line the engine writes is a line it can read back.
//!
//! # Why this is not the same as encoding each event
//!
//! `log.rs` already serialises every `Event` on its own and checks what it
//! carries. That cannot see this class of fault, because an event alone has no
//! entry around it — and the entry's fields and the event's land in **one JSON
//! object**: `#[serde(flatten)]` is what makes `{"event":"paired","name":…}` a
//! flat line instead of a nested one.
//!
//! So a field the entry has and an event also has is one key, and what happens
//! then is silent. `FileTransferred` had a `direction`, and so does `Entry`:
//! an outbound transfer wrote the key twice and read back as a duplicate, and
//! an inbound one had its single key taken by the entry and read back as
//! missing. **Every file transfer ever recorded was a line nothing could
//! read**, the Activity screen skipped them, and no test failed — because no
//! test had ever put an event inside an entry and asked for it back.
use esper_audit::{Entry, Event};
use esper_proto::Permission;

/// One of every variant, so a new one added without a thought about the entry
/// around it fails here.
fn every_variant() -> Vec<Event> {
    vec![
        Event::Recording { on: true },
        Event::Paired {
            name: "rack".into(),
            how: "a code read at this machine".to_owned(),
            once: false,
        },
        Event::Unpaired {
            name: "rack".into(),
        },
        Event::Revoked { sessions_closed: 2 },
        Event::SessionNarrowed {
            granted: vec![Permission::View],
        },
        Event::SessionAsked {
            wanted: Permission::Control,
            allowed: false,
            how: "asked at the host".into(),
        },
        Event::LockAfterChanged {
            after: Some("always".into()),
        },
        Event::LockAfterChanged { after: None },
        Event::PermissionsChanged {
            granted: vec![Permission::View, Permission::Control],
        },
        Event::PairingKept {
            granted: vec![Permission::View],
        },
        Event::PairingKept { granted: vec![] },
        Event::ScheduleChanged {
            when: Some("weekdays 09:00-17:00".into()),
        },
        Event::ScheduleChanged { when: None },
        Event::SessionOpened {
            granted: vec![Permission::View],
            tier: "lan".into(),
            allowed: Some("unattended".into()),
        },
        Event::SessionOpened {
            granted: vec![],
            tier: "given".into(),
            allowed: None,
        },
        Event::SessionClosed {
            duration_secs: 90,
            reason: "closed here".into(),
        },
        Event::FileTransferred {
            way: "out".into(),
            path: "quarterly.tar.zst".into(),
            size: 4_182_919,
            outcome: "complete".into(),
        },
        Event::Refused {
            reason: "unknown key".into(),
        },
    ]
}

#[test]
fn every_line_the_engine_writes_is_one_it_can_read_back() {
    let key = [7u8; 32];
    let mut checked = 0;

    for event in every_variant() {
        // Both directions, because the entry's own field is written only for
        // one of them — and each way failed differently.
        for entry in [
            Entry::new(&key, event.clone(), 1_700_000_000),
            Entry::going_out(&key, event.clone()),
        ] {
            let line = serde_json::to_string(&entry).expect("a line");
            let back: Entry = serde_json::from_str(&line).unwrap_or_else(|e| {
                panic!("the engine cannot read back a line it just wrote: {e}\n  {line}")
            });
            assert_eq!(
                back.event, entry.event,
                "an event changed on the way through JSON:\n  {line}"
            );
            assert_eq!(
                back.direction, entry.direction,
                "which way the session went did not survive:\n  {line}"
            );
            assert_eq!(back.peer_key, entry.peer_key);
            checked += 1;
        }
    }

    assert!(
        checked >= 20,
        "only {checked} lines were checked, which is not every variant"
    );
}

/// And no event may take a name the entry already uses.
///
/// The round trip above catches a collision the moment one exists. This says
/// *why* it failed, in the one place somebody adding a variant will look — and
/// it catches a collision on a field that happens to round-trip anyway.
#[test]
fn no_event_field_shares_a_name_with_the_entry_around_it() {
    let entry_fields = ["at", "peer", "peer_key", "direction", "event"];
    let key = [7u8; 32];

    for event in every_variant() {
        let alone = serde_json::to_value(&event).expect("an event");
        let object = alone.as_object().expect("an event is an object");
        for name in object.keys() {
            // `event` is the tag itself, which is how the variant is named.
            if name == "event" {
                continue;
            }
            assert!(
                !entry_fields.contains(&name.as_str()),
                "an event field is called {name:?}, and the entry around it has one of that name — \
                 flattened, they become a single key and the line stops reading back. \
                 Event: {}",
                serde_json::to_string(&event).unwrap_or_default()
            );
        }
        // And the key must actually be the placeholder's, so a refactor that
        // stopped deriving the peer from the key fails here too.
        let _ = Entry::new(&key, event, 1);
    }
}

/// The key names other things grep for.
///
/// # Why a test names keys rather than trusting a round trip
///
/// A round trip survives any rename: the writer and the reader move together
/// and nothing notices. What does not move with them is everything *outside*
/// this crate that reads the log as text — the smoke suite counts refused
/// transfers with `grep`, and an auditor at a terminal does the same.
///
/// A gate that greps for a key nothing writes counts zero every run and says
/// so every run, which is a gate reporting on a field that is not there. This
/// is the thing that fails instead — the event's own field is `way`, because
/// `Entry` already has a `direction` and the two are flattened into one
/// object.
///
/// So: the line a refused transfer writes, as text, with the keys it must
/// carry and the one it must not.
#[test]
fn a_refused_transfer_is_greppable_by_the_keys_it_has_always_had() {
    let line = serde_json::to_string(&Entry::now(
        &[7u8; 32],
        Event::FileTransferred {
            way: "refused".into(),
            path: "somewhere/else".into(),
            size: 0,
            outcome: "not permitted".into(),
        },
    ))
    .expect("an entry");

    for wanted in [
        r#""event":"file_transferred""#,
        r#""way":"refused""#,
        r#""outcome":"not permitted""#,
    ] {
        assert!(
            line.contains(wanted),
            "a refused transfer does not carry {wanted} — anything grepping the log for it counts \
             zero and cannot tell that from nothing having happened.\n{line}"
        );
    }

    // And not the entry's own key, which is what the rename was about: an
    // inbound entry leaves `direction` out entirely, so a check looking for
    // `"direction":"refused"` is a check that can never match.
    assert!(
        !line.contains(r#""direction""#),
        "an inbound entry wrote a direction, and the event's way is the field that says refused.\n{line}"
    );
}
