//! Pairing codes: the short secret a person carries from one screen to another.
//!
//! # The alphabet
//!
//! A code is read off a screen and typed, or read aloud down a phone line, by
//! someone who did not choose it and will not see it again. So the alphabet
//! excludes every pair that is confusable in either channel:
//!
//! | Excluded | Confused with | Where |
//! |---|---|---|
//! | `0` | `O` | on screen |
//! | `1` | `I`, `L` | on screen |
//! | `2` | `Z` | spoken and on screen |
//! | `5` | `S` | spoken and on screen |
//! | `8` | `B` | spoken |
//! | `U` | `V` | spoken |
//!
//! What remains is 24 symbols, which is not a power of two — and that matters.
//! Generating a code by taking a random byte modulo 24 would make the first 16
//! symbols slightly more likely than the last 8, costing entropy for nothing.
//! [`PairingCode::generate`] rejects out-of-range draws instead.
//!
//! # Why 40 bits is enough
//!
//! Only because the code is used inside a PAKE. A PAKE gives an attacker
//! exactly one online guess per attempt and leaks nothing that permits an
//! offline search, so the code has to survive three guesses in sixty seconds —
//! not unlimited offline guessing forever.
//!
//! A 40-bit secret behind an offline-searchable construction is worthless: a
//! GPU covers 2^40 hashes in about two minutes. The safety of a short code is a
//! property of the protocol around it, never of the code.

use core::fmt;

use rand_core::{OsRng, RngCore};
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, ZeroizeOnDrop};

/// The symbols a code may contain. 24 of them, deliberately not 32.
///
/// Frozen: changing it changes which codes are typeable, and a user reading a
/// code from an older build to a newer one must still be understood.
const ALPHABET: &[u8; 24] = b"34679ACDEFGHJKMNPQRTVWXY";

/// Symbols per group.
const GROUP: usize = 4;

/// Number of groups. `XXXX-XXXX`.
const GROUPS: usize = 2;

/// Total symbols in a code.
pub const CODE_LEN: usize = GROUP * GROUPS;

/// Approximate entropy of a code, in bits: `8 * log2(24)`.
///
/// Reported rather than assumed, because the alphabet size is not a power of
/// two and the obvious arithmetic gets it wrong.
pub const CODE_BITS: f64 = 36.68;

/// A pairing code.
///
/// Zeroized on drop. It is a secret for sixty seconds and there is no reason
/// for it to outlive that in memory.
#[derive(Clone, ZeroizeOnDrop)]
pub struct PairingCode {
    symbols: [u8; CODE_LEN],
}

impl fmt::Debug for PairingCode {
    /// Never prints the code.
    ///
    /// The code appears on the host's physical screen and nowhere else — not in
    /// a log at any level, not in an error, not in a panic message. A derived
    /// `Debug` would put it in all four.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PairingCode(<redacted>)")
    }
}

/// Why a typed code was not accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CodeError {
    /// Wrong number of symbols.
    #[error("a pairing code has {CODE_LEN} characters; this has {found}")]
    WrongLength { found: usize },

    /// A symbol that is not in the alphabet and has no unambiguous source.
    ///
    /// Refused rather than guessed at. A wrong guess produces a code that
    /// parses and then fails against the host, spending one of three attempts;
    /// this error costs nothing but a second look.
    #[error(
        "'{found}' is never part of a pairing code — codes contain no O, 0, I, 1, L, \
         S, 5, B, 8, Z or 2, because those are too easily misread or misheard"
    )]
    BadSymbol { found: char },
}

impl PairingCode {
    /// Generate a fresh code from the operating system's RNG.
    ///
    /// Uses rejection sampling. Taking a byte modulo 24 would bias the first 16
    /// symbols upward — a small bias, and free to avoid.
    #[must_use]
    pub fn generate() -> Self {
        let mut symbols = [0u8; CODE_LEN];
        let mut buf = [0u8; 32];
        let mut filled = 0usize;
        let mut cursor = buf.len();

        while filled < CODE_LEN {
            if cursor >= buf.len() {
                OsRng.fill_bytes(&mut buf);
                cursor = 0;
            }
            let draw = buf.get(cursor).copied().unwrap_or(0);
            cursor += 1;

            // 240 is the largest multiple of 24 below 256. Anything above it
            // would land unevenly, so it is discarded rather than folded.
            if draw >= 240 {
                continue;
            }
            let idx = usize::from(draw) % ALPHABET.len();
            if let (Some(slot), Some(sym)) = (symbols.get_mut(filled), ALPHABET.get(idx)) {
                *slot = *sym;
                filled += 1;
            }
        }

        buf.zeroize();
        Self { symbols }
    }

    /// Parse a code a person typed.
    ///
    /// Forgiving about presentation, strict about content: case is folded,
    /// separators and whitespace are ignored, and two characters with exactly
    /// one plausible source are mapped. Everything else is refused.
    ///
    /// The asymmetry is deliberate. Forgiving presentation costs nothing.
    /// Forgiving *content* by guessing at an ambiguous character produces a code
    /// that parses and then fails against the host — spending one of three
    /// attempts on a typo. Refusing locally is free.
    pub fn parse(input: &str) -> Result<Self, CodeError> {
        let mut symbols = [0u8; CODE_LEN];
        let mut n = 0usize;

        for ch in input.chars() {
            if ch.is_whitespace() || ch == '-' || ch == '_' || ch == '.' {
                continue;
            }
            let upper = ch.to_ascii_uppercase();

            // Map an excluded character onto what was meant — but **only where
            // exactly one alphabet symbol is a plausible source.**
            //
            // The temptation is to remap every excluded character to something
            // nearby. That is a mistake, and an expensive one: a silent remap
            // onto the wrong symbol produces a code that *parses* and then
            // fails against the host, consuming one of only three attempts. A
            // local parse error costs nothing — the user simply looks again.
            //
            // So the bar is uniqueness, not helpfulness:
            //
            // - `O`/`0` -> `Q`. `Q` is the only round glyph in the alphabet, and
            //   nothing else could have been misread as an O or a zero.
            // - `U` -> `V`. Adjacent visually and near-identical spoken, and
            //   `V` is the only candidate.
            //
            // Everything else excluded — `1`/`I`/`L`, `2`/`Z`, `5`/`S`, `8`/`B` —
            // has either no plausible source or more than one (`1` could be a
            // misread `7` as easily as a `J`), so it is refused and the error
            // says the alphabet never contains it.
            let mapped = match upper {
                'O' | '0' => 'Q',
                'U' => 'V',
                other => other,
            };

            let byte = u8::try_from(mapped).map_err(|_| CodeError::BadSymbol { found: ch })?;
            if !ALPHABET.contains(&byte) {
                return Err(CodeError::BadSymbol { found: ch });
            }
            if n >= CODE_LEN {
                return Err(CodeError::WrongLength {
                    found: n.saturating_add(1),
                });
            }
            if let Some(slot) = symbols.get_mut(n) {
                *slot = byte;
                n += 1;
            }
        }

        if n != CODE_LEN {
            return Err(CodeError::WrongLength { found: n });
        }
        Ok(Self { symbols })
    }

    /// The code as bytes, for feeding to the PAKE.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; CODE_LEN] {
        &self.symbols
    }

    /// Render grouped, for display on the host's screen: `4A7C-QF3M`.
    #[must_use]
    pub fn display(&self) -> String {
        let mut out = String::with_capacity(CODE_LEN + GROUPS - 1);
        for (i, b) in self.symbols.iter().enumerate() {
            if i > 0 && i % GROUP == 0 {
                out.push('-');
            }
            out.push(char::from(*b));
        }
        out
    }
}

impl PartialEq for PairingCode {
    /// Constant time.
    ///
    /// A code *is* the secret, and unlike a public key its comparison timing is
    /// a genuine oracle: an attacker who can measure how far a comparison got
    /// learns a prefix, which turns one online guess into eight.
    fn eq(&self, other: &Self) -> bool {
        self.symbols.ct_eq(&other.symbols).into()
    }
}

impl Eq for PairingCode {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn generates_the_right_shape() {
        let c = PairingCode::generate();
        assert_eq!(c.as_bytes().len(), CODE_LEN);
        assert_eq!(
            c.display(),
            format!("{}-{}", &c.display()[..4], &c.display()[5..])
        );
        assert_eq!(c.display().len(), CODE_LEN + 1);
    }

    #[test]
    fn every_generated_symbol_is_in_the_alphabet() {
        for _ in 0..500 {
            for b in PairingCode::generate().as_bytes() {
                assert!(
                    ALPHABET.contains(b),
                    "{:?} is not in the alphabet",
                    char::from(*b)
                );
            }
        }
    }

    /// The alphabet must exclude every character the parser remaps, or a
    /// generated code could contain something the parser rewrites into a
    /// *different* code.
    #[test]
    fn the_alphabet_excludes_every_confusable() {
        for bad in *b"0O1IL2Z5S8BU" {
            assert!(
                !ALPHABET.contains(&bad),
                "{:?} is both in the alphabet and remapped by the parser",
                char::from(bad)
            );
        }
    }

    #[test]
    fn the_alphabet_has_no_duplicates_and_is_the_declared_size() {
        let set: HashSet<&u8> = ALPHABET.iter().collect();
        assert_eq!(set.len(), ALPHABET.len());
        assert_eq!(ALPHABET.len(), 24);
    }

    #[test]
    fn round_trips_through_display_and_parse() {
        for _ in 0..200 {
            let c = PairingCode::generate();
            let parsed = PairingCode::parse(&c.display()).expect("our own output must parse");
            assert_eq!(parsed, c);
        }
    }

    /// Presentation is forgiven; content is not.
    #[test]
    fn parsing_forgives_presentation() {
        let c = PairingCode::generate();
        let d = c.display();
        for variant in [
            d.clone(),
            d.to_lowercase(),
            d.replace('-', ""),
            d.replace('-', " "),
            format!("  {d}  "),
            d.replace('-', "_"),
            d.chars().flat_map(|ch| [ch, ' ']).collect::<String>(),
        ] {
            assert_eq!(
                PairingCode::parse(&variant)
                    .unwrap_or_else(|e| panic!("{variant:?} should parse: {e}")),
                c,
                "variant {variant:?} did not round trip"
            );
        }
    }

    /// Only characters with exactly one plausible source are mapped.
    #[test]
    fn the_two_unambiguous_confusables_are_mapped() {
        for (typed, meant) in [('0', 'Q'), ('O', 'Q'), ('o', 'Q'), ('U', 'V'), ('u', 'V')] {
            let input: String = std::iter::once(typed).chain("34679AC".chars()).collect();
            let parsed = PairingCode::parse(&input)
                .unwrap_or_else(|e| panic!("{typed:?} should map to {meant:?}, got {e}"));
            assert_eq!(
                char::from(parsed.as_bytes()[0]),
                meant,
                "{typed:?} should map to {meant:?}"
            );
        }
    }

    /// An ambiguous character is **refused, not guessed at**.
    ///
    /// Guessing produces a code that parses and then fails against the host,
    /// spending one of three attempts on a typo. `1` is the clearest case: it
    /// could be a misread `7` as easily as a misread `J`, so there is no honest
    /// answer and the user must look again — which is free.
    #[test]
    fn ambiguous_characters_are_refused_rather_than_guessed() {
        for ch in [
            '1', 'I', 'i', 'L', 'l', '2', 'Z', 'z', '5', 'S', 's', '8', 'B', 'b',
        ] {
            let input: String = std::iter::once(ch).chain("34679AC".chars()).collect();
            match PairingCode::parse(&input) {
                Err(CodeError::BadSymbol { found }) => assert_eq!(found, ch),
                other => panic!(
                    "{ch:?} must be refused, not silently mapped — got {other:?}. \
                     A wrong guess costs one of three attempts."
                ),
            }
        }
    }

    /// The error must tell the user what to look for rather than just failing.
    #[test]
    fn the_error_names_the_excluded_characters() {
        let msg = PairingCode::parse("1A34679C")
            .expect_err("should fail")
            .to_string();
        for hint in ["O", "0", "I", "1", "S", "5", "B", "8"] {
            assert!(
                msg.contains(hint),
                "the error should mention {hint:?}: {msg}"
            );
        }
    }

    #[test]
    fn a_short_or_long_code_is_refused_with_the_count() {
        match PairingCode::parse("34679AC") {
            Err(CodeError::WrongLength { found: 7 }) => {}
            other => panic!("expected WrongLength{{7}}, got {other:?}"),
        }
        assert!(matches!(
            PairingCode::parse("34679ACDE"),
            Err(CodeError::WrongLength { .. })
        ));
        assert!(matches!(
            PairingCode::parse(""),
            Err(CodeError::WrongLength { found: 0 })
        ));
    }

    #[test]
    fn a_symbol_outside_the_alphabet_names_itself() {
        match PairingCode::parse("34679AC!") {
            Err(CodeError::BadSymbol { found: '!' }) => {}
            other => panic!("expected BadSymbol{{'!'}}, got {other:?}"),
        }
        assert!(matches!(
            PairingCode::parse("34679AC\u{1F512}"),
            Err(CodeError::BadSymbol { .. })
        ));
    }

    /// Rejection sampling must actually produce a flat distribution. A modulo
    /// would tilt the first 16 symbols upward, and the tilt is invisible
    /// without looking for it.
    #[test]
    fn the_symbol_distribution_is_flat() {
        let draws = 24_000usize;
        let mut counts = [0usize; 24];
        for _ in 0..draws / CODE_LEN {
            for b in PairingCode::generate().as_bytes() {
                let idx = ALPHABET.iter().position(|a| a == b).expect("in alphabet");
                if let Some(c) = counts.get_mut(idx) {
                    *c += 1;
                }
            }
        }
        let expected = draws / 24;
        // A generous band: this catches a systematic modulo bias (which would
        // make the first 16 symbols ~1.5x the last 8), not sampling noise.
        for (i, c) in counts.iter().enumerate() {
            let ratio = *c as f64 / expected as f64;
            assert!(
                (0.75..1.25).contains(&ratio),
                "symbol {:?} appeared {c} times, expected ~{expected} (ratio {ratio:.2}) — \
                 a modulo bias would show here",
                ALPHABET.get(i).copied().map_or('?', char::from)
            );
        }
    }

    #[test]
    fn codes_do_not_repeat() {
        let mut seen = HashSet::new();
        for _ in 0..2000 {
            assert!(
                seen.insert(PairingCode::generate().display()),
                "a generated code repeated within 2000 draws"
            );
        }
    }

    /// The code must never reach a log, an error, or a panic message.
    #[test]
    fn debug_never_reveals_the_code() {
        let c = PairingCode::generate();
        let rendered = format!("{c:?}");
        assert_eq!(rendered, "PairingCode(<redacted>)");
        for b in c.as_bytes() {
            assert!(
                !rendered.contains(char::from(*b))
                    || "PairingCode(<redacted>)".contains(char::from(*b)),
                "Debug leaked a code symbol"
            );
        }
    }

    #[test]
    fn comparison_is_by_value_not_by_reference() {
        let a = PairingCode::generate();
        let b = PairingCode::parse(&a.display()).expect("parse");
        assert_eq!(a, b);
        assert_ne!(a, PairingCode::generate());
    }

    /// The declared entropy must match the alphabet, or the security argument
    /// in the module documentation is wrong.
    #[test]
    fn the_declared_entropy_matches_the_alphabet() {
        let actual = (ALPHABET.len() as f64).log2() * CODE_LEN as f64;
        assert!(
            (actual - CODE_BITS).abs() < 0.05,
            "declared {CODE_BITS} bits, alphabet gives {actual:.2}"
        );
    }
}
