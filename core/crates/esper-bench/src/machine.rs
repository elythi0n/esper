//! Which machine a number came from.
//!
//! **A number without the machine is not a measurement**, and this project's
//! findings say so in their own README. It is also what makes a baseline
//! usable: gating a laptop against a figure recorded on a workstation would
//! fail every time for a reason that is not a regression.

/// A short description of this machine, stable across runs on it.
#[must_use]
pub fn describe() -> String {
    format!(
        "{} — {} core(s), {}",
        cpu_model(),
        std::thread::available_parallelism().map_or(0, std::num::NonZero::get),
        std::env::consts::OS
    )
}

/// The processor's own name for itself.
fn cpu_model() -> String {
    #[cfg(target_os = "linux")]
    {
        if let Ok(text) = std::fs::read_to_string("/proc/cpuinfo") {
            for line in text.lines() {
                if let Some(name) = line.strip_prefix("model name")
                    && let Some((_, value)) = name.split_once(':')
                {
                    return value.trim().to_owned();
                }
            }
        }
    }
    format!("an unnamed {} processor", std::env::consts::ARCH)
}

/// Today, as a date a finding would be filed under.
///
/// Derived from the clock rather than from a date library: a baseline needs to
/// say roughly when it was taken, and a whole calendar dependency for that
/// would be a dependency for a string.
#[must_use]
pub fn today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let days = secs / 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Days since the epoch to a calendar date.
///
/// Howard Hinnant's `civil_from_days`, which is the standard way to do this
/// without a library and is a few lines of arithmetic rather than a
/// dependency.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (
        if m <= 2 { y + 1 } else { y },
        u32::try_from(m).unwrap_or(1),
        u32::try_from(d).unwrap_or(1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_machine_describes_itself_the_same_way_twice() {
        let once = describe();
        assert_eq!(once, describe());
        assert!(!once.is_empty());
        assert!(once.contains("core"));
    }

    /// Against dates everybody agrees on, so this is checked against the
    /// calendar rather than against itself.
    #[test]
    fn the_date_arithmetic_is_the_calendar() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(1), (1970, 1, 2));
        assert_eq!(civil_from_days(59), (1970, 3, 1));
        // A leap day, which is the thing this kind of arithmetic gets wrong.
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(civil_from_days(11_017), (2000, 3, 1));
        // And a century that is not a leap year.
        assert_eq!(civil_from_days(-25_508), (1900, 3, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
    }

    #[test]
    fn today_reads_like_a_date() {
        let text = today();
        assert_eq!(text.len(), 10, "{text}");
        assert_eq!(text.matches('-').count(), 2, "{text}");
        assert!(text.starts_with("20"), "{text}");
    }
}
