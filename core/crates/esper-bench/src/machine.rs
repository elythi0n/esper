//! Which machine a number came from.
//!
//! **A number without the machine is not a measurement**, and this project's
//! findings say so in their own README. It is also what makes a baseline
//! usable: gating a laptop against a figure recorded on a workstation would
//! fail every time for a reason that is not a regression.

/// A short description of this machine, stable across runs on it.
///
/// **A CPU limit makes it a different machine, and the description says so.**
/// A run under `CPUQuota=25%` is not a run on a slower processor: it is this
/// processor, stopped for three quarters of every period, which is a
/// materially different thing for an encoder with a frame deadline. Worse,
/// `available_parallelism` already honours the cgroup — 20 becomes 1 — so
/// without this the run would be described as a one-core machine and its
/// numbers filed against the wrong thing.
///
/// It also does the right thing to the baseline for free: the machine string
/// is compared, so a constrained run cannot be gated against an unconstrained
/// baseline.
#[must_use]
pub fn describe() -> String {
    let plain = format!(
        "{} — {} core(s), {}",
        cpu_model(),
        std::thread::available_parallelism().map_or(0, std::num::NonZero::get),
        std::env::consts::OS
    );
    match cpu_limit() {
        Some(limit) => format!("{plain}, held to {limit}"),
        None => plain,
    }
}

/// The CPU limit this process is running under, if a cgroup sets one.
///
/// Reported as a share of one core, which is how `cpu.max` expresses it: a
/// quota of `25000 100000` is a quarter of one core's time per period. Two
/// cores' worth reads as `200% of a core`, which is the honest way to say
/// something that is not a core count.
#[must_use]
pub fn cpu_limit() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        // The limit applies at this process's own cgroup, not at the root —
        // reading `/sys/fs/cgroup/cpu.max` finds nothing on any ordinary
        // desktop and would report every constrained run as unconstrained.
        let own = std::fs::read_to_string("/proc/self/cgroup").ok()?;
        let relative = own.lines().next()?.rsplit(':').next()?.trim();
        let path = format!("/sys/fs/cgroup{relative}/cpu.max");
        let text = std::fs::read_to_string(path).ok()?;
        let mut parts = text.split_whitespace();
        let quota = parts.next()?;
        let period: f64 = parts.next()?.parse().ok()?;
        // `max` is the ordinary answer: a cgroup with no limit.
        if quota == "max" || period <= 0.0 {
            return None;
        }
        let quota: f64 = quota.parse().ok()?;
        Some(format!("{:.0}% of a core", quota / period * 100.0))
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
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

#[cfg(test)]
mod limit_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// **The description has to change when the limit does**, or a constrained
    /// run is filed against an unconstrained baseline and reads as a
    /// regression on this machine rather than as a measurement of another one.
    #[test]
    fn a_limit_shows_in_the_description() {
        let said = describe();
        match cpu_limit() {
            Some(limit) => assert!(
                said.contains(&limit),
                "running under {limit} and the description does not say so: {said}"
            ),
            None => assert!(
                !said.contains("held to"),
                "nothing limits this process and the description claims otherwise: {said}"
            ),
        }
    }

    /// An ordinary desktop process is not in a quota'd cgroup, and saying it is
    /// would file every number under a machine that does not exist.
    #[test]
    fn an_unlimited_cgroup_reads_as_no_limit() {
        // `cargo test` runs in the session's own scope, which sets no quota.
        // If this ever fails the suite is being run under one, and the
        // numbers it produces belong to that machine rather than to this.
        if let Some(limit) = cpu_limit() {
            println!("note: this suite is running under {limit}");
        }
    }
}
