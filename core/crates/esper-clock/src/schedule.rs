//! When something is allowed: days of the week and a window in the day.
//!
//! # Why this lives beside the clock
//!
//! A schedule is a calendar object. It is written in local days and local
//! hours, it is evaluated against a [`Civil`] time, and getting it right means
//! knowing what "Tuesday" and "18:00" mean on the machine it belongs to —
//! which is the whole of the crate it is in. Putting it next to the device
//! record instead would put the arithmetic somewhere with no timezone.
//!
//! # What a schedule is for
//!
//! Bounding *unattended* access. `unattended` on its own means "any time, for
//! ever", which is the right answer for a machine in a rack and the wrong one
//! for the laptop somebody helps their family with. A schedule makes "always"
//! a choice rather than the only option.
//!
//! It bounds the permission, not the device: outside its hours the device is
//! an ordinary paired device, and somebody at the host can still say yes. A
//! schedule that locked a device out entirely would be a worse control,
//! because the machine most likely to need help at 3am is the one somebody is
//! sitting in front of at 3am.
//!
//! # The window may wrap midnight
//!
//! "22:00 to 06:00" is a real thing to want — a night shift, a backup window —
//! and the naive comparison gets it exactly backwards. So the wrap is a case
//! this type handles rather than a shape it refuses, and **the day is the day
//! the window opened**: a session at 02:00 on Saturday falls inside a Friday
//! window that started at 22:00.

use std::collections::BTreeSet;

use crate::{Civil, Weekday};

/// Minutes in a day, which is the bound on every time in a schedule.
const IN_A_DAY: u16 = 24 * 60;

/// When something is allowed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    /// The days the window opens on. Never empty: a schedule with no days
    /// allows nothing, and something that allows nothing should be absent
    /// rather than present and inert.
    days: BTreeSet<Weekday>,
    /// Minutes after local midnight when the window opens.
    from: u16,
    /// Minutes after local midnight when it closes.
    ///
    /// **Exclusive**, so `08:00-18:00` ends as the clock reaches 18:00 rather
    /// than a minute later, and two windows written back to back do not
    /// overlap by a minute.
    until: u16,
}

impl Schedule {
    /// A schedule for these days and this window.
    ///
    /// # Errors
    ///
    /// If the days are empty, or a time is not a time of day. `from == until`
    /// is refused too: it is either a window of no length or somebody meaning
    /// "all day", and guessing which is worse than asking.
    pub fn new(
        days: impl IntoIterator<Item = Weekday>,
        from: u16,
        until: u16,
    ) -> Result<Self, ScheduleError> {
        let days: BTreeSet<Weekday> = days.into_iter().collect();
        if days.is_empty() {
            return Err(ScheduleError::NoDays);
        }
        if from >= IN_A_DAY || until > IN_A_DAY {
            return Err(ScheduleError::NotATime {
                minutes: from.max(until),
            });
        }
        if from == until {
            return Err(ScheduleError::Empty);
        }
        Ok(Self { days, from, until })
    }

    /// Every hour of every day, which is what `unattended` meant before
    /// schedules existed.
    #[must_use]
    pub fn always() -> Self {
        Self {
            days: Weekday::ALL.into_iter().collect(),
            from: 0,
            until: IN_A_DAY,
        }
    }

    /// The days the window opens on.
    pub fn days(&self) -> impl Iterator<Item = Weekday> + '_ {
        self.days.iter().copied()
    }

    /// The window, as minutes after midnight.
    #[must_use]
    pub const fn window(&self) -> (u16, u16) {
        (self.from, self.until)
    }

    /// Whether the window runs past midnight into the next day.
    #[must_use]
    pub const fn wraps(&self) -> bool {
        self.until < self.from
    }

    /// Whether this is every hour of every day.
    #[must_use]
    pub fn is_always(&self) -> bool {
        self.days.len() == 7 && self.from == 0 && self.until == IN_A_DAY
    }

    /// Whether a local time falls inside this schedule.
    ///
    /// **The only question this type answers**, and the one an access decision
    /// asks. It takes a [`Civil`] rather than an instant, because applying the
    /// timezone is the caller's business and doing it here would hide which
    /// timezone was used.
    #[must_use]
    pub fn allows(&self, now: &Civil) -> bool {
        let minute = now.minute_of_day();
        if self.wraps() {
            // The window opened yesterday if we are before it closes today.
            // **Which day is asked about depends on where in the window we
            // are**: at 23:00 on Friday the day is Friday, and at 02:00 on
            // Saturday the day is still Friday's window.
            if minute < self.until {
                return self.days.contains(&yesterday(now.weekday));
            }
            return minute >= self.from && self.days.contains(&now.weekday);
        }
        self.days.contains(&now.weekday) && minute >= self.from && minute < self.until
    }

    /// What a person writes and reads: `weekdays 08:00-18:00`.
    #[must_use]
    pub fn spelled(&self) -> String {
        format!(
            "{} {}-{}",
            self.days_spelled(),
            clock(self.from),
            clock(self.until)
        )
    }

    /// The day part on its own, using the words somebody would.
    fn days_spelled(&self) -> String {
        let days: Vec<Weekday> = self.days.iter().copied().collect();
        if days.len() == 7 {
            return "daily".to_owned();
        }
        if days == Weekday::WEEKDAYS {
            return "weekdays".to_owned();
        }
        if days == Weekday::WEEKEND {
            return "weekends".to_owned();
        }
        days.iter().map(|d| d.short()).collect::<Vec<_>>().join(",")
    }

    /// Parse what a person wrote.
    ///
    /// `weekdays 08:00-18:00`, `daily 22:00-06:00`, `mon,wed,fri 9-17`,
    /// `always`. Case and spacing are forgiving; anything else is refused by
    /// name rather than interpreted generously, because a schedule somebody
    /// mistyped and this accepted would be an access rule nobody meant.
    ///
    /// # Errors
    ///
    /// If the days or the window cannot be read, or the result would allow
    /// nothing.
    pub fn parse(text: &str) -> Result<Self, ScheduleError> {
        let text = text.trim();
        if text.eq_ignore_ascii_case("always") {
            return Ok(Self::always());
        }
        // **Split at the first digit, not at the first space.** A day list is
        // words and separators and a window starts with a number, so that is
        // the one boundary both `mon, fri 09:30-17:45` and `weekdays 8:00 -
        // 18:00` agree on. Splitting at the first space breaks the former and
        // at the last space breaks the latter; found by a test written from
        // how somebody would actually type it.
        let boundary =
            text.find(|c: char| c.is_ascii_digit())
                .ok_or_else(|| ScheduleError::Unreadable {
                    text: text.to_owned(),
                })?;
        let days_text = text.get(..boundary).unwrap_or_default();
        let window = text.get(boundary..).unwrap_or_default();
        let days = days_of(days_text)?;
        let (from, until) =
            window
                .trim()
                .split_once('-')
                .ok_or_else(|| ScheduleError::Unreadable {
                    text: window.trim().to_owned(),
                })?;
        Self::new(days, minute_of_day(from)?, minute_of_day(until)?)
    }
}

/// The day before, so a window that wrapped midnight can ask about the day it
/// opened on.
const fn yesterday(day: Weekday) -> Weekday {
    match day {
        Weekday::Monday => Weekday::Sunday,
        Weekday::Tuesday => Weekday::Monday,
        Weekday::Wednesday => Weekday::Tuesday,
        Weekday::Thursday => Weekday::Wednesday,
        Weekday::Friday => Weekday::Thursday,
        Weekday::Saturday => Weekday::Friday,
        Weekday::Sunday => Weekday::Saturday,
    }
}

/// Minutes after midnight as `HH:MM`.
fn clock(minutes: u16) -> String {
    // A window ending at midnight is written `24:00`, because `00:00` would
    // read as the start of the day it ends.
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

/// `08:00`, `8:00`, `8`, or `18` — the forms somebody types.
fn minute_of_day(text: &str) -> Result<u16, ScheduleError> {
    let text = text.trim();
    let unreadable = || ScheduleError::Unreadable {
        text: text.to_owned(),
    };
    let (hours, minutes) = match text.split_once(':') {
        Some((h, m)) => (
            h.trim().parse::<u16>().map_err(|_| unreadable())?,
            m.trim().parse::<u16>().map_err(|_| unreadable())?,
        ),
        None => (text.parse::<u16>().map_err(|_| unreadable())?, 0),
    };
    if hours > 24 || minutes > 59 || (hours == 24 && minutes > 0) {
        return Err(ScheduleError::NotATime {
            minutes: hours.saturating_mul(60).saturating_add(minutes),
        });
    }
    Ok(hours * 60 + minutes)
}

/// `weekdays`, `weekends`, `daily`, or a comma-separated list of day names.
fn days_of(text: &str) -> Result<Vec<Weekday>, ScheduleError> {
    // Trailing separators are what a list written in English leaves behind:
    // `mon, fri ` splits into `mon` and `fri` with an empty tail.
    let lower = text
        .trim()
        .trim_end_matches(',')
        .trim()
        .to_ascii_lowercase();
    if lower.is_empty() {
        return Err(ScheduleError::NoDays);
    }
    match lower.as_str() {
        "weekdays" => return Ok(Weekday::WEEKDAYS.to_vec()),
        "weekends" | "weekend" => return Ok(Weekday::WEEKEND.to_vec()),
        "daily" | "everyday" | "every" | "all" => return Ok(Weekday::ALL.to_vec()),
        _ => {}
    }
    let mut days = Vec::new();
    for name in lower.split(',') {
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        days.push(
            Weekday::named(name).ok_or_else(|| ScheduleError::NoSuchDay {
                name: name.to_owned(),
            })?,
        );
    }
    if days.is_empty() {
        return Err(ScheduleError::NoDays);
    }
    Ok(days)
}

/// Why a schedule could not be made.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ScheduleError {
    /// A schedule with no days allows nothing.
    #[error("a schedule needs at least one day")]
    NoDays,
    /// A window of no length.
    #[error("a window that starts when it ends allows nothing; write `always` for all day")]
    Empty,
    /// A time outside a day.
    #[error("{minutes} minutes past midnight is not a time of day")]
    NotATime { minutes: u16 },
    /// A day name nothing recognises.
    #[error("there is no day called {name:?}")]
    NoSuchDay { name: String },
    /// Something that is not a schedule.
    #[error("cannot read {text:?} as a schedule; try `weekdays 08:00-18:00`")]
    Unreadable { text: String },
}

/// Serialised as the text a person writes, because the file it lands in is one
/// they are meant to be able to read and edit.
impl serde::Serialize for Schedule {
    fn serialize<S: serde::Serializer>(&self, out: S) -> Result<S::Ok, S::Error> {
        out.serialize_str(&self.spelled())
    }
}

impl<'de> serde::Deserialize<'de> for Schedule {
    fn deserialize<D: serde::Deserializer<'de>>(from: D) -> Result<Self, D::Error> {
        let text = String::deserialize(from)?;
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    /// A civil time on a given weekday, for asking a schedule about.
    fn at(weekday: Weekday, hour: u8, minute: u8) -> Civil {
        Civil {
            year: 2026,
            month: 9,
            day: 7,
            hour,
            minute,
            second: 0,
            weekday,
        }
    }

    #[test]
    fn a_working_week_allows_the_working_day_and_nothing_else() {
        let s = Schedule::parse("weekdays 08:00-18:00").expect("parsed");
        assert!(s.allows(&at(Weekday::Monday, 8, 0)), "the minute it opens");
        assert!(s.allows(&at(Weekday::Monday, 17, 59)));
        assert!(s.allows(&at(Weekday::Friday, 12, 0)));
        assert!(
            !s.allows(&at(Weekday::Monday, 18, 0)),
            "the end is exclusive, so 18:00 is outside"
        );
        assert!(!s.allows(&at(Weekday::Monday, 7, 59)));
        assert!(!s.allows(&at(Weekday::Saturday, 12, 0)), "not a weekday");
        assert!(!s.allows(&at(Weekday::Sunday, 12, 0)));
    }

    /// **A window that wraps midnight**, which the obvious comparison gets
    /// exactly backwards: at 02:00 it would be outside every window that
    /// starts at 22:00.
    #[test]
    fn a_window_that_wraps_midnight_is_read_the_right_way_round() {
        let s = Schedule::parse("daily 22:00-06:00").expect("parsed");
        assert!(s.wraps());
        assert!(s.allows(&at(Weekday::Monday, 22, 0)), "as it opens");
        assert!(s.allows(&at(Weekday::Monday, 23, 59)));
        assert!(s.allows(&at(Weekday::Tuesday, 0, 0)), "after midnight");
        assert!(s.allows(&at(Weekday::Tuesday, 5, 59)));
        assert!(!s.allows(&at(Weekday::Tuesday, 6, 0)), "as it closes");
        assert!(!s.allows(&at(Weekday::Tuesday, 12, 0)));
        assert!(!s.allows(&at(Weekday::Tuesday, 21, 59)));
    }

    /// **The day is the day the window opened.** A Friday night window covers
    /// the small hours of Saturday and *not* the small hours of Friday.
    #[test]
    fn a_wrapped_window_belongs_to_the_day_it_opened_on() {
        let s = Schedule::parse("fri 22:00-06:00").expect("parsed");
        assert!(s.allows(&at(Weekday::Friday, 23, 0)), "Friday night");
        assert!(
            s.allows(&at(Weekday::Saturday, 2, 0)),
            "Saturday morning is still Friday's window"
        );
        assert!(
            !s.allows(&at(Weekday::Friday, 2, 0)),
            "Friday morning belongs to Thursday's window, which does not exist"
        );
        assert!(!s.allows(&at(Weekday::Saturday, 23, 0)), "Saturday night");
    }

    /// `always` is every hour of every day, and says so about itself.
    #[test]
    fn always_is_every_hour_of_every_day() {
        let s = Schedule::parse("always").expect("parsed");
        assert!(s.is_always());
        for day in Weekday::ALL {
            for hour in 0..24u8 {
                assert!(s.allows(&at(day, hour, 30)), "{day:?} at {hour}");
            }
        }
        assert!(s.allows(&at(Weekday::Sunday, 0, 0)));
        assert!(s.allows(&at(Weekday::Sunday, 23, 59)));
    }

    /// The forms somebody actually types.
    #[test]
    fn the_shapes_a_person_writes_are_read() {
        for (text, spelled) in [
            ("weekdays 08:00-18:00", "weekdays 08:00-18:00"),
            ("WEEKDAYS 8-18", "weekdays 08:00-18:00"),
            ("  weekdays   8:00 - 18:00  ", "weekdays 08:00-18:00"),
            ("weekends 10:00-23:00", "weekends 10:00-23:00"),
            ("daily 00:00-24:00", "daily 00:00-24:00"),
            ("mon,wed,fri 9-17", "mon,wed,fri 09:00-17:00"),
            ("Mon, Fri 09:30-17:45", "mon,fri 09:30-17:45"),
            ("sun 22:00-06:00", "sun 22:00-06:00"),
        ] {
            let s = Schedule::parse(text).unwrap_or_else(|e| panic!("{text:?}: {e}"));
            assert_eq!(s.spelled(), spelled, "{text:?}");
            // And what it prints, it can read back.
            assert_eq!(Schedule::parse(&s.spelled()).expect("re-parsed"), s);
        }
    }

    /// **`daily 00:00-24:00` is `always`**, so the two spellings mean one
    /// thing rather than two nearly-identical rules.
    #[test]
    fn a_full_day_every_day_is_always() {
        let written = Schedule::parse("daily 00:00-24:00").expect("parsed");
        assert!(written.is_always());
        assert_eq!(written, Schedule::always());
    }

    /// Nonsense is refused by name. A schedule somebody mistyped and this
    /// accepted would be an access rule nobody meant.
    #[test]
    fn what_cannot_be_read_is_refused_rather_than_guessed() {
        for text in [
            "",
            "weekdays",
            "weekdays 08:00",
            "caturday 08:00-18:00",
            "weekdays 25:00-26:00",
            "weekdays 08:70-18:00",
            "weekdays 08:00-08:00",
            "always 08:00-18:00",
            "8-18",
            "weekdays eight to six",
        ] {
            assert!(
                Schedule::parse(text).is_err(),
                "{text:?} was accepted as a schedule"
            );
        }
    }

    /// A schedule with no days, or a window of no length, cannot be
    /// constructed — because something that allows nothing should be absent
    /// rather than present and inert.
    #[test]
    fn a_schedule_that_allows_nothing_cannot_be_made() {
        assert_eq!(Schedule::new(Vec::new(), 0, 60), Err(ScheduleError::NoDays));
        assert_eq!(
            Schedule::new(Weekday::ALL, 480, 480),
            Err(ScheduleError::Empty)
        );
        assert!(matches!(
            Schedule::new(Weekday::ALL, 0, 2000),
            Err(ScheduleError::NotATime { .. })
        ));
    }

    /// It survives the file it is stored in, as the text somebody wrote.
    #[test]
    fn it_round_trips_through_the_store_as_readable_text() {
        let s = Schedule::parse("weekdays 08:00-18:00").expect("parsed");
        let json = serde_json::to_string(&s).expect("serialised");
        assert_eq!(json, "\"weekdays 08:00-18:00\"");
        let back: Schedule = serde_json::from_str(&json).expect("deserialised");
        assert_eq!(back, s);
        // And a file somebody edited by hand into something unreadable is a
        // parse failure rather than a schedule that allows the wrong hours.
        assert!(serde_json::from_str::<Schedule>("\"weekdays\"").is_err());
    }
}
