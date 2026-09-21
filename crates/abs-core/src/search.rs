//! Diacritic-insensitive text matching for the Library screen's local search — it filters an
//! already-synced, in-memory item list (no server round trip), so this is the one place that
//! needs to change to make "national characters" optional for the user typing a query.

use unicode_normalization::char::is_combining_mark;
use unicode_normalization::UnicodeNormalization;

/// Lowercases, substitutes the handful of letters Unicode doesn't decompose at all (`ł`/`Ł` are
/// distinct letters, not "l" plus a diacritic, so NFD alone can never turn them into `l`), then
/// strips combining diacritical marks via NFD decomposition — which handles the general case
/// (`ą`, `ć`, `ę`, `ń`, `ó`, `ś`, `ź`, `ż` and equivalents in other languages) without needing an
/// entry per letter. Two strings that only differ by diacritics normalize to the same value, so
/// comparing normalized forms with `.contains(...)` makes the match diacritic-insensitive.
pub fn normalize_for_search(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            'ł' => 'l',
            'Ł' => 'L',
            c => c,
        })
        .collect::<String>()
        .nfd()
        .filter(|c| !is_combining_mark(*c))
        .collect::<String>()
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reported_case_matches_both_ways() {
        assert_eq!(normalize_for_search("łąka"), normalize_for_search("laka"));
        assert_eq!(normalize_for_search("łąka"), normalize_for_search("ląka"));
    }

    #[test]
    fn mixed_case_national_characters_normalize_the_same_as_plain_ascii() {
        assert_eq!(normalize_for_search("ŁĄKA"), normalize_for_search("laka"));
    }

    #[test]
    fn plain_ascii_is_unchanged_apart_from_case() {
        assert_eq!(normalize_for_search("Dune"), "dune");
    }

    #[test]
    fn other_common_diacritics_are_stripped_via_nfd() {
        assert_eq!(normalize_for_search("café"), normalize_for_search("cafe"));
        assert_eq!(normalize_for_search("Zoë"), normalize_for_search("zoe"));
    }
}
