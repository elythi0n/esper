//! `esper-bench` — every performance target this project states, measured.
//!
//! # Why this exists
//!
//! `capture-and-codec.md` has a table of targets, and until this existed every
//! one of them had been checked by hand, once, by somebody who then wrote the
//! number into a document. The document itself says it: *numbers without a
//! date are wishes*. So do the findings: *a number without the machine, the
//! build and the method is not a measurement*.
//!
//! This is the method. It runs on the machine in front of you, against the
//! synthetic capturer so it needs no screen, and it says three things about
//! every number: what it is, whether the product's promise is met, and whether
//! it moved since last time.
//!
//! # Two verdicts, deliberately separate
//!
//! A **missed target** is reported and does not fail. Some are missed today,
//! and a suite that goes red for a known gap is a suite people stop reading.
//! A **regression against the baseline** fails, because that is something
//! somebody just did.
//!
//! # It refuses to measure a debug build
//!
//! An unoptimised encoder is not slow in an interesting way, and a number from
//! one would be worse than no number: it would be written down.
//!
//! ```text
//! esper-bench                       measure, and print the table
//! esper-bench --json out.json       and write a baseline
//! esper-bench --check base.json     fail on a regression against it
//! ```

mod machine;
mod scenario;
mod target;

use std::process::ExitCode;

use target::TARGETS;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{}", usage());
        return ExitCode::SUCCESS;
    }

    // **Refused rather than warned about.** A warning above a table of numbers
    // is a warning nobody reads, and the numbers get quoted anyway.
    if cfg!(debug_assertions) {
        eprintln!(
            "esper-bench: this is a debug build, and a number from one is worse than no \
             number — it would be written down.\n            Run it with `--release`."
        );
        return ExitCode::FAILURE;
    }

    let json_to = after("--json", &args);
    let check = after("--check", &args);

    let machine = machine::describe();
    println!("{machine}\n");

    let measured = match scenario::run_all() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("esper-bench: {e}");
            return ExitCode::FAILURE;
        }
    };

    // **A baseline that is not there gates nothing, and is not an error.** A
    // machine measuring itself for the first time has none, and refusing to
    // run is refusing to give somebody the numbers they asked for.
    let baseline = match check.as_ref() {
        None => None,
        Some(path) => match read_baseline(path) {
            Ok(b) => Some(b),
            Err(e) => {
                println!("  no baseline to gate against: {e}\n");
                None
            }
        },
    };

    let verdict = report(&measured, baseline.as_ref());

    if let Some(path) = json_to
        && let Err(e) = write_baseline(&path, &machine, &measured)
    {
        eprintln!("esper-bench: {e}");
        return ExitCode::FAILURE;
    }

    if verdict.regressed > 0 {
        eprintln!(
            "\n{} measurement(s) worse than the baseline by more than {:.0}%.",
            verdict.regressed,
            target::DRIFT * 100.0
        );
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// What a run came to.
struct Verdict {
    regressed: usize,
}

/// A baseline file: what a named machine last measured.
#[derive(serde::Serialize, serde::Deserialize)]
struct Baseline {
    /// The machine it was taken on. **Compared, not decoration**: a baseline
    /// from a different machine is not a baseline, and gating against one
    /// would fail every laptop that is not the one it was recorded on.
    machine: String,
    /// When, so an old baseline can be recognised as old.
    taken: String,
    /// Name to figure.
    measured: std::collections::BTreeMap<String, f64>,
}

fn read_baseline(path: &str) -> Result<Baseline, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))
}

fn write_baseline(path: &str, machine: &str, measured: &scenario::Measured) -> Result<(), String> {
    let baseline = Baseline {
        machine: machine.to_owned(),
        taken: machine::today(),
        measured: measured
            .iter()
            .map(|(k, v)| ((*k).to_owned(), *v))
            .collect(),
    };
    let text = serde_json::to_string_pretty(&baseline).map_err(|e| format!("encoding: {e}"))?;
    std::fs::write(path, text + "\n").map_err(|e| format!("{path}: {e}"))?;
    println!("\nbaseline written to {path}");
    Ok(())
}

/// Print the table, and say what it came to.
fn report(measured: &scenario::Measured, baseline: Option<&Baseline>) -> Verdict {
    // **A baseline from another machine gates the portable figures and
    // nothing else.** An encode time on somebody else's processor is not a
    // number about this one; a ratio taken on one machine, or a figure the
    // policy decides, is the same everywhere. Gating everything would fail on
    // every CI run and gating nothing would catch nothing there.
    let same_machine = baseline.is_some_and(|b| b.machine == machine::describe());
    if let Some(b) = baseline
        && !same_machine
    {
        let portable = TARGETS.iter().filter(|t| t.portable).count();
        println!(
            "  the baseline is from another machine, so only the {portable} portable \
             figure(s) are gated:\n    baseline  {}\n    here      {}\n",
            b.machine,
            machine::describe()
        );
    }

    println!(
        "  {:<22} {:>12} {:>10}  {:<10} against the baseline",
        "measurement", "measured", "target", "verdict"
    );
    println!("  {}", "-".repeat(78));

    let mut regressed = 0usize;
    let mut missed = 0usize;
    for t in TARGETS {
        let Some(&value) = measured.get(t.name) else {
            continue;
        };
        let want = t.want.map_or_else(|| "—".to_owned(), |w| show(w, t.unit));
        let verdict = match t.meets(value) {
            None => "—",
            Some(true) => "met",
            Some(false) => {
                missed += 1;
                "MISSED"
            }
        };
        let gated = same_machine || t.portable;
        let against = match baseline
            .filter(|_| gated)
            .and_then(|b| b.measured.get(t.name))
        {
            None => String::new(),
            Some(&was) => {
                if t.regressed(value, was) {
                    regressed += 1;
                    format!("WORSE than {}", show(was, t.unit))
                } else if t.improved(value, was) {
                    format!("better than {}", show(was, t.unit))
                } else {
                    format!("as before ({})", show(was, t.unit))
                }
            }
        };
        println!(
            "  {:<22} {:>12} {:>10}  {:<10} {}",
            t.name,
            show(value, t.unit),
            want,
            verdict,
            against
        );
    }

    println!();
    if missed > 0 {
        println!(
            "  {missed} target(s) not met. That is not a failure here — a target is what the \
             product\n  should do, and some are not met yet. It is a failure of the product, \
             written down."
        );
    }

    Verdict { regressed }
}

/// A number with its unit, at a sensible number of digits.
fn show(value: f64, unit: &str) -> String {
    if value >= 100.0 {
        format!("{value:.0} {unit}")
    } else if value >= 1.0 {
        format!("{value:.2} {unit}")
    } else {
        format!("{value:.3} {unit}")
    }
}

/// The value after a flag, if it is there.
fn after(flag: &str, args: &[String]) -> Option<String> {
    let at = args.iter().position(|a| a == flag)?;
    args.get(at + 1).cloned()
}

fn usage() -> String {
    let mut out = String::from(
        "usage: esper-bench [--json <file>] [--check <file>]\n\
         \n\
         Measures every performance target this project states, against the\n\
         synthetic capturer, on this machine.\n\
         \n\
         \x20 --json <file>    write what was measured, as a baseline\n\
         \x20 --check <file>   fail if anything is worse than that baseline\n\
         \n\
         A target that is not met is reported and does not fail. A measurement\n\
         worse than the baseline does fail, because that is a change somebody\n\
         just made.\n\
         \n\
         What is measured:\n",
    );
    for t in TARGETS {
        out.push_str(&format!("\n  {:<22} {}\n", t.name, t.about));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flag_takes_the_value_after_it() {
        let args: Vec<String> = ["--json", "out.json", "--check", "base.json"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        assert_eq!(after("--json", &args), Some("out.json".to_owned()));
        assert_eq!(after("--check", &args), Some("base.json".to_owned()));
        assert_eq!(after("--nothing", &args), None);
        // A flag with nothing after it is nothing, not a panic.
        let dangling: Vec<String> = vec!["--json".to_owned()];
        assert_eq!(after("--json", &dangling), None);
    }

    /// Every target is in the usage, or a person cannot find out what a name
    /// in the table means.
    #[test]
    fn the_usage_lists_every_measurement() {
        let text = usage();
        for t in TARGETS {
            assert!(text.contains(t.name), "{} is not in the usage", t.name);
        }
    }

    #[test]
    fn a_number_is_shown_at_a_useful_number_of_digits() {
        assert_eq!(show(0.055, "ms"), "0.055 ms");
        assert_eq!(show(4.56789, "ms"), "4.57 ms");
        assert_eq!(show(407.0, "kbit/s"), "407 kbit/s");
        // Not scientific notation, and not fifteen decimal places.
        assert!(!show(0.000_001, "ms").contains('e'));
    }
}
