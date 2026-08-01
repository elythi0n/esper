//! What time it is where the host is.
//!
//! # Why this exists at all
//!
//! A schedule — "weekdays 08:00 to 18:00" — is about the day somebody standing
//! at the host is having. Evaluating it needs three things the standard library
//! does not provide: the machine's timezone, the offset that timezone had *at a
//! particular instant*, and the calendar arithmetic to turn an instant into a
//! date and a time of day.
//!
//! A schedule in UTC would be wrong twice a year and confusing all year. A
//! schedule evaluated with a fixed offset would be wrong for half of it. So the
//! machine's own timezone data is read and used, which is the only answer that
//! is right in March and October as well as in June.
//!
//! # Why not a dependency
//!
//! The obvious crate for this reads the local offset through an API that is
//! documented as unsound in a multithreaded process, and answers with an error
//! rather than a time in exactly the case a daemon is in. The next one along
//! brings a timezone database of its own, which then disagrees with the
//! machine's whenever one is updated and the other is not.
//!
//! What is actually needed is small: read `/etc/localtime`, which is
//! [RFC 8536](https://www.rfc-editor.org/rfc/rfc8536) and documented, find the
//! offset in force at an instant, and do sixty lines of calendar arithmetic. It
//! is all pure functions over bytes, which is the kind of thing this project
//! prefers to own — see `esper-png` for the same reasoning applied to a
//! different format.
//!
//! # `$TZ` is deliberately ignored
//!
//! A schedule is a statement about the machine, so the machine's own answer is
//! the one that counts: `/etc/localtime`. `TZ` in the environment is a
//! per-process override, and honouring it would mean a host started by a
//! service manager with `TZ=UTC` quietly enforcing a schedule three hours away
//! from the one somebody wrote — and would make an environment variable part
//! of an access-control decision. The resolved zone is reported instead, so
//! anybody can see which day the host thinks it is having.

#![forbid(unsafe_code)]

use std::path::Path;

pub mod schedule;

pub use schedule::{Schedule, ScheduleError};

/// Where the machine keeps its answer.
pub const LOCALTIME: &str = "/etc/localtime";

/// Seconds in a minute, an hour and a day, named so the arithmetic below reads.
const MINUTE: i64 = 60;
const HOUR: i64 = 60 * MINUTE;
const DAY: i64 = 24 * HOUR;

/// A day of the week.
///
/// **Monday first**, because a schedule is written about a working week and
/// "weekdays" is Monday to Friday. The numbering is this type's own and is not
/// on any wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Weekday {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

impl Weekday {
    /// Every day, in order, for a caller building a set.
    pub const ALL: [Self; 7] = [
        Self::Monday,
        Self::Tuesday,
        Self::Wednesday,
        Self::Thursday,
        Self::Friday,
        Self::Saturday,
        Self::Sunday,
    ];

    /// Monday to Friday.
    pub const WEEKDAYS: [Self; 5] = [
        Self::Monday,
        Self::Tuesday,
        Self::Wednesday,
        Self::Thursday,
        Self::Friday,
    ];

    /// Saturday and Sunday.
    pub const WEEKEND: [Self; 2] = [Self::Saturday, Self::Sunday];

    /// The three-letter name a person reads and writes.
    #[must_use]
    pub const fn short(self) -> &'static str {
        match self {
            Self::Monday => "mon",
            Self::Tuesday => "tue",
            Self::Wednesday => "wed",
            Self::Thursday => "thu",
            Self::Friday => "fri",
            Self::Saturday => "sat",
            Self::Sunday => "sun",
        }
    }

    /// The day that name means, however it was capitalised.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        let lower = name.to_ascii_lowercase();
        Self::ALL.into_iter().find(|d| d.short() == lower)
    }

    /// Which day it is, counting from a day number where 0 is 1 January 1970.
    ///
    /// 1 January 1970 was a Thursday, which is where the offset comes from.
    #[must_use]
    const fn from_day_number(days: i64) -> Self {
        // `rem_euclid` so a date before 1970 lands in range rather than
        // negative — the arithmetic has to work for a clock somebody has set
        // wrongly as well as for one they have not.
        match (days + 3).rem_euclid(7) {
            0 => Self::Monday,
            1 => Self::Tuesday,
            2 => Self::Wednesday,
            3 => Self::Thursday,
            4 => Self::Friday,
            5 => Self::Saturday,
            _ => Self::Sunday,
        }
    }
}

/// A date and a time of day, with no timezone attached.
///
/// What a person means when they say "quarter past nine on Tuesday". Produced
/// by [`Zone::civil_at`], which is where the timezone was applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Civil {
    pub year: i32,
    /// 1 to 12.
    pub month: u8,
    /// 1 to 31.
    pub day: u8,
    /// 0 to 23.
    pub hour: u8,
    /// 0 to 59.
    pub minute: u8,
    /// 0 to 60, because a leap second is a real thing a clock can report.
    pub second: u8,
    pub weekday: Weekday,
}

impl Civil {
    /// Minutes since midnight.
    ///
    /// What a schedule compares against, so it is derived here rather than in
    /// every caller.
    #[must_use]
    pub const fn minute_of_day(&self) -> u16 {
        (self.hour as u16) * 60 + self.minute as u16
    }

    /// The instant this civil time describes, in a zone with this offset.
    ///
    /// The inverse of the conversion below, for a caller that knows the offset
    /// — the tests, and anything that wants to say when a window opens.
    #[must_use]
    pub const fn as_unix(&self, offset_seconds: i32) -> i64 {
        let days = days_from_civil(self.year, self.month, self.day);
        days * DAY + (self.hour as i64) * HOUR + (self.minute as i64) * MINUTE + self.second as i64
            - offset_seconds as i64
    }
}

/// An instant as civil time in UTC.
///
/// The arithmetic every other conversion is built from: apply an offset to the
/// instant first, then convert.
#[must_use]
pub fn utc_at(unix: i64) -> Civil {
    // Floor division, so an instant before 1970 gives the right day rather
    // than rounding towards zero and landing a day late.
    let days = unix.div_euclid(DAY);
    let secs = unix.rem_euclid(DAY);
    let (year, month, day) = civil_from_days(days);
    Civil {
        year,
        month,
        day,
        // Each of these is bounded by the division above, so the casts are
        // exact rather than hopeful.
        hour: (secs / HOUR) as u8,
        minute: ((secs % HOUR) / MINUTE) as u8,
        second: (secs % MINUTE) as u8,
        weekday: Weekday::from_day_number(days),
    }
}

/// Days since 1 January 1970 for a civil date, proleptic Gregorian.
///
/// Howard Hinnant's algorithm, which is exact for every year the type can hold
/// and needs no table. Its shape is unusual because it moves the start of the
/// year to March, which is what makes the leap day the *last* day of a year and
/// removes every special case.
#[must_use]
const fn days_from_civil(year: i32, month: u8, day: u8) -> i64 {
    let y = if month <= 2 { year - 1 } else { year } as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = month as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The civil date of a day number. The inverse of [`days_from_civil`].
#[must_use]
const fn civil_from_days(days: i64) -> (i32, u8, u8) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    ((if m <= 2 { y + 1 } else { y }) as i32, m as u8, d as u8)
}

/// A timezone: what the offset from UTC has been, and what it will be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Zone {
    /// What the zone is called, for saying which day the host thinks it is
    /// having. From the path, because the file itself does not carry it.
    name: String,
    /// Transition instants and the offset that starts at each, in order.
    changes: Vec<(i64, i32)>,
    /// The offset before the first transition.
    before: i32,
    /// What happens after the last transition, from the file's own footer.
    ///
    /// `None` when the file has none, in which case the last transition's
    /// offset stands for ever — which is right for a zone that has stopped
    /// changing and wrong for one whose data simply ran out. See
    /// [`Zone::offset_at`].
    after: Option<Posix>,
}

/// The POSIX rule a TZif file ends with, for instants past its last transition.
///
/// **Why this is parsed rather than ignored.** A tzdata build stops emitting
/// transitions somewhere in the future — 2037 in the one this was written
/// against, and *this year* in builds configured to rely on the footer alone.
/// A reader that stopped at the last transition would then apply one offset all
/// year, which is wrong for half of it. It would also be wrong *silently*, and
/// the first symptom would be an unattended session refused an hour early on a
/// morning in March.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Posix {
    /// Offset in force outside daylight saving.
    standard: i32,
    /// Offset in force during it, and the rules for when it starts and ends.
    daylight: Option<(i32, Rule, Rule)>,
}

/// One end of a daylight-saving period: which day of which month, and at what
/// local time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rule {
    /// 1 to 12.
    month: u8,
    /// 1 to 5, where 5 means "the last one in the month".
    week: u8,
    /// 0 is Sunday, as POSIX numbers it.
    day: u8,
    /// Seconds after midnight, local time. Can exceed a day and can be
    /// negative, which real rules do use.
    at: i64,
}

impl Zone {
    /// The machine's own timezone.
    ///
    /// # Errors
    ///
    /// If `/etc/localtime` cannot be read or is not a timezone file.
    pub fn local() -> Result<Self, ClockError> {
        Self::at_path(Path::new(LOCALTIME))
    }

    /// A timezone from a file.
    ///
    /// # Errors
    ///
    /// If the file cannot be read or is not a timezone file.
    pub fn at_path(path: &Path) -> Result<Self, ClockError> {
        let bytes = std::fs::read(path).map_err(|e| ClockError::Unreadable {
            path: path.display().to_string(),
            detail: e.to_string(),
        })?;
        let mut zone = Self::parse(&bytes)?;
        zone.name = name_of(path);
        Ok(zone)
    }

    /// What the zone is called.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// A fixed offset, for a caller that has no timezone data.
    ///
    /// **Not a fallback.** Nothing constructs this from a failure to read the
    /// machine's zone: a schedule evaluated in the wrong timezone is worse than
    /// one that refuses to be evaluated, because it looks like it worked. It
    /// exists so tests can state an offset, and so that a caller with a genuine
    /// fixed offset — UTC — can say so.
    #[must_use]
    pub fn fixed(name: &str, offset_seconds: i32) -> Self {
        Self {
            name: name.to_owned(),
            changes: Vec::new(),
            before: offset_seconds,
            after: None,
        }
    }

    /// The offset from UTC in force at an instant, in seconds.
    #[must_use]
    pub fn offset_at(&self, unix: i64) -> i32 {
        // Past the last transition the footer decides, if there is one. This
        // is the case that would otherwise be wrong for half of every year
        // once a tzdata build stops emitting transitions.
        if let Some((last, offset)) = self.changes.last().copied()
            && unix >= last
        {
            return match self.after.as_ref() {
                Some(rule) => rule.offset_at(unix),
                None => offset,
            };
        }
        // Before the first, the zone's own answer for "as long as anyone
        // recorded".
        let Some(&(first, _)) = self.changes.first() else {
            return self.before;
        };
        if unix < first {
            return self.before;
        }
        // Otherwise the last transition at or before the instant. A binary
        // search because the table has a few hundred entries and this is on
        // the path of an access-control decision.
        match self.changes.binary_search_by_key(&unix, |(at, _)| *at) {
            Ok(exact) => self.changes.get(exact).map_or(self.before, |(_, o)| *o),
            Err(next) => self
                .changes
                .get(next.saturating_sub(1))
                .map_or(self.before, |(_, o)| *o),
        }
    }

    /// An instant as civil time in this zone.
    #[must_use]
    pub fn civil_at(&self, unix: i64) -> Civil {
        utc_at(unix.saturating_add(i64::from(self.offset_at(unix))))
    }

    /// Parse a TZif file.
    ///
    /// # Errors
    ///
    /// If the bytes are not a timezone file, or claim more data than they hold.
    pub fn parse(bytes: &[u8]) -> Result<Self, ClockError> {
        let first = Header::read(bytes, 0)?;
        // **The 64-bit block, not the 32-bit one.** A version 2 or 3 file
        // carries both: the first is for readers that predate the format's
        // extension and is truncated to what fits in 32 bits, and the second
        // is the whole truth. Reading the first would work until 2038 and then
        // stop, which is the sort of bug that is written today and found by
        // somebody else.
        if first.version == b'1' {
            let block = read_block(bytes, first.after_header(), &first, 4)?;
            return Ok(Self {
                name: String::new(),
                changes: block.changes,
                before: block.before,
                after: None,
            });
        }
        let second_at = first.after_header() + first.block_len(4);
        let second = Header::read(bytes, second_at)?;
        let block = read_block(bytes, second.after_header(), &second, 8)?;
        let after = footer(bytes, block.end);
        Ok(Self {
            name: String::new(),
            changes: block.changes,
            before: block.before,
            after,
        })
    }
}

impl Posix {
    /// The offset this rule gives at an instant.
    fn offset_at(&self, unix: i64) -> i32 {
        let Some((daylight, start, end)) = self.daylight.as_ref().copied() else {
            return self.standard;
        };
        // The year is taken in *standard* time, so a rule at the very start or
        // end of a year is attributed to the year it belongs to.
        let year = utc_at(unix.saturating_add(i64::from(self.standard))).year;
        let begins = start.instant(year, self.standard);
        let ends = end.instant(year, daylight);
        if begins <= ends {
            // Northern hemisphere: daylight saving is a window inside the year.
            if unix >= begins && unix < ends {
                daylight
            } else {
                self.standard
            }
        } else if unix >= begins || unix < ends {
            // Southern hemisphere: the window wraps the new year.
            daylight
        } else {
            self.standard
        }
    }
}

impl Rule {
    /// The instant this rule fires in a given year, given the offset in force
    /// just before it.
    fn instant(&self, year: i32, offset: i32) -> i64 {
        let day = self.day_of_month(year);
        let days = days_from_civil(year, self.month, day);
        days * DAY + self.at - i64::from(offset)
    }

    /// Which day of the month the rule's "nth weekday" lands on.
    fn day_of_month(&self, year: i32) -> u8 {
        let first = days_from_civil(year, self.month, 1);
        // POSIX numbers Sunday 0; this crate's `Weekday` starts at Monday, so
        // the two are lined up here rather than in the arithmetic below.
        let first_weekday = match Weekday::from_day_number(first) {
            Weekday::Sunday => 0u8,
            Weekday::Monday => 1,
            Weekday::Tuesday => 2,
            Weekday::Wednesday => 3,
            Weekday::Thursday => 4,
            Weekday::Friday => 5,
            Weekday::Saturday => 6,
        };
        let shift = (i16::from(self.day) - i16::from(first_weekday)).rem_euclid(7);
        let mut day = 1 + shift + (i16::from(self.week) - 1) * 7;
        // Week 5 means "the last one", which is week 4 or 5 depending on the
        // month — so it is stepped back until it lands inside it.
        let length = i16::from(days_in_month(year, self.month));
        while day > length {
            day -= 7;
        }
        u8::try_from(day.max(1)).unwrap_or(1)
    }
}

/// How many days a month has, leap years included.
const fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        2 => 28,
        // Not a month. One day, so arithmetic on it stays in range instead of
        // dividing by zero somewhere else.
        _ => 1,
    }
}

const fn is_leap(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// A TZif header, in either of the two blocks.
#[derive(Debug)]
struct Header {
    version: u8,
    at: usize,
    counts: [u32; 6],
}

impl Header {
    fn read(bytes: &[u8], at: usize) -> Result<Self, ClockError> {
        let head = bytes
            .get(at..at.saturating_add(44))
            .ok_or(ClockError::Truncated { wanted: at + 44 })?;
        if head.get(..4) != Some(b"TZif") {
            return Err(ClockError::NotATimezone);
        }
        let version = head.get(4).copied().unwrap_or(b'1');
        let mut counts = [0u32; 6];
        for (i, slot) in counts.iter_mut().enumerate() {
            let from = 20 + i * 4;
            let raw = head
                .get(from..from + 4)
                .ok_or(ClockError::Truncated { wanted: at + 44 })?;
            *slot = u32::from_be_bytes([
                raw.first().copied().unwrap_or(0),
                raw.get(1).copied().unwrap_or(0),
                raw.get(2).copied().unwrap_or(0),
                raw.get(3).copied().unwrap_or(0),
            ]);
        }
        Ok(Self {
            version,
            at,
            counts,
        })
    }

    const fn after_header(&self) -> usize {
        self.at + 44
    }

    fn isutcnt(&self) -> usize {
        self.counts.first().copied().unwrap_or(0) as usize
    }
    fn isstdcnt(&self) -> usize {
        self.counts.get(1).copied().unwrap_or(0) as usize
    }
    fn leapcnt(&self) -> usize {
        self.counts.get(2).copied().unwrap_or(0) as usize
    }
    fn timecnt(&self) -> usize {
        self.counts.get(3).copied().unwrap_or(0) as usize
    }
    fn typecnt(&self) -> usize {
        self.counts.get(4).copied().unwrap_or(0) as usize
    }
    fn charcnt(&self) -> usize {
        self.counts.get(5).copied().unwrap_or(0) as usize
    }

    /// How long this block's data is, for a given transition-time width.
    fn block_len(&self, width: usize) -> usize {
        self.timecnt() * width
            + self.timecnt()
            + self.typecnt() * 6
            + self.charcnt()
            + self.leapcnt() * (width + 4)
            + self.isstdcnt()
            + self.isutcnt()
    }
}

/// What one data block holds: the transitions, the offset before them, and
/// where the block ended so the footer can be found.
struct Block {
    changes: Vec<(i64, i32)>,
    before: i32,
    end: usize,
}

/// Read one data block.
fn read_block(bytes: &[u8], at: usize, header: &Header, width: usize) -> Result<Block, ClockError> {
    let count = header.timecnt();
    let types = header.typecnt();
    if types == 0 {
        return Err(ClockError::NotATimezone);
    }
    let times_at = at;
    let index_at = times_at + count * width;
    let type_at = index_at + count;
    let end = at + header.block_len(width);
    if bytes.len() < end {
        return Err(ClockError::Truncated { wanted: end });
    }

    // The offsets, one per local time type.
    let mut offsets = Vec::with_capacity(types);
    let mut standard_first = None;
    for i in 0..types {
        let from = type_at + i * 6;
        let raw = bytes
            .get(from..from + 6)
            .ok_or(ClockError::Truncated { wanted: from + 6 })?;
        let seconds = i32::from_be_bytes([
            raw.first().copied().unwrap_or(0),
            raw.get(1).copied().unwrap_or(0),
            raw.get(2).copied().unwrap_or(0),
            raw.get(3).copied().unwrap_or(0),
        ]);
        let is_dst = raw.get(4).copied().unwrap_or(0) != 0;
        if !is_dst && standard_first.is_none() {
            standard_first = Some(seconds);
        }
        offsets.push(seconds);
    }

    let mut changes = Vec::with_capacity(count);
    for i in 0..count {
        let from = times_at + i * width;
        let raw = bytes.get(from..from + width).ok_or(ClockError::Truncated {
            wanted: from + width,
        })?;
        let when = if width == 8 {
            let mut b = [0u8; 8];
            for (slot, byte) in b.iter_mut().zip(raw) {
                *slot = *byte;
            }
            i64::from_be_bytes(b)
        } else {
            let mut b = [0u8; 4];
            for (slot, byte) in b.iter_mut().zip(raw) {
                *slot = *byte;
            }
            i64::from(i32::from_be_bytes(b))
        };
        let which = bytes.get(index_at + i).copied().unwrap_or(0) as usize;
        let offset = offsets
            .get(which)
            .copied()
            .ok_or(ClockError::NotATimezone)?;
        changes.push((when, offset));
    }
    // **In order, because a binary search is about to trust that.** A file
    // whose transitions are not sorted is malformed; sorting rather than
    // refusing keeps a strange file usable, and the answer is right either
    // way.
    changes.sort_by_key(|(at, _)| *at);

    // Before the first transition, RFC 8536 says to use the first type that is
    // not daylight saving, and type zero if they all are.
    let before = standard_first
        .or_else(|| offsets.first().copied())
        .unwrap_or(0);
    Ok(Block {
        changes,
        before,
        end,
    })
}

/// The POSIX TZ string a version 2 or 3 file ends with, if it has one.
///
/// Absent or unparseable is `None` rather than an error: the transitions are
/// still usable and cover every instant a running host asks about. What is lost
/// is correctness past the last of them, which [`Zone::offset_at`] documents.
fn footer(bytes: &[u8], end: usize) -> Option<Posix> {
    let rest = bytes.get(end..)?;
    let text = std::str::from_utf8(rest).ok()?;
    let spec = text.trim().trim_matches('\n');
    if spec.is_empty() {
        return None;
    }
    parse_posix(spec)
}

/// Parse `EET-2EEST,M3.5.0/3,M10.5.0/4` and its simpler forms.
fn parse_posix(spec: &str) -> Option<Posix> {
    let mut parts = spec.split(',');
    let names = parts.next()?;
    let (standard_name, rest) = take_name(names)?;
    let _ = standard_name;
    let (standard, rest) = take_offset(rest)?;
    // POSIX offsets are *west* positive, which is the opposite sign from the
    // one everybody means by "UTC+2". Negated here, once, rather than in three
    // places that would each have to remember.
    let standard = -standard;
    if rest.is_empty() {
        return Some(Posix {
            standard,
            daylight: None,
        });
    }
    let (daylight_name, rest) = take_name(rest)?;
    let _ = daylight_name;
    // A daylight name with no offset means one hour ahead, which is what
    // almost every zone does and what POSIX says to assume.
    let daylight = if rest.is_empty() {
        standard + 3600
    } else {
        -take_offset(rest)?.0
    };
    let start = parse_rule(parts.next()?)?;
    let end = parse_rule(parts.next()?)?;
    Some(Posix {
        standard,
        daylight: Some((daylight, start, end)),
    })
}

/// A zone abbreviation: letters, or anything inside angle brackets.
fn take_name(text: &str) -> Option<(&str, &str)> {
    if let Some(rest) = text.strip_prefix('<') {
        let close = rest.find('>')?;
        return Some((rest.get(..close)?, rest.get(close + 1..)?));
    }
    let end = text
        .find(|c: char| c.is_ascii_digit() || c == '+' || c == '-' || c == ',')
        .unwrap_or(text.len());
    if end < 3 {
        return None;
    }
    Some((text.get(..end)?, text.get(end..)?))
}

/// An offset as `h`, `hh:mm` or `hh:mm:ss`, with an optional sign.
fn take_offset(text: &str) -> Option<(i32, &str)> {
    let end = text
        .find(|c: char| !(c.is_ascii_digit() || c == ':' || c == '+' || c == '-'))
        .unwrap_or(text.len());
    let (head, tail) = (text.get(..end)?, text.get(end..)?);
    Some((parse_hms(head)?, tail))
}

fn parse_hms(text: &str) -> Option<i32> {
    let (sign, digits) = match text.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, text.strip_prefix('+').unwrap_or(text)),
    };
    let mut fields = digits.split(':');
    let hours: i32 = fields.next()?.parse().ok()?;
    let minutes: i32 = fields.next().unwrap_or("0").parse().ok()?;
    let seconds: i32 = fields.next().unwrap_or("0").parse().ok()?;
    Some(sign * (hours * 3600 + minutes * 60 + seconds))
}

/// Parse `M3.5.0/3` — month, week, day, and the local time it happens.
///
/// The `Jn` and `n` forms POSIX also allows name a day of the year rather than
/// a weekday. No zone in the database uses them, so they are refused rather
/// than half-implemented: `None` here means the transitions are used, which is
/// correct today and merely imprecise in 2038.
fn parse_rule(text: &str) -> Option<Rule> {
    let body = text.strip_prefix('M')?;
    let (date, at) = match body.split_once('/') {
        Some((date, at)) => (date, parse_hms(at)? as i64),
        // POSIX's default: two in the morning, local time.
        None => (body, 2 * HOUR),
    };
    let mut fields = date.split('.');
    let month: u8 = fields.next()?.parse().ok()?;
    let week: u8 = fields.next()?.parse().ok()?;
    let day: u8 = fields.next()?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=5).contains(&week) || day > 6 {
        return None;
    }
    Some(Rule {
        month,
        week,
        day,
        at,
    })
}

/// What a timezone file is called, from where it was found.
///
/// `/etc/localtime` is usually a symlink into the database, so the useful name
/// is the target's — `Europe/Helsinki` rather than `localtime`.
fn name_of(path: &Path) -> String {
    let resolved = std::fs::read_link(path).unwrap_or_else(|_| path.to_path_buf());
    let text = resolved.to_string_lossy();
    // The two directory names the database lives under, so the answer is the
    // zone rather than the path to it.
    for marker in ["/zoneinfo/", "/usr/share/zoneinfo/"] {
        if let Some(at) = text.find(marker)
            && let Some(name) = text.get(at + marker.len()..)
        {
            return name.to_owned();
        }
    }
    text.into_owned()
}

/// Why the time could not be worked out.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ClockError {
    /// The machine's timezone file could not be read.
    #[error("cannot read {path}: {detail}")]
    Unreadable { path: String, detail: String },
    /// The bytes are not a timezone file.
    #[error("not a timezone file")]
    NotATimezone,
    /// The file claims more data than it holds.
    #[error("a timezone file that claims {wanted} bytes and is shorter")]
    Truncated { wanted: usize },
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    /// The calendar arithmetic, against dates whose day number is known.
    #[test]
    fn the_calendar_arithmetic_is_exact_at_known_dates() {
        for (year, month, day, days) in [
            (1970, 1, 1, 0),
            (1970, 1, 2, 1),
            (1969, 12, 31, -1),
            (2000, 3, 1, 11_017),
            (2024, 2, 29, 19_782),
            (2026, 9, 7, 20_703),
            (1900, 1, 1, -25_567),
        ] {
            assert_eq!(
                days_from_civil(year, month, day),
                days,
                "{year}-{month}-{day}"
            );
            assert_eq!(civil_from_days(days), (year, month, day), "day {days}");
        }
    }

    /// **Round trip over a century**, because an off-by-one in a leap rule is
    /// invisible at any single date and obvious across a few thousand.
    #[test]
    fn every_day_for_a_century_round_trips() {
        for days in -25_567..=36_524 {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days, "{y}-{m}-{d}");
        }
    }

    /// 1 January 1970 was a Thursday, and the week walks from there in both
    /// directions.
    #[test]
    fn the_weekday_is_right_including_before_1970() {
        assert_eq!(Weekday::from_day_number(0), Weekday::Thursday);
        assert_eq!(Weekday::from_day_number(1), Weekday::Friday);
        assert_eq!(Weekday::from_day_number(3), Weekday::Sunday);
        assert_eq!(Weekday::from_day_number(4), Weekday::Monday);
        assert_eq!(Weekday::from_day_number(-1), Weekday::Wednesday);
        assert_eq!(Weekday::from_day_number(-4), Weekday::Sunday);
        // 7 September 2026 is a Monday.
        assert_eq!(utc_at(20_703 * DAY).weekday, Weekday::Monday);
    }

    /// An instant becomes the date and time somebody would read off a clock.
    #[test]
    fn an_instant_becomes_a_date_and_a_time() {
        // 2026-09-07T14:40:59Z
        let c = utc_at(1_788_792_059);
        assert_eq!(
            (c.year, c.month, c.day, c.hour, c.minute, c.second),
            (2026, 9, 7, 14, 40, 59)
        );
        assert_eq!(c.weekday, Weekday::Monday);
        assert_eq!(c.minute_of_day(), 14 * 60 + 40);
        // And back.
        assert_eq!(c.as_unix(0), 1_788_792_059);
        assert_eq!(c.as_unix(3 * 3600), 1_788_792_059 - 3 * 3600);
    }

    /// A time before 1970 must not round towards zero, which is the mistake
    /// integer division makes and `div_euclid` does not.
    #[test]
    fn an_instant_before_1970_is_the_day_it_actually_was() {
        // 1969-12-31T23:59:59Z is one second before the epoch.
        let c = utc_at(-1);
        assert_eq!(
            (c.year, c.month, c.day, c.hour, c.minute, c.second),
            (1969, 12, 31, 23, 59, 59)
        );
    }

    /// Names round trip, and unknown ones are refused rather than guessed.
    #[test]
    fn weekday_names_round_trip() {
        for day in Weekday::ALL {
            assert_eq!(Weekday::named(day.short()), Some(day));
            assert_eq!(Weekday::named(&day.short().to_uppercase()), Some(day));
        }
        assert_eq!(Weekday::named("caturday"), None);
        assert_eq!(Weekday::named(""), None);
    }

    /// **The POSIX footer, parsed.** Helsinki's, which is the shape almost
    /// every European zone has.
    #[test]
    fn a_posix_footer_is_parsed_the_way_posix_means_it() {
        let p = parse_posix("EET-2EEST,M3.5.0/3,M10.5.0/4").expect("parsed");
        // POSIX writes the offset west-positive; two hours *east* is what the
        // zone actually is.
        assert_eq!(p.standard, 2 * 3600);
        let (daylight, start, end) = p.daylight.expect("a daylight rule");
        assert_eq!(daylight, 3 * 3600);
        assert_eq!(
            start,
            Rule {
                month: 3,
                week: 5,
                day: 0,
                at: 3 * HOUR
            }
        );
        assert_eq!(
            end,
            Rule {
                month: 10,
                week: 5,
                day: 0,
                at: 4 * HOUR
            }
        );
    }

    /// A zone with no daylight saving, and one whose name is bracketed —
    /// both real forms in the database.
    #[test]
    fn footers_without_daylight_saving_and_with_bracketed_names() {
        let p = parse_posix("UTC0").expect("parsed");
        assert_eq!(p.standard, 0);
        assert!(p.daylight.is_none());

        let p = parse_posix("IST-2IDT,M3.4.4/26,M10.5.0").expect("parsed");
        assert_eq!(p.standard, 2 * 3600);
        assert_eq!(p.daylight.map(|(d, _, _)| d), Some(3 * 3600));

        let p = parse_posix("<+07>-7").expect("parsed");
        assert_eq!(p.standard, 7 * 3600);
        assert!(p.daylight.is_none());
    }

    /// **The last Sunday in March 2038**, which is what the footer is for:
    /// past every transition a tzdata build recorded.
    #[test]
    fn the_footer_puts_daylight_saving_where_the_calendar_does() {
        let p = parse_posix("EET-2EEST,M3.5.0/3,M10.5.0/4").expect("parsed");
        // 2038-03-28 is the last Sunday in March.
        let (_, start, end) = p.daylight.expect("a rule");
        assert_eq!(start.day_of_month(2038), 28);
        assert_eq!(end.day_of_month(2038), 31);

        // An hour before the change, standard time; an hour after, daylight.
        let change = start.instant(2038, 2 * 3600);
        assert_eq!(p.offset_at(change - 3600), 2 * 3600);
        assert_eq!(p.offset_at(change + 3600), 3 * 3600);
        // And in December, standard again.
        let december = Civil {
            year: 2038,
            month: 12,
            day: 1,
            hour: 12,
            minute: 0,
            second: 0,
            weekday: Weekday::Monday,
        }
        .as_unix(2 * 3600);
        assert_eq!(p.offset_at(december), 2 * 3600);
    }

    /// **A southern-hemisphere rule wraps the new year**, which the northern
    /// arithmetic gets backwards if it is not handled.
    #[test]
    fn a_rule_that_wraps_the_year_is_read_the_right_way_round() {
        // New Zealand: daylight saving from late September to early April.
        let p = parse_posix("NZST-12NZDT,M9.5.0,M4.1.0/3").expect("parsed");
        assert_eq!(p.standard, 12 * 3600);
        let january = Civil {
            year: 2038,
            month: 1,
            day: 15,
            hour: 12,
            minute: 0,
            second: 0,
            weekday: Weekday::Friday,
        }
        .as_unix(13 * 3600);
        let june = Civil {
            year: 2038,
            month: 6,
            day: 15,
            hour: 12,
            minute: 0,
            second: 0,
            weekday: Weekday::Tuesday,
        }
        .as_unix(12 * 3600);
        assert_eq!(p.offset_at(january), 13 * 3600, "January is summer there");
        assert_eq!(p.offset_at(june), 12 * 3600, "June is winter there");
    }

    /// "The last one in the month" is week 5, and lands on the fourth
    /// occurrence in a month that has only four.
    #[test]
    fn the_last_weekday_of_a_month_is_found_in_short_months_too() {
        // February 2038 starts on a Monday and has 28 days, so the last
        // Sunday is the 28th and there are exactly four.
        let last_sunday = Rule {
            month: 2,
            week: 5,
            day: 0,
            at: 0,
        };
        assert_eq!(last_sunday.day_of_month(2038), 28);
        // And the first Sunday is the 7th.
        let first_sunday = Rule {
            week: 1,
            ..last_sunday
        };
        assert_eq!(first_sunday.day_of_month(2038), 7);
        // A leap February has 29 days: 2024 started on a Thursday, so the last
        // Thursday is the 29th.
        let last_thursday = Rule {
            month: 2,
            week: 5,
            day: 4,
            at: 0,
        };
        assert_eq!(last_thursday.day_of_month(2024), 29);
    }

    /// A fixed zone is a zone, so a test or a caller with a genuine fixed
    /// offset needs no file.
    #[test]
    fn a_fixed_zone_answers_the_same_thing_at_every_instant() {
        let utc = Zone::fixed("UTC", 0);
        assert_eq!(utc.offset_at(0), 0);
        assert_eq!(utc.offset_at(1_788_792_059), 0);
        let three = Zone::fixed("Fixed+3", 3 * 3600);
        assert_eq!(three.civil_at(1_788_792_059).hour, 17);
    }

    /// Rubbish is refused rather than read as a zone with strange rules.
    #[test]
    fn something_that_is_not_a_timezone_file_is_refused() {
        assert_eq!(Zone::parse(b""), Err(ClockError::Truncated { wanted: 44 }));
        assert_eq!(Zone::parse(&[0u8; 100]), Err(ClockError::NotATimezone));
        let mut nearly = Vec::from(*b"TZif2");
        nearly.extend(std::iter::repeat_n(0u8, 100));
        // A header that claims transitions it does not carry.
        nearly.splice(32..36, [0, 0, 0, 200]);
        assert!(matches!(
            Zone::parse(&nearly),
            Err(ClockError::Truncated { .. } | ClockError::NotATimezone)
        ));
    }
}
