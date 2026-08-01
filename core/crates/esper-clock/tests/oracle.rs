#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! This crate's answers against another implementation's.
//!
//! # What this is
//!
//! `scripts/check-clock.py` writes a table of cases — a zone, an instant, and
//! what Python's `zoneinfo` says the local time and offset are — and then runs
//! this test with `ESPER_CLOCK_CASES` pointing at it. Every line is compared.
//!
//! Python's timezone handling is an independent reading of the same files and
//! the same RFC. A parser checked only against itself is checked against its
//! own misunderstanding, and the format has corners — the 64-bit block, the
//! POSIX footer, southern-hemisphere rules, zones that changed their standard
//! offset — where a plausible misreading is right most of the time.
//!
//! # Why it skips by default
//!
//! No `ESPER_CLOCK_CASES`, no comparison: `cargo test` must not need Python.
//! The script is what makes it run, and it says so when Python or the timezone
//! database is missing.

use std::path::PathBuf;

use esper_clock::{Weekday, Zone};

/// One line of the table.
struct Case {
    zone: String,
    unix: i64,
    year: i32,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
    weekday: Weekday,
    offset: i32,
}

fn parse(line: &str) -> Option<Case> {
    let mut f = line.split('\t');
    let zone = f.next()?.to_owned();
    let unix = f.next()?.parse().ok()?;
    let year = f.next()?.parse().ok()?;
    let month = f.next()?.parse().ok()?;
    let day = f.next()?.parse().ok()?;
    let hour = f.next()?.parse().ok()?;
    let minute = f.next()?.parse().ok()?;
    let second = f.next()?.parse().ok()?;
    let weekday = Weekday::named(f.next()?)?;
    let offset = f.next()?.parse().ok()?;
    Some(Case {
        zone,
        unix,
        year,
        month,
        day,
        hour,
        minute,
        second,
        weekday,
        offset,
    })
}

#[test]
fn every_case_the_oracle_produced_agrees() {
    let Some(path) = std::env::var_os("ESPER_CLOCK_CASES") else {
        println!(
            "skipped: run scripts/check-clock.py, which writes a table of cases from Python's \
             zoneinfo and sets ESPER_CLOCK_CASES to it"
        );
        return;
    };
    let text = std::fs::read_to_string(PathBuf::from(&path))
        .unwrap_or_else(|e| panic!("reading the cases at {path:?}: {e}"));

    let mut checked = 0usize;
    let mut zones = std::collections::BTreeMap::new();
    let mut wrong: Vec<String> = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let case = parse(line).unwrap_or_else(|| panic!("a case this test cannot read: {line:?}"));
        let zone = zones.entry(case.zone.clone()).or_insert_with(|| {
            Zone::at_path(&PathBuf::from("/usr/share/zoneinfo").join(&case.zone))
                .unwrap_or_else(|e| panic!("{}: {e}", case.zone))
        });
        let got = zone.civil_at(case.unix);
        let offset = zone.offset_at(case.unix);
        let same = got.year == case.year
            && got.month == case.month
            && got.day == case.day
            && got.hour == case.hour
            && got.minute == case.minute
            && got.second == case.second
            && got.weekday == case.weekday
            && offset == case.offset;
        if !same {
            wrong.push(format!(
                "{} at {}: this crate says {:04}-{:02}-{:02} {:02}:{:02}:{:02} {} offset {}, \
                 the oracle says {:04}-{:02}-{:02} {:02}:{:02}:{:02} {} offset {}",
                case.zone,
                case.unix,
                got.year,
                got.month,
                got.day,
                got.hour,
                got.minute,
                got.second,
                got.weekday.short(),
                offset,
                case.year,
                case.month,
                case.day,
                case.hour,
                case.minute,
                case.second,
                case.weekday.short(),
                case.offset
            ));
        }
        checked += 1;
    }

    assert!(
        wrong.is_empty(),
        "{} of {checked} cases disagree:\n{}",
        wrong.len(),
        wrong
            .iter()
            .take(20)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(
        checked > 100,
        "only {checked} cases were compared, which is not a check"
    );
    println!("{checked} cases across {} zones agree", zones.len());
}
