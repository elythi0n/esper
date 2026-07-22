//! Reading the file: `key = value`, one per line, `#` for a comment.
//!
//! A subset of TOML — dotted keys with scalar values — so a file written by
//! this program is one a real TOML parser reads, if this ever needs sections,
//! arrays or dates. What it refuses is as important as what it accepts: a
//! line it does not understand is an error with a line number, because a
//! configuration file that silently ignores half of itself is how somebody
//! ends up running without the setting they thought they had set.

/// A line that could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{what}")]
pub struct ParseError {
    /// Which line, counting from one, the way an editor does.
    pub line: usize,
    /// What is wrong with it.
    pub what: String,
}

/// Every `key = value` in the text, with the line each came from.
///
/// Values come back as raw text; what they mean is the setting's business.
pub fn parse(text: &str) -> Result<Vec<(String, String, usize)>, ParseError> {
    let mut out = Vec::new();

    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        let content = strip_comment(raw).trim();
        if content.is_empty() {
            continue;
        }

        // A section header is valid TOML and this subset has no use for one.
        // Refused rather than ignored: somebody who wrote `[host]` expects the
        // keys under it to be `host.*`, and quietly reading them as top-level
        // keys would be a file that means something different from what it
        // says.
        if content.starts_with('[') {
            return Err(ParseError {
                line,
                what: format!(
                    "sections are not used here — write the whole key instead, \
                     as in `host.listen = \"…\"` ({content})"
                ),
            });
        }

        let Some((key, value)) = content.split_once('=') else {
            return Err(ParseError {
                line,
                what: format!("expected `key = value`, found {content:?}"),
            });
        };

        let key = key.trim();
        if key.is_empty() {
            return Err(ParseError {
                line,
                what: "a value with no key".to_owned(),
            });
        }
        if !key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
        {
            return Err(ParseError {
                line,
                what: format!("{key:?} is not a setting's name"),
            });
        }

        let value = unquote(value.trim()).map_err(|what| ParseError { line, what })?;
        out.push((key.to_owned(), value, line));
    }

    Ok(out)
}

/// Everything before an unquoted `#`.
///
/// Inside quotes a `#` is a character, not a comment: an address, a name or a
/// key may contain one, and truncating a value at it would be a setting
/// silently changed by its own punctuation.
fn strip_comment(line: &str) -> &str {
    let mut in_quotes = false;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_quotes = !in_quotes,
            '#' if !in_quotes => return line.get(..i).unwrap_or(""),
            _ => {}
        }
    }
    line
}

/// A value with its quotes removed, if it had any.
///
/// Quoted or bare are both accepted, because both are what somebody writes.
/// An *unterminated* quote is an error: it means the rest of the line is not
/// what it looks like.
fn unquote(value: &str) -> Result<String, String> {
    if let Some(rest) = value.strip_prefix('"') {
        return match rest.strip_suffix('"') {
            Some(inner) if !inner.ends_with('\\') => Ok(inner.to_owned()),
            _ => Err(format!("a quote that is never closed: {value}")),
        };
    }
    if value.is_empty() {
        return Err("a key with no value".to_owned());
    }
    Ok(value.to_owned())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn a_file_is_keys_values_and_comments() {
        let text = "\
# Where to listen.
host.listen = \"0.0.0.0:7645\"

discovery.mdns = false   # chatty on a shared network
host.fps = 30
";
        let pairs = parse(text).unwrap();
        assert_eq!(
            pairs,
            vec![
                ("host.listen".to_owned(), "0.0.0.0:7645".to_owned(), 2),
                ("discovery.mdns".to_owned(), "false".to_owned(), 4),
                ("host.fps".to_owned(), "30".to_owned(), 5),
            ]
        );
    }

    #[test]
    fn a_hash_inside_quotes_is_a_character() {
        // An address, a name or a key may contain one, and truncating a value
        // at its own punctuation is a setting silently changed.
        let pairs = parse("device.name = \"the # machine\"").unwrap();
        assert_eq!(pairs.first().map(|p| p.1.as_str()), Some("the # machine"));
    }

    #[test]
    fn a_line_that_is_not_understood_says_which_line() {
        // Ignoring it is how somebody ends up running without the setting
        // they thought they had set.
        let e = parse("host.listen = \"x\"\nthis is not a setting\n").unwrap_err();
        assert_eq!(e.line, 2);
        assert!(e.what.contains("key = value"), "{}", e.what);
    }

    #[test]
    fn a_section_header_is_refused_rather_than_ignored() {
        let e = parse("[host]\nlisten = \"x\"\n").unwrap_err();
        assert_eq!(e.line, 1);
        assert!(e.what.contains("whole key"), "{}", e.what);
    }

    #[test]
    fn an_unterminated_quote_is_an_error() {
        let e = parse("device.name = \"unfinished\n").unwrap_err();
        assert!(e.what.contains("never closed"), "{}", e.what);
    }

    #[test]
    fn a_key_with_no_value_and_a_value_with_no_key_are_both_refused() {
        assert!(parse("host.fps =\n").is_err());
        assert!(parse("= 30\n").is_err());
    }

    #[test]
    fn an_empty_file_is_a_valid_one() {
        // An empty file produces a working system, so an empty file is not an
        // error.
        assert!(parse("").unwrap().is_empty());
        assert!(parse("\n\n# only a comment\n").unwrap().is_empty());
    }
}
