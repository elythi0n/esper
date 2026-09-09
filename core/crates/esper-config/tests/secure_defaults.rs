#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The defaults that are security decisions, and the document that states them.
//!
//! # Why this is a test and not a review
//!
//! The registry knows which settings exist and what they default to. A
//! document knows which of those defaults were *decided* rather than merely
//! chosen, and why. Neither knows the other, and the direction they drift is
//! always the same: a setting is added to the code, its default is sensible to
//! whoever added it, and the document keeps describing the list from before.
//!
//! Somebody deciding whether to deploy this reads the document.
//!
//! **This is the check that was not there.** The secure set was verified by a
//! test that hand-listed five settings it already knew existed, so a setting
//! named in the decision and missing from the product passed in silence — and
//! seven were in exactly that state.
//!
//! # Both directions, and why the second one matters more
//!
//! A row here for a setting that does not exist is the worse failure: it reads
//! as a promise the product keeps, and there is nothing to notice it.

use std::collections::BTreeSet;
use std::path::Path;

/// The product's own security document. **Not the planning tree's** — the
/// product may not name a decision record, and a reader of this repository
/// cannot follow one. See `check-isolation`.
fn threat_model() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("crate is at <root>/core/crates/<name>")
        .join("docs/threat-model.md");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// The settings the table names, read out of the table itself.
fn named_in_the_table(text: &str) -> BTreeSet<String> {
    let start = text
        .find("### The defaults that are security decisions")
        .expect("the threat model no longer has the secure-defaults table; this check is stale");
    let body = &text[start..];
    let end = body
        .find("\n**Two things are not settings")
        .unwrap_or(body.len());
    let mut found = BTreeSet::new();
    for line in body[..end].lines() {
        if !line.starts_with('|') {
            continue;
        }
        let Some(first) = line.split('|').nth(1) else {
            continue;
        };
        // A row may name two settings — `audit.retain` / `audit.max_size`.
        for word in first.split('/') {
            let key = word.trim().trim_matches('`').trim();
            if key.contains('.') && !key.contains(' ') {
                found.insert(key.to_owned());
            }
        }
    }
    found
}

/// Every setting the table names exists.
#[test]
fn the_table_names_settings_this_product_has() {
    let text = threat_model();
    let named = named_in_the_table(&text);
    // A floor: a table that parsed as nothing would agree with everything.
    assert!(
        named.len() >= 10,
        "the secure-defaults table parsed as {} row(s), which is not this document",
        named.len()
    );

    for key in &named {
        assert!(
            esper_config::SETTINGS.iter().any(|s| s.key == *key),
            "the threat model names {key} as a security default and this product has no such \
             setting — a row that reads as a promise the product keeps"
        );
    }
}

/// And the other direction: a setting whose default is a security decision is
/// in the table.
///
/// **Named here rather than derived**, because "is this default a security
/// decision" is a judgement and not a property of the type. What the list
/// costs is that somebody adding such a setting has to add it in two places;
/// what it buys is that they cannot add it in one.
#[test]
fn a_security_default_is_in_the_table() {
    let text = threat_model();
    let named = named_in_the_table(&text);

    for key in [
        "broker.address",
        "broker.key",
        "host.serve_without_indicator",
        "host.privacy_screen",
        "host.lock_local_input",
        "host.allow_asking",
        "host.lock_after_session",
        "policy.deny_when_locked",
        "session.idle_timeout",
        "files.enabled",
        "files.root",
        "forward.allow",
        "audit.retain",
        "audit.max_size",
        "discovery.mdns",
    ] {
        assert!(
            esper_config::SETTINGS.iter().any(|s| s.key == key),
            "{key} is listed here as a security default and is not a setting — this list is stale"
        );
        assert!(
            named.contains(key),
            "{key}'s default is a security decision and the threat model does not name it"
        );
    }
}

/// The two things that are deliberately *not* settings are said to be, because
/// "there is no switch" is a stronger claim than any default and is the kind
/// that quietly acquires one.
#[test]
fn what_is_deliberately_not_a_setting_is_said_to_be_not_a_setting() {
    let text = threat_model();
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        flat.contains("The decoder's sandbox has no switch at all"),
        "the threat model does not say the sandbox is not a setting"
    );
    // And it is true: nothing in the registry turns it off.
    for setting in esper_config::SETTINGS {
        assert!(
            !setting.key.contains("sandbox"),
            "{} exists, and the threat model says there is no such switch",
            setting.key
        );
    }
    // The same for the per-device permissions an earlier design had as global
    // flags.
    for gone in ["clipboard.inbound", "clipboard.outbound"] {
        assert!(
            !esper_config::SETTINGS.iter().any(|s| s.key == gone),
            "{gone} is back as a setting, and what a device may do is a permission"
        );
    }
}
