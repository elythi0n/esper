//! A setting's value, once it has been read and checked.

use std::borrow::Cow;
use std::fmt;

/// What a setting holds.
///
/// Checked when it is read rather than when it is used: a bad address in a
/// file should stop the program at startup with the line number, not fail an
/// hour later when somebody connects.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// Text, including addresses and keys, which are validated by their kind.
    ///
    /// Borrowed or owned, because a default is a literal in a table that has
    /// to be constructible in a `static` and a value read from a file is not.
    Text(Cow<'static, str>),
    /// True or false.
    Flag(bool),
    /// A whole number.
    Whole(u64),
    /// A number with a fraction, for a CPU budget in cores.
    Fraction(f64),
    /// Deliberately nothing. Distinct from a missing setting: a value somebody
    /// set to `none` is a decision, and the program reports it as one.
    Nothing,
}

impl Value {
    /// The text, if it is text.
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text(t) => Some(t.as_ref()),
            _ => None,
        }
    }

    /// Text, however it was written.
    #[must_use]
    pub fn owned_text(text: impl Into<String>) -> Self {
        Self::Text(Cow::Owned(text.into()))
    }

    /// The flag, if it is one.
    #[must_use]
    pub const fn flag(&self) -> Option<bool> {
        match self {
            Self::Flag(b) => Some(*b),
            _ => None,
        }
    }

    /// The number, if it is a whole one.
    #[must_use]
    pub const fn whole(&self) -> Option<u64> {
        match self {
            Self::Whole(n) => Some(*n),
            _ => None,
        }
    }

    /// The number, whole or not.
    #[must_use]
    pub fn number(&self) -> Option<f64> {
        match self {
            Self::Whole(n) => Some(*n as f64),
            Self::Fraction(f) => Some(*f),
            _ => None,
        }
    }

    /// Whether this is the deliberate absence of a value.
    #[must_use]
    pub const fn is_nothing(&self) -> bool {
        matches!(self, Self::Nothing)
    }
}

impl fmt::Display for Value {
    /// As it would be written in the file, so `config show` output can be
    /// pasted into one.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text(t) => write!(f, "{t:?}"),
            Self::Flag(b) => write!(f, "{b}"),
            Self::Whole(n) => write!(f, "{n}"),
            Self::Fraction(x) => write!(f, "{x}"),
            Self::Nothing => write!(f, "\"none\""),
        }
    }
}
