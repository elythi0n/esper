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

mod legible;
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

    // A characterisation rather than a gate: it prints a table and asserts
    // nothing, because what a quantiser rung *buys* is a question rather than
    // a promise. Behind a flag so the gate stays fast and stable.
    // Where the knee in the rate-distortion curve actually is, rather than
    // assuming the rungs that exist are the right ones.
    if args.iter().any(|a| a == "--sweep") {
        println!("{}\n", machine::describe());
        return match sweep() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("esper-bench: {e}");
                ExitCode::FAILURE
            }
        };
    }

    // **Whether text at a rung can be read**, which the ladder promises and
    // nothing measured. A characterisation like the two above: it prints the
    // recognition rate at every resolution rung and asserts nothing, because
    // where the floor *should* be is a decision and this is the evidence for
    // making it.
    if args.iter().any(|a| a == "--legibility") {
        println!("{}\n", machine::describe());
        return match legibility() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("esper-bench: {e}");
                ExitCode::FAILURE
            }
        };
    }

    // **What each rung of the CPU ladder actually costs**, which decides
    // whether stepping down it is graceful or a cliff.
    if args.iter().any(|a| a == "--ladder") {
        println!("{}\n", machine::describe());
        return match ladder() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("esper-bench: {e}");
                ExitCode::FAILURE
            }
        };
    }

    // **A soak, which is the only measurement here that takes longer than a
    // coffee.** Everything else runs for seconds; a leak, a drift or a slow
    // degradation is invisible at that length by definition.
    if let Some(mins) = after("--soak", &args) {
        println!("{}\n", machine::describe());
        let minutes: u64 = match mins.parse() {
            Ok(m) => m,
            Err(_) => {
                eprintln!("esper-bench: --soak takes a number of minutes");
                return ExitCode::FAILURE;
            }
        };
        return match soak(minutes) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("esper-bench: {e}");
                ExitCode::FAILURE
            }
        };
    }

    if args.iter().any(|a| a == "--curve") {
        println!("{}\n", machine::describe());
        return match curve() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("esper-bench: {e}");
                ExitCode::FAILURE
            }
        };
    }

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

    if verdict.unmeasured > 0 {
        eprintln!(
            "\n{} target(s) have no scenario behind them, which is not a pass.",
            verdict.unmeasured
        );
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

/// Print what each rung of the quantiser ladder buys, on both kinds of content.
///
/// **The ladder is the lever congestion control pulls**, and what it is worth
/// depends entirely on the content: on a still screen there is almost nothing
/// to quantise, so every rung looks alike; on video the quantiser is where all
/// the bits go. The rungs' documented figures were taken on a screen. This is
/// the other half, and both are printed so the difference is the point rather
/// than a footnote.
fn curve() -> Result<(), scenario::BenchError> {
    for (what, motion, width, height) in [
        (
            "a screen — 1280x720, what the rungs were characterised on",
            esper_capture::pattern::Motion::Screen,
            1280,
            720,
        ),
        (
            "video — 1920x1080, full motion",
            esper_capture::pattern::Motion::Everything,
            1920,
            1080,
        ),
    ] {
        println!("  {what}\n");
        println!(
            "    {:<10} {:>9} {:>11} {:>9} {:>12} {:>11}",
            "rung", "qp", "bitrate", "psnr", "keyframe", "encode"
        );
        println!("    {}", "-".repeat(67));
        let rungs = scenario::rate_quality(motion, width, height)?;
        let widest = rungs.iter().map(|r| r.kbit).fold(0.0f64, f64::max).max(1.0);
        let narrowest = rungs
            .iter()
            .map(|r| r.kbit)
            .fold(f64::INFINITY, f64::min)
            .max(1.0);
        for r in &rungs {
            println!(
                "    {:<10} {:>9} {:>8.0} kbit {:>6.1} dB {:>9} B {:>7.2} ms",
                r.name,
                format!("{}..{}", r.qp.0, r.qp.1),
                r.kbit,
                r.psnr,
                r.keyframe_bytes,
                r.ms
            );
        }
        println!(
            "\n    the whole ladder spans {:.1}x in bitrate\n",
            widest / narrowest
        );
    }
    Ok(())
}

/// Which end of a quantiser range binds, on each kind of content.
///
/// A rung is a range, and only one of its two ends is doing anything at a
/// time: the encoder's own choice of quantiser sits *outside* the range on
/// both kinds of content, at opposite ends. So the experiment holds one end
/// still and moves the other, which is the only way to see it.
fn sweep() -> Result<(), scenario::BenchError> {
    const FIXED_MAX: u8 = 36;
    const FIXED_MIN: u8 = 22;
    let moving_min: &[(&str, (u8, u8))] = &[
        ("14..36", (14, FIXED_MAX)),
        ("18..36", (18, FIXED_MAX)),
        ("22..36", (22, FIXED_MAX)),
        ("26..36", (26, FIXED_MAX)),
        ("30..36", (30, FIXED_MAX)),
    ];
    let moving_max: &[(&str, (u8, u8))] = &[
        ("22..32", (FIXED_MIN, 32)),
        ("22..36", (FIXED_MIN, 36)),
        ("22..40", (FIXED_MIN, 40)),
        ("22..44", (FIXED_MIN, 44)),
        ("22..48", (FIXED_MIN, 48)),
    ];
    for (what, motion, width, height) in [
        (
            "a screen — 1280x720",
            esper_capture::pattern::Motion::Screen,
            1280,
            720,
        ),
        (
            "video — 1920x1080, full motion",
            esper_capture::pattern::Motion::Everything,
            1920,
            1080,
        ),
    ] {
        println!("  {what}\n");
        for (heading, candidates) in [
            ("moving the minimum, maximum held at 36", moving_min),
            ("moving the maximum, minimum held at 22", moving_max),
        ] {
            println!("    {heading}");
            for rung in scenario::ranges(candidates, motion, width, height)? {
                println!(
                    "      qp {:<8} {:>8.0} kbit {:>6.1} dB {:>9} B {:>7.2} ms",
                    rung.name, rung.kbit, rung.psnr, rung.keyframe_bytes, rung.ms
                );
            }
            println!();
        }
    }
    println!("  The end that changes nothing is the end this content never reaches.");
    Ok(())
}

/// What a run came to.
struct Verdict {
    regressed: usize,
    unmeasured: usize,
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
    let mut unmeasured = Vec::new();
    for t in TARGETS {
        // **A target with no scenario is not a pass.** Skipping it silently is
        // how a figure gets added to the list, never run, and read as fine.
        let Some(&value) = measured.get(t.name) else {
            unmeasured.push(t.name);
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
    if !unmeasured.is_empty() {
        println!(
            "  {} target(s) were never measured: {}",
            unmeasured.len(),
            unmeasured.join(", ")
        );
    }
    if missed > 0 {
        println!(
            "  {missed} target(s) not met. That is not a failure here — a target is what the \
             product\n  should do, and some are not met yet. It is a failure of the product, \
             written down."
        );
    }

    Verdict {
        regressed,
        unmeasured: unmeasured.len(),
    }
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

/// Print whether text survives each resolution rung.
///
/// **The ladder says "never below a legibility floor" and the floor is a
/// number of pixels.** This is what that number buys: the share of characters
/// on a page of text that can still be told apart from the other ninety-four,
/// after the scaler alone and after the scaler and the codec.
///
/// It carries its own guards, because a recognition rate is exactly the kind
/// of number that can be produced by a broken measurement and believed. Full
/// resolution has to score near one — if it does not, the metric is wrong
/// rather than the codec. And a page of noise has to score near chance, or the
/// matcher is agreeing with itself.
fn legibility() -> Result<(), String> {
    const FROM: (u32, u32) = (1920, 1080);
    // **The product's own rungs**, not a list of round numbers. Measuring a
    // resolution the ladder never selects says nothing about the ladder — and
    // the first version of this measured 720, which is not a rung, and spent
    // an afternoon explaining an anomaly at a size no session ever uses.
    let rungs = esper_stream::RUNGS;

    let coarse = esper_encode::budget::Fidelity::Coarse.qp_range();
    let balanced = esper_encode::budget::Fidelity::Balanced.qp_range();

    println!("  Characters correctly told apart, out of the 95 printable ones.");
    println!("  A page of terminal text at 1920x1080, through each rung.\n");
    println!(
        "  {:>9}  {:>10}  {:>12}  {:>12}",
        "long edge", "scaled", "balanced qp", "coarse qp"
    );
    println!(
        "  {:>9}  {:>10}  {:>12}  {:>12}",
        "---------", "------", "-----------", "---------"
    );

    for &edge in rungs {
        let b = legible::at_rung(FROM, edge, balanced).map_err(|e| e.to_string())?;
        let c = legible::at_rung(FROM, edge, coarse).map_err(|e| e.to_string())?;
        println!(
            "  {:>9}  {:>9.0}%  {:>11.0}%  {:>11.0}%",
            b.long_edge,
            b.scaled * 100.0,
            b.coded * 100.0,
            c.coded * 100.0
        );
    }

    println!("\n  A reading session at full resolution — a page of text with somebody");
    println!("  typing into it, thirty frames a second for four seconds.\n");
    println!(
        "  {:>10}  {:>12}  {:>10}  {:>9}",
        "sharpness", "steady rate", "keyframe", "legible"
    );
    println!(
        "  {:>10}  {:>12}  {:>10}  {:>9}",
        "---------", "-----------", "--------", "-------"
    );
    for (name, f) in [
        ("sharp", esper_encode::budget::Fidelity::High),
        ("balanced", esper_encode::budget::Fidelity::Balanced),
        ("soft", esper_encode::budget::Fidelity::Coarse),
    ] {
        let r = legible::reading_session(FROM, f.qp_range(), 30, 4).map_err(|e| e.to_string())?;
        println!(
            "  {name:>10}  {:>9.0} kbit  {:>7.0} kB  {:>8.0}%",
            r.kbit,
            r.keyframe_kbytes,
            r.legible * 100.0
        );
    }

    println!("\n  Guards, so the numbers above mean something:");
    let control = legible::at_rung(FROM, FROM.0, balanced).map_err(|e| e.to_string())?;
    println!(
        "    full resolution, unscaled:  {:.0}%  (must be near 100, or the metric is broken)",
        control.scaled * 100.0
    );
    let chance = legible::against_noise(FROM).map_err(|e| e.to_string())?;
    println!(
        "    a page of noise:            {:.0}%  (must be near 1, which is chance over 95)",
        chance * 100.0
    );
    Ok(())
}

/// Print what each rung of the CPU governor's ladder costs.
///
/// **Graceful degradation is a claim about the *steps*, not the rungs.** A
/// ladder can be perfectly ordered and still lurch, if one step halves the
/// bill and the next takes a tenth off it. And it can fail the other way: the
/// rung below "full size, coarse" is "three quarters the size, less coarse",
/// which trades a quantiser step for a resolution step — and whether that is
/// cheaper is a measurement, not a reading of the table.
fn ladder() -> Result<(), String> {
    let steps = scenario::ladder_cost((1920, 1080)).map_err(|e| e.to_string())?;
    println!("  Every rung of the CPU ladder, on screen-like content at 1080p.\n");
    println!(
        "  {:>4}  {:>5}  {:>11}  {:>10}  {:>11}  {:>8}  {:>9}",
        "rung", "fps", "size", "quantiser", "cpu / frame", "cpu / s", "rate"
    );
    println!(
        "  {:>4}  {:>5}  {:>11}  {:>10}  {:>11}  {:>8}  {:>9}",
        "----", "-----", "-----------", "---------", "-----------", "-------", "----"
    );
    // **Judged on CPU, because that is what this ladder is for.** The
    // governor spends these rungs when the *processor* runs short; the
    // bitrate is worth printing beside them and is not the thing that has to
    // descend. Judging a CPU ladder by its bitrate is how a rung that trades
    // pixels for quality reads as a defect when it is a deliberate trade.
    let mut previous: Option<f64> = None;
    let mut climbs = Vec::new();
    for s in &steps {
        // What the rung costs a second: one frame's encode, times its own
        // frame rate. A rung that halves the frame rate halves the bill even
        // if every frame costs exactly what it did.
        let per_second = s.ms * f64::from(s.fps) / 1000.0;
        if previous.is_some_and(|was| per_second > was * 1.02) {
            climbs.push(s.index);
        }
        println!(
            "  {:>4}  {:>5}  {:>11}  {:>10}  {:>8.2} ms  {:>7.2}  {:>4.0} kbit",
            s.index,
            s.fps,
            format!("{}x{}", s.width, s.height),
            format!("{:?}", s.fidelity),
            s.ms,
            per_second,
            s.kbit
        );
        previous = Some(per_second);
    }
    println!("\n  cpu / s is a share of one core: 1.00 is a core fully spent on encoding.");
    if climbs.is_empty() {
        println!("  Every step down the ladder costs less processor than the one above it.");
    } else {
        println!(
            "  ⚠ rungs {climbs:?} cost MORE processor than the rung above them: stepping \
             down there is a cliff in the wrong direction."
        );
    }
    Ok(())
}

/// Run a long session and report whether it got worse.
///
/// **Three things a short run cannot see**, and the report names each rather
/// than producing one verdict: memory that grows, a frame rate that sags, and
/// a bitrate that creeps. The screens are plugged and unplugged throughout,
/// because rebuilding every buffer in the pipeline is the most disruptive
/// thing that happens to it and doing it once only proves it does not crash.
///
/// It asserts nothing. What counts as drift over eight hours is a judgement
/// somebody makes from the table, and a threshold invented here would be one
/// more number nobody measured.
fn soak(minutes: u64) -> Result<(), String> {
    println!("  A session for {minutes} minute(s), with its screens changing every ten seconds.\n");
    println!(
        "  {:>7}  {:>8}  {:>10}  {:>10}  {:>8}",
        "minute", "frames", "rate", "resident", "screens"
    );
    println!(
        "  {:>7}  {:>8}  {:>10}  {:>10}  {:>8}",
        "------", "------", "----", "--------", "-------"
    );
    let mut first: Option<(u64, f64, f64)> = None;
    let mut last: Option<(u64, f64, f64)> = None;
    scenario::soak(minutes, |m| {
        println!(
            "  {:>7}  {:>8}  {:>5.0} kbit  {:>7.1} MiB  {:>8}",
            m.minute, m.frames, m.kbit, m.rss_mib, m.replugs
        );
        let point = (m.frames, m.kbit, m.rss_mib);
        if first.is_none() {
            first = Some(point);
        }
        last = Some(point);
    })
    .map_err(|e| e.to_string())?;

    if let (Some(a), Some(b)) = (first, last) {
        println!("\n  First minute against last:");
        println!(
            "    frames    {:>8} → {:<8}  {:+.0}%",
            a.0,
            b.0,
            percent(a.0 as f64, b.0 as f64)
        );
        println!(
            "    rate      {:>8.0} → {:<8.0}  {:+.0}%",
            a.1,
            b.1,
            percent(a.1, b.1)
        );
        println!(
            "    resident  {:>8.1} → {:<8.1}  {:+.0}%",
            a.2,
            b.2,
            percent(a.2, b.2)
        );
        println!(
            "\n  Resident memory is the leak check and the other two are the drift check.\n  \
             Eight hours is what the target asks for; this ran {minutes}."
        );
    }
    Ok(())
}

/// Change from one number to another, as a percentage.
fn percent(from: f64, to: f64) -> f64 {
    if from.abs() < f64::EPSILON {
        return 0.0;
    }
    (to - from) / from * 100.0
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
         \x20 --curve          print what each quantiser rung buys, and assert nothing\n\
         \x20 --sweep          print the whole rate-distortion curve, to place the rungs\n\
         \x20 --legibility     print whether text can still be read at each rung\n\
         \x20 --ladder         print what each rung of the CPU ladder costs\n\
         \x20 --soak <minutes> run a session that long, moving its screens about\n\
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
