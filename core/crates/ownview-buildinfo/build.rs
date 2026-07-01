// A build script runs at compile time on a trusted machine with no network
// input, so the no-panic rule that governs the engine does not apply: if cargo
// has not set CARGO_MANIFEST_DIR something is deeply wrong and failing loudly
// is correct.
#![allow(clippy::expect_used)]

//! Stamps the release version and build time into the binary.
//!
//! The version is read from the project's `VERSION` file — the same file the
//! Makefile stamps into the Go binary — so the two halves of OwnView can never
//! disagree about which release they are. `SOURCE_DATE_EPOCH` is honoured so
//! the build stays reproducible.

use std::path::Path;

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    // crates/ownview-buildinfo/ -> crates/ -> core/ -> project root
    let version_file = Path::new(&manifest)
        .ancestors()
        .nth(3)
        .map(|root| root.join("VERSION"));

    let version = version_file
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map_or_else(|| "v0.0.0-unknown".to_owned(), |s| s.trim().to_owned());

    // Reproducible builds: honour SOURCE_DATE_EPOCH when the build system sets it.
    let build_time = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .map_or_else(
            || std::env::var("OWNVIEW_BUILD_TIME").unwrap_or_else(|_| "unknown".to_owned()),
            format_epoch,
        );

    println!("cargo:rustc-env=OWNVIEW_VERSION={version}");
    println!("cargo:rustc-env=OWNVIEW_BUILD_TIME={build_time}");
    if let Some(p) = version_file {
        println!("cargo:rerun-if-changed={}", p.display());
    }
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    println!("cargo:rerun-if-env-changed=OWNVIEW_BUILD_TIME");
}

/// Format a Unix timestamp as RFC 3339 UTC, without pulling in a date crate for
/// one line in a build script.
fn format_epoch(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };

    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}
