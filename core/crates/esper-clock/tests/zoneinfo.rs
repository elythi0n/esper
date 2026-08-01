#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The parser against the machine's own timezone database.
//!
//! # Why these are not unit tests
//!
//! They read `/usr/share/zoneinfo`, which is a real file somebody else
//! produced — and that is the point. A parser tested only against bytes it
//! also constructed is tested against its own misunderstanding of the format.
//!
//! They skip themselves where the database is absent, which is the ordinary
//! case in a minimal container. `scripts/check-clock.py` is the wider check:
//! it compares this crate's answers against Python's `zoneinfo` across many
//! zones and many instants, including instants past the last transition a
//! tzdata build records.

use std::path::{Path, PathBuf};

use esper_clock::{Civil, Weekday, Zone};

/// A zone from the database, or `None` with a note.
fn zone(name: &str) -> Option<Zone> {
    let path = PathBuf::from("/usr/share/zoneinfo").join(name);
    if !path.exists() {
        println!("skipped: this machine has no {}", path.display());
        return None;
    }
    match Zone::at_path(&path) {
        Ok(z) => Some(z),
        Err(e) => panic!("{name} is in the database and would not parse: {e}"),
    }
}

/// **The two instants a year that a wrong answer is invisible the rest of the
/// time.**
///
/// Helsinki changes at 01:00 UTC on the last Sunday of March and of October.
/// An hour either side of each, so an off-by-one-hour or an off-by-one-week is
/// a failure rather than a shrug.
#[test]
fn helsinkis_daylight_saving_changes_at_the_instant_it_should() {
    let Some(zone) = zone("Europe/Helsinki") else {
        return;
    };
    // 2026-03-29T01:00:00Z, the change forward.
    let spring = Civil {
        year: 2026,
        month: 3,
        day: 29,
        hour: 1,
        minute: 0,
        second: 0,
        weekday: Weekday::Sunday,
    }
    .as_unix(0);
    assert_eq!(zone.offset_at(spring - 1), 2 * 3600, "a second before");
    assert_eq!(zone.offset_at(spring), 3 * 3600, "at the change");
    assert_eq!(zone.offset_at(spring + 3600), 3 * 3600, "an hour after");

    // 2026-10-25T01:00:00Z, the change back.
    let autumn = Civil {
        year: 2026,
        month: 10,
        day: 25,
        hour: 1,
        minute: 0,
        second: 0,
        weekday: Weekday::Sunday,
    }
    .as_unix(0);
    assert_eq!(zone.offset_at(autumn - 1), 3 * 3600, "a second before");
    assert_eq!(zone.offset_at(autumn), 2 * 3600, "at the change");

    // And the civil time reads as a clock in Helsinki would: 04:00 local at
    // the spring change, because the hour from 03:00 to 04:00 does not exist.
    let c = zone.civil_at(spring);
    assert_eq!((c.hour, c.minute), (4, 0), "{c:?}");
}

/// **Past the last transition in the file**, which is what the POSIX footer is
/// for. A reader that stopped at the table would answer with one offset all
/// year, and be wrong for half of it.
#[test]
fn the_footer_carries_the_zone_past_the_end_of_its_table() {
    let Some(zone) = zone("Europe/Helsinki") else {
        return;
    };
    // 2045, long past any transition a tzdata build records.
    let midsummer = Civil {
        year: 2045,
        month: 7,
        day: 1,
        hour: 12,
        minute: 0,
        second: 0,
        weekday: Weekday::Saturday,
    }
    .as_unix(0);
    let midwinter = Civil {
        year: 2045,
        month: 1,
        day: 15,
        hour: 12,
        minute: 0,
        second: 0,
        weekday: Weekday::Sunday,
    }
    .as_unix(0);
    assert_eq!(zone.offset_at(midsummer), 3 * 3600, "July 2045 is summer");
    assert_eq!(
        zone.offset_at(midwinter),
        2 * 3600,
        "January 2045 is winter"
    );
}

/// A zone with no daylight saving answers the same thing all year, and one
/// south of the equator has its summer in January.
#[test]
fn zones_that_do_not_change_and_zones_that_change_the_other_way() {
    if let Some(zone) = zone("Asia/Tokyo") {
        let january = Civil {
            year: 2027,
            month: 1,
            day: 15,
            hour: 3,
            minute: 0,
            second: 0,
            weekday: Weekday::Friday,
        }
        .as_unix(0);
        let july = Civil {
            year: 2027,
            month: 7,
            day: 15,
            hour: 3,
            minute: 0,
            second: 0,
            weekday: Weekday::Thursday,
        }
        .as_unix(0);
        assert_eq!(zone.offset_at(january), 9 * 3600);
        assert_eq!(zone.offset_at(july), 9 * 3600);
    }

    if let Some(zone) = zone("Pacific/Auckland") {
        let january = Civil {
            year: 2027,
            month: 1,
            day: 15,
            hour: 0,
            minute: 0,
            second: 0,
            weekday: Weekday::Friday,
        }
        .as_unix(0);
        let july = Civil {
            year: 2027,
            month: 7,
            day: 15,
            hour: 0,
            minute: 0,
            second: 0,
            weekday: Weekday::Thursday,
        }
        .as_unix(0);
        assert_eq!(
            zone.offset_at(january),
            13 * 3600,
            "January is summer there"
        );
        assert_eq!(zone.offset_at(july), 12 * 3600, "July is winter there");
    }
}

/// **Every zone in the database parses.** A format this crate reads has one
/// producer and thousands of files; the interesting failure is the one zone
/// with a shape nothing else has.
#[test]
fn every_zone_on_this_machine_parses() {
    let root = Path::new("/usr/share/zoneinfo");
    if !root.exists() {
        println!("skipped: this machine has no timezone database");
        return;
    }
    let mut parsed = 0usize;
    let mut refused: Vec<String> = Vec::new();
    let mut queue = vec![root.to_path_buf()];
    while let Some(dir) = queue.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // `right/` and `posix/` hold whole second copies of the
                // database; one pass over the ordinary files is the check.
                if !matches!(
                    path.file_name().and_then(|n| n.to_str()),
                    Some("right" | "posix")
                ) {
                    queue.push(path);
                }
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            // The database also holds `tzdata.zi`, `leapseconds` and similar
            // text files, which are not timezone files and are not claiming
            // to be.
            if bytes.get(..4) != Some(b"TZif") {
                continue;
            }
            match Zone::parse(&bytes) {
                Ok(_) => parsed += 1,
                Err(e) => refused.push(format!("{}: {e}", path.display())),
            }
        }
    }
    assert!(
        refused.is_empty(),
        "{} of {} zones would not parse: {:?}",
        refused.len(),
        parsed + refused.len(),
        refused
    );
    assert!(
        parsed > 100,
        "only {parsed} zones were found, which is not a database"
    );
}

/// The zone's name comes back, because a host has to be able to say which day
/// it thinks it is having.
#[test]
fn the_zone_says_what_it_is_called() {
    let Some(zone) = zone("Europe/Helsinki") else {
        return;
    };
    assert_eq!(zone.name(), "Europe/Helsinki");
}

/// And the machine's own zone loads, whatever it is.
#[test]
fn the_machine_has_a_local_timezone_and_it_parses() {
    match Zone::local() {
        Ok(zone) => {
            assert!(
                !zone.name().is_empty(),
                "a zone with no name cannot be reported to anybody"
            );
            // Whatever it is, the offset is one a real zone could have:
            // fourteen hours either side of UTC covers every one that exists.
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let offset = zone.offset_at(now);
            assert!(
                (-14 * 3600..=14 * 3600).contains(&offset),
                "{} reports an offset of {offset} seconds",
                zone.name()
            );
        }
        Err(e) => println!("skipped: this machine has no local timezone ({e})"),
    }
}
