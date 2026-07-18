//! Human-readable device fingerprints.
//!
//! A fingerprint has to be read aloud — down a phone line, to someone who is
//! not technical, possibly on a bad connection. Hex fails that badly: it is
//! long, "b" and "d" are hard to distinguish spoken, and nobody reads 64
//! characters accurately.
//!
//! So a fingerprint renders as words. The list below is Esper's own, and its
//! properties are enforced by the tests in this module rather than inherited
//! from a standard we might misremember:
//!
//! - exactly 256 words, so each encodes one byte with no remainder
//! - all distinct, and **distinct in their first three letters**, so a
//!   half-heard word is still unambiguous
//! - three to eight ASCII lowercase letters, no homophones of each other
//! - drawn from 25 letters of the alphabet, so they do not sound alike in bulk
//!
//! Eight words is 64 bits, which is the bar for a fingerprint a human compares.
//!
//! **The list is frozen.** Changing a word changes every fingerprint it appears
//! in, which would make previously-verified devices look like different
//! devices. `WORDLIST_CHECKSUM` guards against an accidental edit.

/// The number of words in a rendered fingerprint. Eight words × 8 bits = 64.
pub const FINGERPRINT_WORDS: usize = 8;

/// One byte per word.
pub const WORDLIST_LEN: usize = 256;

/// Words used to render fingerprints. Frozen — see the module docs.
///
/// The six-per-line layout is pinned rather than left to rustfmt. rustfmt packs
/// as many as fit, so changing one word reflows several lines and buries the
/// actual change in a large diff. For a frozen list where every edit invalidates
/// fingerprints, a one-line-per-change diff is worth the annotation.
#[rustfmt::skip]
pub(crate) const WORDS: [&str; WORDLIST_LEN] = [
    "absurd", "actor", "adopt", "agree", "album", "almond",
    "amuse", "animal", "anvil", "april", "armor", "aspect",
    "atom", "aurora", "awake", "badge", "bamboo", "basil",
    "beetle", "benign", "bicycle", "blanket", "blossom", "bonus",
    "bracket", "bronze", "budget", "bunker", "bypass", "camel",
    "carbon", "caviar", "chalk", "chorus", "citrus", "clever",
    "cluster", "cocoa", "combat", "coral", "cover", "credit",
    "crystal", "cupcake", "cyclone", "daisy", "dapper", "debate",
    "deliver", "descent", "diamond", "digest", "diploma", "diverse",
    "domain", "double", "drummer", "eagle", "eclipse", "eggplant",
    "elder", "email", "emit", "endorse", "enrich", "envelop",
    "errand", "estate", "exact", "expand", "fabric", "fantasy",
    "fedora", "fiber", "figure", "firefly", "fleet", "fluid",
    "fossil", "frost", "gadget", "garden", "gecko", "geyser",
    "giraffe", "global", "golden", "green", "gulf", "hammer",
    "heather", "hexagon", "highway", "honey", "hotel", "hydrant",
    "igloo", "infant", "inner", "iris", "ivory", "jaguar",
    "jersey", "jockey", "jubilee", "jumbo", "kangaroo", "kelp",
    "kettle", "kilo", "kitchen", "koala", "lamp", "larch",
    "laundry", "layer", "lens", "liberty", "linen", "listen",
    "llama", "lodge", "lounge", "lunar", "magnet", "major",
    "maple", "matrix", "member", "meteor", "mimic", "mission",
    "moment", "mosaic", "muffin", "myrtle", "nature", "nebula",
    "neon", "nest", "newborn", "nitrogen", "noodle", "notice",
    "nugget", "nylon", "obelisk", "oboe", "occupy", "office",
    "omega", "opera", "orbit", "origin", "otter", "oxide",
    "pacific", "palace", "parade", "pause", "pencil", "petal",
    "piano", "pilot", "pivot", "planet", "plum", "polar",
    "poster", "prairie", "probe", "pulley", "puppet", "pyramid",
    "quest", "rabbit", "rainbow", "ranch", "razor", "recipe",
    "region", "render", "reveal", "rifle", "river", "rocket",
    "roof", "round", "rudder", "rumor", "rustic", "sailor",
    "sapling", "savory", "school", "sculpt", "sedan", "senate",
    "serene", "shadow", "shrimp", "signal", "siren", "slate",
    "smooth", "sodium", "sorbet", "spark", "splash", "squad",
    "stitch", "studio", "summer", "surf", "sweater", "syntax",
    "tadpole", "tapestry", "teacup", "terrain", "thorn", "ticket",
    "timber", "toaster", "tomato", "total", "toxic", "triangle",
    "tulip", "tuxedo", "ultra", "update", "urban", "utensil",
    "valley", "variety", "velvet", "vessel", "vibrant", "viking",
    "viper", "vital", "voltage", "wafer", "wander", "watch",
    "welcome", "wheat", "window", "wolf", "worthy", "yacht",
    "yeti", "young", "zephyr", "zinc",
];

/// A checksum over the frozen list.
///
/// Changing a word changes every fingerprint containing it, so a device that
/// was verified would silently look like a different device. This constant is
/// asserted by a test: an accidental edit fails the build rather than quietly
/// invalidating every fingerprint a user has ever compared.
#[cfg(test)]
pub(crate) const WORDLIST_CHECKSUM: u64 = 0xdb0c_8e25_6c38_cb9f;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// The property that makes a half-heard word recoverable.
    #[test]
    fn every_word_is_unique_in_its_first_three_letters() {
        let prefixes: HashSet<&str> = WORDS.iter().map(|w| &w[..3]).collect();
        assert_eq!(
            prefixes.len(),
            WORDLIST_LEN,
            "two words share a three-letter prefix; a misheard word becomes ambiguous"
        );
    }

    #[test]
    fn every_word_is_unique() {
        let set: HashSet<&&str> = WORDS.iter().collect();
        assert_eq!(set.len(), WORDLIST_LEN);
    }

    /// Exactly 256 so a byte maps to a word with no remainder and no bias.
    #[test]
    fn the_list_is_exactly_a_byte_wide() {
        assert_eq!(WORDS.len(), 256);
    }

    #[test]
    fn every_word_is_short_lowercase_ascii() {
        for w in WORDS {
            assert!(
                (3..=8).contains(&w.len()),
                "{w:?} is {} characters; hard to say or hard to hear",
                w.len()
            );
            assert!(
                w.chars().all(|c| c.is_ascii_lowercase()),
                "{w:?} is not plain lowercase ASCII"
            );
        }
    }

    /// A list clustered on a few initial letters sounds like noise when eight
    /// words are read in sequence.
    #[test]
    fn the_list_spans_the_alphabet() {
        let initials: HashSet<char> = WORDS.iter().filter_map(|w| w.chars().next()).collect();
        assert!(
            initials.len() >= 20,
            "only {} distinct initial letters; the list is too clustered",
            initials.len()
        );
    }

    /// The list is sorted, which is not required to work but makes review and
    /// diffing sane, and makes an accidental duplicate obvious.
    #[test]
    fn the_list_is_sorted() {
        let mut sorted = WORDS;
        sorted.sort_unstable();
        assert_eq!(WORDS, sorted, "the wordlist must stay sorted");
    }

    /// Guards the frozen list against an accidental edit. If this fails because
    /// you changed a word deliberately, understand first that every fingerprint
    /// containing that word changes with it.
    #[test]
    fn the_list_has_not_changed() {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a
        for w in WORDS {
            for b in w.as_bytes() {
                h ^= u64::from(*b);
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
            h ^= 0xff;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        assert_eq!(
            h, WORDLIST_CHECKSUM,
            "the wordlist changed. If deliberate, update WORDLIST_CHECKSUM to {h:#x} \
             — but every fingerprint containing a changed word changes too."
        );
    }
}
