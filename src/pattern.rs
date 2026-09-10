//! Matching of vPIC pattern keys against a VIN.
//!
//! A pattern key is matched against the VIN's *match key*, which vPIC builds as
//! VIN positions 4-8, a literal `|`, then positions 10-17:
//!
//! ```text
//! VIN        1C6RR7LT2JS179571
//!               ^^^^^ ^^^^^^^^
//!               4-8   10-17
//! match key  RR7LT|JS179571
//! ```
//!
//! Position 9 (the check digit) is deliberately absent, and the `|` is a literal
//! character both sides have to agree on. Keys are matched from the left and may
//! be shorter than the match key, so `RR7L` and `*****|*S` both match above.
//! In the second form the `*` after the bar covers the model year at position 10
//! and the `S` pins the plant code at position 11.
//!
//! Within a key:
//!
//! - `*` matches exactly one character,
//! - `[ABC]` / `[A-H]` matches one character from the class,
//! - `#` matches one digit and marks a span to be read out of the VIN
//!   (see [`formula_value`]),
//! - anything else is literal.
//!
//! `_` is a quirk: vPIC matches bracket-free keys with SQL `LIKE`, where `_` is
//! a single-character wildcard, but matches keys containing `[` with a regex,
//! where `_` is literal. Both behaviours are reproduced.

/// Build the match key vPIC compares pattern keys against.
///
/// # Examples
///
/// ```
/// use corgi_rs::pattern::match_key;
/// assert_eq!(match_key("1C6RR7LT2JS179571"), "RR7LT|JS179571");
/// ```
pub fn match_key(vin: &str) -> String {
    let mut key = String::with_capacity(14);
    if vin.len() > 3 {
        key.push_str(&vin[3..vin.len().min(8)]);
    }
    if vin.len() > 9 {
        key.push('|');
        key.push_str(&vin[9..vin.len().min(17)]);
    }
    key
}

/// How specific a key is, for ranking. Mirrors vPIC's
/// `LENGTH(REPLACE(Keys, '*', ''))`: bracket punctuation counts, wildcards do not.
pub fn literal_len(keys: &str) -> usize {
    keys.chars().filter(|c| *c != '*').count()
}

/// Whether `keys` matches `key`, anchored at the left.
///
/// # Examples
///
/// ```
/// use corgi_rs::pattern::keys_match;
/// assert!(keys_match("RR7LT", "RR7LT|JS179571"));
/// assert!(keys_match("*****|*S", "RR7LT|JS179571"));
/// assert!(keys_match("[QR]R7", "RR7LT|JS179571"));
/// assert!(!keys_match("[QS]R7", "RR7LT|JS179571"));
/// // A key that outruns the VIN cannot match.
/// assert!(!keys_match("RR7LT|JS1795711", "RJFBG|FC123456"));
/// ```
pub fn keys_match(keys: &str, key: &str) -> bool {
    let underscore_is_wildcard = !keys.contains('[');
    let mut input = key.chars();
    let mut pattern = keys.chars().peekable();

    while let Some(p) = pattern.next() {
        let Some(i) = input.next() else {
            // Key is longer than what the VIN offers.
            return false;
        };

        let ok = match p {
            '*' => true,
            '_' if underscore_is_wildcard => true,
            '#' => i.is_ascii_digit(),
            '[' => {
                let mut class = String::new();
                let mut closed = false;
                for c in pattern.by_ref() {
                    if c == ']' {
                        closed = true;
                        break;
                    }
                    class.push(c);
                }
                // An unterminated class is malformed data; refuse to match on it
                // rather than guessing.
                closed && class_contains(&class, i)
            }
            _ => p == i,
        };

        if !ok {
            return false;
        }
    }

    true
}

/// Whether `ch` is a member of the character class body `class` (the text
/// between `[` and `]`), which may mix single characters and `A-H` style ranges.
fn class_contains(class: &str, ch: char) -> bool {
    let chars: Vec<char> = class.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        if i + 2 < chars.len() && chars[i + 1] == '-' {
            let (start, end) = (chars[i], chars[i + 2]);
            if start <= ch && ch <= end {
                return true;
            }
            i += 3;
        } else {
            if chars[i] == ch {
                return true;
            }
            i += 1;
        }
    }

    false
}

/// Whether `keys` is a formula key, i.e. one that reads digits out of the VIN
/// instead of naming a fixed value.
pub fn is_formula(keys: &str) -> bool {
    keys.contains('#')
}

/// Match a formula key and return the digits it selects from the match key.
///
/// vPIC replaces every digit of the match key with `#` before matching, then
/// slices the match key between the first and last `#` of the pattern. So `**##`
/// against `RR12T|...` yields `12`.
///
/// # Examples
///
/// ```
/// use corgi_rs::pattern::formula_value;
/// assert_eq!(formula_value("**##", "RR12T|JS179571").as_deref(), Some("12"));
/// assert_eq!(formula_value("**##", "RR7LT|JS179571"), None);
/// ```
pub fn formula_value(keys: &str, key: &str) -> Option<String> {
    let first = keys.find('#')?;
    let last = keys.rfind('#')?;

    if !keys_match(keys, key) {
        return None;
    }

    // Pattern offsets are character offsets into the match key; both are ASCII.
    let value: String = key.chars().skip(first).take(last - first + 1).collect();
    if value.is_empty() { None } else { Some(value) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_key_drops_the_check_digit() {
        // Position 9 ('3' here) must not appear: patterns never address it.
        assert_eq!(match_key("1HGCP26739A060971"), "CP267|9A060971");
    }

    #[test]
    fn match_key_tolerates_short_input() {
        assert_eq!(match_key("1HG"), "");
        assert_eq!(match_key("1HGCP267"), "CP267");
    }

    #[test]
    fn literal_len_counts_bracket_punctuation_but_not_wildcards() {
        assert_eq!(literal_len("RR7LT"), 5);
        assert_eq!(literal_len("*****|*S"), 2);
        assert_eq!(literal_len("[FWX]G"), 6);
    }

    #[test]
    fn keys_match_is_a_prefix_match() {
        let key = "RR7LT|JS179571";
        assert!(keys_match("R", key));
        assert!(keys_match("RR7LT|J", key));
        assert!(!keys_match("RR7LX", key));
    }

    #[test]
    fn keys_match_requires_the_separator_to_line_up() {
        // The `|` is literal, so a VIS key only matches after five VDS positions.
        assert!(keys_match("*****|*S", "RR7LT|JS179571"));
        assert!(!keys_match("****|*C", "RJFBG|FC123456"));
    }

    #[test]
    fn keys_match_handles_classes_and_ranges() {
        let key = "RR7LT|JS179571";
        assert!(keys_match("[A-Z]R7", key));
        assert!(keys_match("[QRS]R7", key));
        assert!(!keys_match("[A-Q]R7", key));
        assert!(keys_match("[0-9A-Z]R7", key));
    }

    #[test]
    fn keys_match_rejects_an_unterminated_class() {
        assert!(!keys_match("[ABR", "RR7LT|JS179571"));
    }

    #[test]
    fn underscore_is_a_wildcard_only_without_brackets() {
        assert!(keys_match("_R7LT", "RR7LT|JS179571"));
        // With a class present the key is matched as a regex, where `_` is literal.
        assert!(!keys_match("_R7L[LT]", "RR7LT|JS179571"));
    }

    #[test]
    fn formula_keys_only_match_digits() {
        assert_eq!(
            formula_value("**##", "RR12T|JS179571").as_deref(),
            Some("12")
        );
        assert_eq!(formula_value("**##", "RJ1BG|FC123456"), None);
        assert_eq!(formula_value("*#", "R1FBG|FC123456").as_deref(), Some("1"));
    }
}
