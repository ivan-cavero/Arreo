//! The pairing code: four words from a committed 256-word list (T-0024).
//!
//! One sentence: a code is 32 bits of entropy a human can read aloud, and it
//! exists only inside the SPAKE2 password — never on the wire, never at the
//! relay.
//!
//! Why words: §3.3's "pair in 30 seconds, no password to type" needs something
//! a person can carry between two screens without OCR, a cable, or an account.
//! Four words from a 256-word list is 32 bits — small enough to type, large
//! enough that guessing is hopeless *when the guess budget is one* (SPAKE2
//! upgrades the low-entropy secret to a strong channel, and a wrong guess burns
//! the session; see `flow`).
//!
//! The list is committed (not generated) so codes are reproducible in tests and
//! reviewable by eye: every word is 3–9 lowercase ASCII letters and no two
//! words are confusable at a glance.

use super::PairingError;

/// Exactly 256 words → 8 bits per word.
pub const WORD_COUNT: usize = 256;

/// Words in a code → 32 bits of entropy.
pub const WORDS_PER_CODE: usize = 4;

/// The committed word list. Index = the 8-bit value the word encodes.
pub const WORDS: [&str; WORD_COUNT] = [
    "amber", "anchor", "apple", "arrow", "atlas", "autumn", "badge", "bamboo", "barrel", "basket",
    "beacon", "beaver", "bishop", "bottle", "bramble", "bridge", "bronze", "brook", "brush",
    "bubble", "bucket", "buffer", "bundle", "button", "cactus", "camel", "candle", "canyon",
    "carbon", "carpet", "castle", "cedar", "cellar", "chalk", "cherry", "circle", "citrus",
    "clever", "cliff", "clover", "cobalt", "cocoa", "comet", "copper", "coral", "cotton", "cougar",
    "crane", "crater", "cricket", "crimson", "crystal", "cypress", "daisy", "dapper", "delta",
    "desert", "diamond", "dolphin", "donkey", "dragon", "drum", "eagle", "ember", "engine",
    "falcon", "feather", "fiddle", "figure", "flint", "forest", "fossil", "fountain", "frost",
    "garden", "garnet", "geyser", "ginger", "glacier", "granite", "grape", "gravel", "grove",
    "guitar", "gypsum", "hammer", "harbor", "harvest", "hazel", "helmet", "heron", "hickory",
    "hollow", "honey", "hunter", "indigo", "island", "ivory", "jacket", "jaguar", "jasper",
    "jungle", "juniper", "kernel", "kettle", "keypad", "kitten", "knight", "koala", "ladder",
    "lagoon", "lantern", "laptop", "lattice", "lavender", "lemon", "leopard", "level", "lilac",
    "linen", "lizard", "lobster", "locust", "lotus", "lumber", "lunar", "magnet", "mango", "maple",
    "marble", "marlin", "meadow", "medal", "melon", "mentor", "meteor", "mirror", "mitten",
    "monkey", "mustard", "napkin", "nebula", "needle", "nickel", "nectar", "ninja", "noodle",
    "nugget", "oasis", "ocean", "olive", "onion", "opal", "orbit", "orchid", "osprey", "otter",
    "oxide", "oyster", "paddle", "palm", "panda", "papaya", "parrot", "pastel", "peach", "pebble",
    "pecan", "pelican", "penguin", "pepper", "petal", "pewter", "phoenix", "piano", "pigeon",
    "pillar", "pilot", "piston", "pixel", "planet", "plasma", "plateau", "plum", "pocket", "polar",
    "pollen", "poplar", "poppy", "portal", "potato", "prairie", "pretzel", "prism", "puffin",
    "pumpkin", "puzzle", "python", "quartz", "quill", "quilt", "rabbit", "radar", "radish",
    "raven", "ribbon", "riddle", "ripple", "river", "robin", "rocket", "rooster", "rose", "ruby",
    "rudder", "saddle", "saffron", "salmon", "sandal", "satin", "sapling", "scarlet", "scroll",
    "seabird", "sesame", "shadow", "shelter", "shovel", "shrimp", "signal", "silver", "siren",
    "skate", "sledge", "slipper", "smoke", "socket", "sonnet", "sparrow", "spider", "spring",
    "spruce", "squirrel", "stable", "sunset", "teapot", "tulip", "tundra", "turbine", "turtle",
    "unicorn", "velvet", "violet", "walnut", "willow", "zebra",
];

/// A pairing code: the word indices (never the words themselves, so equality
/// and entropy are structural rather than stringly).
#[derive(Clone, PartialEq, Eq)]
pub struct Code {
    indices: [u8; WORDS_PER_CODE],
}

impl std::fmt::Debug for Code {
    /// Print the phrase. A code is a *short-lived secret*, but unlike a key it
    /// is meant to be shown to the human who is pairing — hiding it in logs
    /// would make debugging pairing impossible. It is never logged by the
    /// transport, only by the CLI that is already displaying it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Code({})", self.phrase())
    }
}

impl Code {
    /// A fresh code from OS entropy.
    pub fn random() -> Result<Self, PairingError> {
        let mut bytes = [0u8; WORDS_PER_CODE];
        getrandom::fill(&mut bytes).map_err(|e| PairingError::Entropy(e.to_string()))?;
        Ok(Self { indices: bytes })
    }

    /// Build from four word indices (tests and vectors).
    #[must_use]
    pub fn from_indices(indices: [u8; WORDS_PER_CODE]) -> Self {
        Self { indices }
    }

    /// Parse a human-typed code. Case, spacing and separators are forgiven;
    /// unknown words are not (a typo must fail, never silently become another
    /// code — that would turn a fixable typo into a mysterious failure).
    pub fn parse(text: &str) -> Result<Self, PairingError> {
        let normalized: String = text
            .to_ascii_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphabetic() { c } else { ' ' })
            .collect();
        let words: Vec<&str> = normalized.split_whitespace().collect();
        if words.len() != WORDS_PER_CODE {
            return Err(PairingError::CodeShape {
                got: words.len(),
                want: WORDS_PER_CODE,
            });
        }
        let mut indices = [0u8; WORDS_PER_CODE];
        for (slot, word) in indices.iter_mut().zip(words) {
            let index = WORDS
                .iter()
                .position(|candidate| *candidate == word)
                .ok_or_else(|| PairingError::UnknownWord {
                    word: word.to_string(),
                    hint: nearest_word(word),
                })?;
            *slot = index as u8;
        }
        Ok(Self { indices })
    }

    /// The display form: `"harbor lantern ember quartz"`.
    #[must_use]
    pub fn phrase(&self) -> String {
        self.indices
            .iter()
            .map(|index| WORDS[*index as usize])
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The exact bytes SPAKE2 uses as its password. One function, so the two
    /// sides cannot encode the same code differently (a mismatch here would
    /// look exactly like a wrong code).
    ///
    /// The words are joined with single spaces in lowercase — the same string
    /// the human sees — so "the code is the password" is literally true.
    #[must_use]
    pub fn password(&self) -> Vec<u8> {
        self.phrase().into_bytes()
    }

    /// Entropy in bits — quoted in the CLI so the user knows what they are
    /// trusting, and asserted by the tests so the list cannot silently shrink.
    #[must_use]
    pub fn entropy_bits() -> u32 {
        // log2(256) * 4
        (WORD_COUNT.trailing_zeros()) * WORDS_PER_CODE as u32
    }
}

/// Closest list word by edit distance, for a "did you mean" that costs one
/// line instead of a support question.
fn nearest_word(word: &str) -> Option<&'static str> {
    let mut best: Option<(&'static str, usize)> = None;
    for candidate in WORDS {
        let distance = edit_distance(word, candidate);
        if best.is_none() || distance < best.map(|(_, d)| d).unwrap_or(usize::MAX) {
            best = Some((candidate, distance));
        }
    }
    match best {
        // Only suggest when the typo is close: a wild guess is worse than none.
        Some((candidate, distance)) if distance <= 3 => Some(candidate),
        _ => None,
    }
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        current[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            current[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(current[j] + 1);
        }
        std::mem::swap(&mut prev, &mut current);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_is_exactly_256_distinct_lowercase_words() {
        let mut seen = std::collections::HashSet::new();
        for (index, word) in WORDS.iter().enumerate() {
            assert!(
                word.chars().all(|c| c.is_ascii_lowercase()),
                "word {index} ({word}) is not lowercase ascii"
            );
            assert!(
                (3..=9).contains(&word.len()),
                "word {index} ({word}) is an awkward length to read aloud"
            );
            assert!(seen.insert(*word), "duplicate word {word} at index {index}");
        }
        assert_eq!(seen.len(), WORD_COUNT);
        assert_eq!(Code::entropy_bits(), 32);
    }

    #[test]
    fn codes_round_trip_through_the_human_form() {
        let code = Code::from_indices([7, 64, 136, 255]);
        let phrase = code.phrase();
        assert_eq!(phrase, "bamboo engine mirror zebra");
        assert_eq!(Code::parse(&phrase).expect("parses"), code);
        assert_eq!(code.password(), phrase.as_bytes());
    }

    #[test]
    fn parsing_forgives_formatting_but_not_wrong_words() {
        let code = Code::from_indices([7, 64, 136, 255]);
        // Case, padding, punctuation and separators are all the human's choice.
        for typed in [
            "  BAMBOO   ENGINE MIRROR ZEBRA ",
            "Bamboo-Engine-Mirror-Zebra",
            "bamboo,engine,mirror,zebra",
            "bamboo\nengine\nmirror\nzebra",
        ] {
            assert_eq!(Code::parse(typed).expect(typed), code, "{typed:?}");
        }
        // Wrong word count is a shape error, with the count in the message.
        match Code::parse("bamboo engine mirror") {
            Err(PairingError::CodeShape { got, want }) => {
                assert_eq!((got, want), (3, 4));
            }
            other => panic!("expected CodeShape, got {other:?}"),
        }
        // A typo names the word and suggests the nearest one.
        match Code::parse("bambu engine mirror zebra") {
            Err(PairingError::UnknownWord { word, hint }) => {
                assert_eq!(word, "bambu");
                assert_eq!(hint, Some("bamboo"));
            }
            other => panic!("expected UnknownWord, got {other:?}"),
        }
        // A code with no suggestion is still a clean refusal.
        assert!(matches!(
            Code::parse("qqqq engine mirror zebra"),
            Err(PairingError::UnknownWord { hint: None, .. })
        ));
    }

    #[test]
    fn generated_codes_use_the_whole_space_and_do_not_repeat() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..256 {
            let code = Code::random().expect("entropy");
            // Every word must be a real list word (no index overflow).
            for word in code.phrase().split(' ') {
                assert!(WORDS.contains(&word), "{word} is not in the list");
            }
            seen.insert(code.phrase());
        }
        assert!(
            seen.len() > 250,
            "codes repeat far too often: {}",
            seen.len()
        );
    }

    #[test]
    fn a_code_never_equals_another_by_accident() {
        let a = Code::from_indices([0, 0, 0, 0]);
        let b = Code::from_indices([0, 0, 0, 1]);
        assert_ne!(a, b);
        assert_ne!(a.phrase(), b.phrase());
        assert_ne!(a.password(), b.password());
    }
}
