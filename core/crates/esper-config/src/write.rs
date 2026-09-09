//! Changing one setting in the file, and leaving the rest of it alone.
//!
//! # Why this edits lines rather than rewriting the file
//!
//! A configuration file is somebody's: it has their comments in it, their
//! order, and the settings they chose to write down. A writer that parsed the
//! file into a map and printed it back would produce a *correct* file that had
//! lost every comment and reordered every line — which is the kind of help
//! nobody asked for, and which makes the next diff unreadable.
//!
//! So this finds the line the key is on and changes the value on it. Every
//! other byte of the file is the byte that was there.
//!
//! # Why these are functions on text
//!
//! Nothing here opens a file. The interesting cases — a key that is absent, a
//! key written twice, a value with a comment after it, a file with no trailing
//! newline — are all about the text, and a test that has to lay out a
//! directory to reach them is a test nobody writes enough of.

use crate::Value;
use crate::parse::strip_comment;

/// What a change did, for the sentence a person is told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Changed {
    /// How many lines were rewritten.
    pub lines: usize,
    /// Whether the key was not in the file and has been added.
    pub added: bool,
}

/// Set `key` to `value`, adding the line if it is not there.
///
/// **Every line for that key, not only the one that wins.** The loader takes
/// the last assignment, so a file with the key twice means what its last line
/// says — and changing only that one would leave a file whose earlier line
/// contradicts it. A reader of such a file has to know the precedence rule to
/// know what their own machine does, which is not a thing to make somebody
/// know.
#[must_use]
pub fn set_in(text: &str, key: &str, value: &Value) -> (String, Changed) {
    let written = format!("{key} = {value}");
    let mut lines: Vec<String> = text.lines().map(ToOwned::to_owned).collect();
    let mut changed = 0usize;

    for line in &mut lines {
        let Some(comment) = assigns(line, key) else {
            continue;
        };
        // The comment somebody wrote after the value is theirs, and is about
        // the setting rather than about the value it had.
        *line = format!("{written}{comment}");
        changed = changed.saturating_add(1);
    }

    if changed > 0 {
        return (
            rejoin(&lines, text),
            Changed {
                lines: changed,
                added: false,
            },
        );
    }

    lines.push(written);
    (
        rejoin(&lines, text),
        Changed {
            lines: 1,
            added: true,
        },
    )
}

/// Take `key` out, so the default decides again.
///
/// Removes the whole line, including a comment written after the value on it:
/// that comment is about the assignment being removed. A comment on a line of
/// its own is left, because it may belong to the setting below.
#[must_use]
pub fn unset_in(text: &str, key: &str) -> (String, Changed) {
    let mut kept: Vec<String> = Vec::new();
    let mut removed = 0usize;
    for line in text.lines() {
        if assigns(line, key).is_some() {
            removed = removed.saturating_add(1);
            continue;
        }
        kept.push(line.to_owned());
    }
    (
        rejoin(&kept, text),
        Changed {
            lines: removed,
            added: false,
        },
    )
}

/// Whether this line assigns `key`, and everything after the value on it.
///
/// The tail comes back so it can be kept: it is the only part of the line that
/// is not being replaced. It starts at the end of the *value*, not at the `#`,
/// so the spacing somebody used to line their comments up survives.
fn assigns<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let content = strip_comment(line);
    let (name, _) = content.split_once('=')?;
    if name.trim() != key {
        return None;
    }
    Some(line.get(content.trim_end().len()..).unwrap_or(""))
}

/// Put the lines back, ending the file the way it ended before.
///
/// **A file that had a trailing newline keeps one, and a file that did not is
/// given one.** `lines()` does not say which it was, and a file whose last
/// line has no newline is a file the next `>>` appends to the middle of.
fn rejoin(lines: &[String], _was: &str) -> String {
    let mut out = lines.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    /// **The rest of the file survives.** A writer that rewrote the file from
    /// a parsed map would pass every test about the value and lose every
    /// comment somebody wrote, which is the thing that makes a configuration
    /// file theirs.
    #[test]
    fn everything_that_was_not_the_setting_is_untouched() {
        let before = "\
# What this machine is called.
device.name = \"elythion\"

# Serving.
host.fps = 30   # thirty is plenty
host.budget = 1
";
        let (after, changed) = set_in(before, "host.fps", &Value::Whole(60));
        assert_eq!(
            changed,
            Changed {
                lines: 1,
                added: false
            }
        );
        assert_eq!(
            after,
            "\
# What this machine is called.
device.name = \"elythion\"

# Serving.
host.fps = 60   # thirty is plenty
host.budget = 1
"
        );
    }

    /// A key that is not there is added, and the file still parses.
    #[test]
    fn a_setting_that_was_never_written_is_added() {
        let (after, changed) = set_in("device.name = \"elythion\"\n", "host.fps", &Value::Whole(45));
        assert!(changed.added);
        assert_eq!(after, "device.name = \"elythion\"\nhost.fps = 45\n");
        let read = crate::parse(&after).expect("what was written is what is read");
        assert_eq!(read.len(), 2);
        let (key, value, _) = read.last().expect("the line just written");
        assert_eq!(key, "host.fps");
        assert_eq!(value, "45");
    }

    /// **Every assignment, not only the last.** The loader takes the last one,
    /// so a file with the key twice would otherwise end up saying two
    /// different things — one of which is what the machine does and one of
    /// which is what somebody reads.
    #[test]
    fn a_key_written_twice_is_changed_everywhere() {
        let before = "host.fps = 30\nhost.budget = 1\nhost.fps = 15\n";
        let (after, changed) = set_in(before, "host.fps", &Value::Whole(60));
        assert_eq!(changed.lines, 2);
        assert_eq!(after, "host.fps = 60\nhost.budget = 1\nhost.fps = 60\n");
    }

    /// Text is quoted the way the file wants it, so what is written is what
    /// comes back.
    #[test]
    fn a_value_is_written_the_way_the_file_reads_it() {
        for (value, want) in [
            (
                Value::owned_text("0.0.0.0:7645"),
                "host.listen = \"0.0.0.0:7645\"\n",
            ),
            (Value::Flag(true), "host.listen = true\n"),
            (Value::Whole(7), "host.listen = 7\n"),
            (Value::Nothing, "host.listen = \"none\"\n"),
        ] {
            let (after, _) = set_in("", "host.listen", &value);
            assert_eq!(after, want, "{value:?}");
            crate::parse(&after).expect("a file this wrote is a file this reads");
        }
    }

    /// A `#` inside a value is part of the value, not the start of a comment —
    /// so a line carrying one is still recognised as the key's.
    #[test]
    fn a_hash_inside_a_value_does_not_hide_the_line() {
        let before = "device.name = \"elythion #2\"\n";
        let (after, changed) = set_in(before, "device.name", &Value::owned_text("plum"));
        assert_eq!(changed.lines, 1, "the line was not found: {after}");
        assert_eq!(after, "device.name = \"plum\"\n");
    }

    /// Taking one off leaves the file, and the comment lines in it, alone.
    #[test]
    fn unsetting_removes_the_assignment_and_nothing_else() {
        let before = "# Serving.\nhost.fps = 30 # a note\nhost.budget = 1\n";
        let (after, changed) = unset_in(before, "host.fps");
        assert_eq!(changed.lines, 1);
        assert_eq!(after, "# Serving.\nhost.budget = 1\n");

        // And one that was never there changes nothing at all.
        let (same, none) = unset_in(&after, "host.fps");
        assert_eq!(none.lines, 0);
        assert_eq!(same, after);
    }

    /// A file with no trailing newline gets one, so the next line added is a
    /// line rather than the end of the last one.
    #[test]
    fn the_file_ends_with_a_newline_either_way() {
        let (after, _) = set_in("host.fps = 30", "host.budget", &Value::Whole(2));
        assert_eq!(after, "host.fps = 30\nhost.budget = 2\n");
        assert!(after.ends_with('\n'));
    }
}
