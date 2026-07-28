//! What a path a peer sent is allowed to be.
//!
//! # Why this is a separate thing from opening the file
//!
//! The kernel can be made to enforce the confinement — see [`crate::Vault`] —
//! and where it can, that is the real defence. This is the other one, and it
//! exists for three reasons that survive having a good syscall.
//!
//! A name that is refused here is refused **with a reason a person can read**,
//! before anything touches a disk. A name that is merely *confined* can still
//! be a name nothing else on the fleet can hold: a Windows machine cannot
//! store `aux.txt` or `report.` or anything with a `:` in it, and a Linux host
//! that accepts them quietly builds a directory that cannot be copied
//! anywhere. And a name with a newline in it can forge a line in the session
//! log — the record that would otherwise have shown you what happened.
//!
//! # Nothing here repairs anything
//!
//! Every rule refuses. None of them normalise, strip, or substitute. **A
//! transfer that writes to a different name than the one asked for is a
//! transfer nobody can reason about**, and a sender told "no, and here is why"
//! can fix it, where a sender told nothing cannot.

/// The most bytes one component may be.
///
/// What every platform holds — `NAME_MAX` on Linux, and the same figure on
/// the others.
pub const MOST_PER_COMPONENT: usize = 255;

/// The most bytes a whole relative path may be.
///
/// Well under any platform's limit, because **the root's own length is added
/// to this** and the root is not known here. A relative path longer than a
/// kilobyte is not a path anybody typed.
pub const MOST_BYTES: usize = 1024;

/// The most components deep a path may go.
///
/// A bound on how much directory a peer can make this machine create, and far
/// past anything real.
pub const MOST_COMPONENTS: usize = 64;

/// Names Windows will not store, whatever the extension.
///
/// Refused **on every platform**, so a Linux host does not fill up with files
/// the rest of the fleet cannot receive. `CON.txt` is as reserved as `CON`,
/// which is why the check is against the part before the first dot.
const RESERVED: [&str; 26] = [
    "CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$", "COM1", "COM2", "COM3", "COM4", "COM5",
    "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8",
    "LPT9", "COM0", "LPT0",
];

/// Characters no component may contain, past the control characters.
///
/// `\` and `:` because they are separators on Windows — `a\b` sent as one
/// component would become two there, and `a:b` would be an alternate data
/// stream. The rest because Windows refuses them outright.
const FORBIDDEN: [char; 7] = ['\\', ':', '<', '>', '"', '|', '?'];

/// Characters that change what a name *looks like* without changing what it
/// is.
///
/// The bidirectional overrides are the interesting ones: `U+202E` before
/// `txt.exe` displays as `exe.txt` in every file manager there is, which is
/// the oldest trick for making somebody run something. The zero-width ones
/// make two different names look identical. Neither has any use in a filename
/// that is worth the attack.
const DECEIVING: [char; 9] = [
    '\u{200E}', '\u{200F}', '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}', '\u{2066}',
    '\u{FEFF}',
];

/// A relative path a peer named, which has been checked.
///
/// Constructing one is the only way to get one, so a function that takes a
/// `SafePath` is a function that cannot be handed an unchecked string.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SafePath {
    /// Joined with `/`, which is what the wire uses and what every platform
    /// accepts as a separator.
    text: String,
}

/// Why a name was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BadName {
    /// Nothing at all.
    Empty,
    /// It names a place rather than a place *within* somewhere.
    Absolute,
    /// A `.` or `..` component.
    Traversal,
    /// `a//b`. Refused rather than collapsed, because collapsing is repairing.
    EmptyComponent,
    /// A character that is not printable.
    Control,
    /// A character that changes what the name looks like without changing it.
    Deceiving,
    /// A character one of the platforms refuses.
    Forbidden(char),
    /// A name Windows treats as a device.
    Reserved(String),
    /// A component ending in a dot or a space, which Windows silently strips —
    /// so `evil.` and `evil` are one file there and two here.
    TrailingDotOrSpace,
    /// Longer than a component may be.
    ComponentTooLong,
    /// Longer than a path may be.
    TooLong,
    /// Deeper than a path may go.
    TooDeep,
}

impl BadName {
    /// A sentence for the person who typed the name.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::Empty => "a path with nothing in it".to_owned(),
            Self::Absolute => {
                "an absolute path — transfers are relative to the host's file directory".to_owned()
            }
            Self::Traversal => "a path with `.` or `..` in it".to_owned(),
            Self::EmptyComponent => "a path with an empty part in it".to_owned(),
            Self::Control => "a name with a control character in it".to_owned(),
            Self::Deceiving => {
                "a name with a character in it that changes how the name looks but not what it is"
                    .to_owned()
            }
            Self::Forbidden(c) => {
                format!("a name containing {c:?}, which not every machine allows")
            }
            Self::Reserved(name) => {
                format!("`{name}` is a device name on Windows and cannot be a file there")
            }
            Self::TrailingDotOrSpace => {
                "a name ending in a dot or a space, which Windows would silently change".to_owned()
            }
            Self::ComponentTooLong => {
                format!("a name longer than the {MOST_PER_COMPONENT} bytes a filename may be")
            }
            Self::TooLong => format!("a path longer than the {MOST_BYTES} bytes one may be"),
            Self::TooDeep => format!("a path more than {MOST_COMPONENTS} directories deep"),
        }
    }
}

impl std::fmt::Display for BadName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl SafePath {
    /// Check a path a peer sent.
    ///
    /// # Errors
    ///
    /// With the first rule it broke. One reason rather than all of them: the
    /// sender fixes one thing and asks again, and listing every fault of a
    /// name that was probably generated is telling an attacker what to try.
    pub fn parse(raw: &str) -> Result<Self, BadName> {
        if raw.is_empty() {
            return Err(BadName::Empty);
        }
        if raw.len() > MOST_BYTES {
            return Err(BadName::TooLong);
        }
        // A leading separator, or a drive letter, names somewhere rather than
        // somewhere within.
        if raw.starts_with('/') || raw.starts_with('\\') || is_drive_letter(raw) {
            return Err(BadName::Absolute);
        }
        // Split on `/` only. A `\` is not a separator here — it is a forbidden
        // character, caught below — because treating it as one would mean this
        // and the receiving platform disagreeing about how many components
        // there are.
        let parts: Vec<&str> = raw.split('/').collect();
        if parts.len() > MOST_COMPONENTS {
            return Err(BadName::TooDeep);
        }
        for part in &parts {
            check_component(part)?;
        }
        Ok(Self {
            text: parts.join("/"),
        })
    }

    /// The path, as the wire carries it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Each directory, then the file.
    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.text.split('/')
    }

    /// How many components there are. Always at least one.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.components().count()
    }

    /// The last component — the file itself.
    #[must_use]
    pub fn file_name(&self) -> &str {
        self.text.rsplit('/').next().unwrap_or(&self.text)
    }

    /// Everything above the file, if it is not at the top.
    #[must_use]
    pub fn parent(&self) -> Option<Self> {
        let cut = self.text.rfind('/')?;
        self.text.get(..cut).map(|text| Self {
            text: text.to_owned(),
        })
    }

    /// This path with a different last component.
    ///
    /// # Errors
    ///
    /// If the new name is not one a path may contain — the same rules, because
    /// a name this crate *makes* has to pass what a name a peer sends passes.
    pub fn with_file_name(&self, name: &str) -> Result<Self, BadName> {
        match self.parent() {
            Some(parent) => Self::parse(&format!("{}/{name}", parent.as_str())),
            None => Self::parse(name),
        }
    }
}

#[cfg(test)]
impl SafePath {
    /// A path that has **not** been checked, for tests that ask what the layer
    /// below would do if this layer were wrong.
    ///
    /// The name rules and the confinement are two defences, and the point of
    /// having two is that either alone would hold. That is only knowable by
    /// handing the second one something the first would have refused.
    pub(crate) fn unchecked(text: &str) -> Self {
        Self {
            text: text.to_owned(),
        }
    }
}

impl std::fmt::Display for SafePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

/// Whether this begins `C:` or the like.
fn is_drive_letter(raw: &str) -> bool {
    let mut bytes = raw.bytes();
    matches!((bytes.next(), bytes.next()), (Some(c), Some(b':')) if c.is_ascii_alphabetic())
}

fn check_component(part: &str) -> Result<(), BadName> {
    if part.is_empty() {
        return Err(BadName::EmptyComponent);
    }
    if part == "." || part == ".." {
        return Err(BadName::Traversal);
    }
    if part.len() > MOST_PER_COMPONENT {
        return Err(BadName::ComponentTooLong);
    }
    for c in part.chars() {
        // `is_control` covers the ASCII set and the Unicode ones, including
        // DEL and the C1 block that a terminal will act on.
        if c.is_control() {
            return Err(BadName::Control);
        }
        if DECEIVING.contains(&c) || matches!(c, '\u{2067}'..='\u{2069}') {
            return Err(BadName::Deceiving);
        }
        if FORBIDDEN.contains(&c) || c == '*' {
            return Err(BadName::Forbidden(c));
        }
    }
    if part.ends_with('.') || part.ends_with(' ') {
        return Err(BadName::TrailingDotOrSpace);
    }
    // A device name with or without an extension: `CON`, `CON.txt`, `con.TXT`.
    let stem = part.split('.').next().unwrap_or(part);
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem)) {
        return Err(BadName::Reserved(stem.to_owned()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The whole point, in one test.** Everything here is a real path that
    /// has been used to escape a directory, in this product's shape.
    #[test]
    fn nothing_that_names_somewhere_else_gets_through() {
        for escape in [
            "../etc/passwd",
            "..",
            "a/../../etc/passwd",
            "./a",
            "a/./b",
            "/etc/passwd",
            "//etc/passwd",
            "\\windows\\system32\\config\\sam",
            "C:/windows/system32",
            "c:windows",
            "..\\..\\windows",
            "a//b",
            "a/",
            "/",
        ] {
            assert!(
                SafePath::parse(escape).is_err(),
                "{escape:?} was accepted as a path"
            );
        }
    }

    /// And the ordinary paths people actually send are not caught by any of it.
    #[test]
    fn the_names_people_use_are_not_refused() {
        for fine in [
            "report.pdf",
            "reports/2026/q3.xlsx",
            "build/artefact-1.2.3.tar.gz",
            "a.b.c.d",
            "notes on the thing.md",
            "рапорт.txt",
            "日本語のファイル.txt",
            "emoji-🎛.png",
            "with-dash_and_underscore",
            ".hidden",
            ".config/esper/notes",
            "!#$%&'()+,;=@[]^`{}~.txt",
        ] {
            assert!(
                SafePath::parse(fine).is_ok(),
                "{fine:?} was refused: {:?}",
                SafePath::parse(fine).err()
            );
        }
    }

    /// **A name that only some of the fleet can hold is a name none of it
    /// should accept.** A Linux host that took these would build a directory
    /// that cannot be copied to a Windows machine, and nobody would find out
    /// until it mattered.
    #[test]
    fn a_name_windows_cannot_store_is_refused_here_too() {
        for name in [
            "CON",
            "con",
            "Con.txt",
            "NUL",
            "nul.log",
            "aux",
            "COM1",
            "com9.dat",
            "LPT1",
            "conin$",
            "CONOUT$.txt",
            "PRN.PDF",
        ] {
            assert!(
                matches!(SafePath::parse(name), Err(BadName::Reserved(_))),
                "{name:?} is a Windows device and was accepted"
            );
            // And nested, not only at the top.
            assert!(SafePath::parse(&format!("a/b/{name}")).is_err(), "{name:?}");
        }
        // `console.txt` is not `con`. A rule that refused it would be a rule
        // people work around.
        assert!(SafePath::parse("console.txt").is_ok());
        assert!(SafePath::parse("connect").is_ok());
        assert!(SafePath::parse("nullify.rs").is_ok());
    }

    /// Windows silently strips a trailing dot or space, so `evil.` and `evil`
    /// are one file there and two here — which is a way to make a transfer
    /// overwrite something it was not asked to.
    #[test]
    fn a_name_windows_would_silently_change_is_refused() {
        for name in ["evil.", "evil ", "a/b.", "trailing space ", "dots..."] {
            assert!(
                matches!(SafePath::parse(name), Err(BadName::TrailingDotOrSpace)),
                "{name:?} was accepted"
            );
        }
        // Leading is fine: nothing strips it, and `.hidden` is an ordinary
        // name on the platform this mostly runs on.
        assert!(SafePath::parse(".hidden").is_ok());
        assert!(SafePath::parse(" leading").is_ok());
    }

    /// **A filename that can forge a line in the log is not a filename.** The
    /// session log is what would have shown you what happened.
    #[test]
    fn a_name_that_could_forge_a_log_line_is_refused() {
        for name in [
            "ok\nfake log line",
            "ok\rcarriage",
            "tab\there",
            "bell\u{7}",
            "null\u{0}byte",
            "escape\u{1b}[2Jclear",
            "delete\u{7f}",
            "c1\u{9b}",
        ] {
            assert!(
                matches!(SafePath::parse(name), Err(BadName::Control)),
                "{name:?} was accepted"
            );
        }
    }

    /// **The oldest trick there is.** A right-to-left override before
    /// `txt.exe` shows as `exe.txt` in every file manager, and the two
    /// zero-width marks make two different names look like one.
    #[test]
    fn a_name_that_is_not_what_it_looks_like_is_refused() {
        for name in [
            "invoice\u{202E}txt.exe",
            "a\u{202D}b",
            "a\u{200E}b",
            "a\u{200F}b",
            "\u{FEFF}leading-bom",
            "a\u{2066}b",
            "a\u{2069}b",
        ] {
            assert!(
                matches!(SafePath::parse(name), Err(BadName::Deceiving)),
                "{name:?} was accepted"
            );
        }
        // Text that is genuinely right-to-left needs no override character to
        // be right-to-left, so nothing legitimate is caught by this.
        assert!(SafePath::parse("مستند.pdf").is_ok());
        assert!(SafePath::parse("מסמך.pdf").is_ok());
    }

    #[test]
    fn a_character_a_platform_refuses_is_refused_on_all_of_them() {
        for (name, c) in [
            ("a\\b", '\\'),
            ("stream:hidden", ':'),
            ("a<b", '<'),
            ("a>b", '>'),
            ("a\"b", '"'),
            ("a|b", '|'),
            ("a?b", '?'),
            ("a*b", '*'),
        ] {
            assert_eq!(
                SafePath::parse(name),
                Err(BadName::Forbidden(c)),
                "{name:?} was accepted"
            );
        }
    }

    #[test]
    fn the_bounds_hold_at_their_own_edges() {
        let longest = "a".repeat(MOST_PER_COMPONENT);
        assert!(SafePath::parse(&longest).is_ok());
        assert_eq!(
            SafePath::parse(&format!("{longest}a")),
            Err(BadName::ComponentTooLong)
        );

        // Exactly at the path limit, built out of legal components.
        let deep = std::iter::repeat_n("ab", 40).collect::<Vec<_>>().join("/");
        assert!(SafePath::parse(&deep).is_ok(), "{}", deep.len());
        let too_long = std::iter::repeat_n("ab", MOST_BYTES)
            .collect::<Vec<_>>()
            .join("/");
        assert_eq!(SafePath::parse(&too_long), Err(BadName::TooLong));

        let too_deep = std::iter::repeat_n("a", MOST_COMPONENTS + 1)
            .collect::<Vec<_>>()
            .join("/");
        assert_eq!(SafePath::parse(&too_deep), Err(BadName::TooDeep));
        let deep_enough = std::iter::repeat_n("a", MOST_COMPONENTS)
            .collect::<Vec<_>>()
            .join("/");
        assert!(SafePath::parse(&deep_enough).is_ok());

        assert_eq!(SafePath::parse(""), Err(BadName::Empty));
    }

    #[test]
    fn a_path_can_be_taken_apart_and_put_back_together() {
        let p = SafePath::parse("reports/2026/q3.xlsx").expect("a path");
        assert_eq!(p.as_str(), "reports/2026/q3.xlsx");
        assert_eq!(p.file_name(), "q3.xlsx");
        assert_eq!(p.depth(), 3);
        assert_eq!(
            p.components().collect::<Vec<_>>(),
            vec!["reports", "2026", "q3.xlsx"]
        );
        assert_eq!(
            p.parent().map(|p| p.as_str().to_owned()),
            Some("reports/2026".to_owned())
        );

        let top = SafePath::parse("one.txt").expect("a path");
        assert_eq!(top.parent(), None);
        assert_eq!(top.file_name(), "one.txt");

        // **A name this crate makes passes what a name a peer sends passes.**
        // The partial file's name is built with this, and a partial name that
        // escaped would be an escape by a different door.
        assert_eq!(
            p.with_file_name("q3.xlsx.part")
                .map(|p| p.as_str().to_owned()),
            Ok("reports/2026/q3.xlsx.part".to_owned())
        );
        assert!(p.with_file_name("../../evil").is_err());
        assert!(top.with_file_name("..").is_err());
    }

    /// Every refusal has something to say, or a person gets a rule number.
    #[test]
    fn every_refusal_says_something() {
        for bad in [
            BadName::Empty,
            BadName::Absolute,
            BadName::Traversal,
            BadName::EmptyComponent,
            BadName::Control,
            BadName::Deceiving,
            BadName::Forbidden('|'),
            BadName::Reserved("CON".to_owned()),
            BadName::TrailingDotOrSpace,
            BadName::ComponentTooLong,
            BadName::TooLong,
            BadName::TooDeep,
        ] {
            assert!(!bad.message().is_empty(), "{bad:?}");
            assert_eq!(bad.to_string(), bad.message());
        }
    }
}
