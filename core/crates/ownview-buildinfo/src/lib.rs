//! Release identity, stamped at build time.
//!
//! This lives in one crate rather than a `build.rs` per binary so the two can
//! never disagree — and so the calendar arithmetic in the build script is
//! written and reviewed once.
//!
//! The version is read from the project's `VERSION` file, which is the same
//! file the Makefile stamps into the Go binary. That is deliberate: `ownview`
//! and `ownview-engine` must agree about which release they are, and the
//! cheapest way to guarantee that is a single source.

/// The release version, e.g. `v0.0.0`.
pub const VERSION: &str = env!("OWNVIEW_VERSION");

/// Build time as RFC 3339 UTC, or `"unknown"` when not stamped.
///
/// Honours `SOURCE_DATE_EPOCH`, so a reproducible build produces a
/// reproducible value.
pub const BUILD_TIME: &str = env!("OWNVIEW_BUILD_TIME");

/// One line identifying this build, for `--version` output and logs.
#[must_use]
pub fn summary(binary: &str) -> String {
    format!("{binary} {VERSION} (built {BUILD_TIME})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_stamped_from_the_version_file() {
        assert!(!VERSION.is_empty());
        assert!(
            VERSION.starts_with('v'),
            "VERSION file should hold a v-prefixed version, got {VERSION:?}"
        );
    }

    #[test]
    fn build_time_is_present() {
        assert!(!BUILD_TIME.is_empty());
    }

    #[test]
    fn summary_names_the_binary() {
        assert!(summary("ownview-engine").starts_with("ownview-engine v"));
    }
}
