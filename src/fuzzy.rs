//! Lightweight fuzzy string matching (0-100 scores), modeled after the
//! Python reference's use of `thefuzz`/`rapidfuzz`.
//!
//! `rapidfuzz` (crates.io, v0.5.0) does not yet expose `partial_ratio` or an
//! `extractOne`-style helper, so per docs/SPEC.md's fallback note this
//! reimplements the two scoring functions the Python reference relies on
//! (`fuzz.ratio`, `fuzz.partial_ratio`) on top of `strsim`'s Levenshtein
//! distance.

use strsim::levenshtein;

/// Normalized Levenshtein similarity between two strings, as a 0-100 score.
pub fn ratio(a: &str, b: &str) -> f64 {
    let a_len = a.chars().count();
    let b_len = b.chars().count();
    if a_len == 0 && b_len == 0 {
        return 100.0;
    }
    let dist = levenshtein(a, b) as f64;
    let max_len = a_len.max(b_len) as f64;
    (1.0 - dist / max_len) * 100.0
}

/// Best `ratio` between `a` and any equal-length window of the longer of
/// `a`/`b`. Mirrors rapidfuzz's `partial_ratio` for the common
/// "is the short string contained/similar within the long one" case.
pub fn partial_ratio(a: &str, b: &str) -> f64 {
    let (short, long) = if a.chars().count() <= b.chars().count() {
        (a, b)
    } else {
        (b, a)
    };
    let short_len = short.chars().count();
    if short_len == 0 {
        return if long.is_empty() { 100.0 } else { 0.0 };
    }
    let long_chars: Vec<char> = long.chars().collect();
    if long_chars.len() <= short_len {
        return ratio(short, long);
    }

    let mut best = 0.0f64;
    for start in 0..=(long_chars.len() - short_len) {
        let window: String = long_chars[start..start + short_len].iter().collect();
        let score = ratio(short, &window);
        if score > best {
            best = score;
        }
        if best >= 100.0 {
            break;
        }
    }
    best
}

/// Finds the best-scoring candidate for `query` among `candidates` (case
/// insensitive), mirroring `thefuzz.process.extractOne(query, candidates,
/// score_cutoff=...)`. Returns `None` if no candidate reaches `score_cutoff`.
pub fn extract_one<'a, I>(query: &str, candidates: I, score_cutoff: f64) -> Option<(&'a str, f64)>
where
    I: IntoIterator<Item = &'a str>,
{
    let query_lower = query.to_lowercase();
    // (candidate, score, whole-string ratio)
    let mut best: Option<(&str, f64, f64)> = None;
    for candidate in candidates {
        let candidate_lower = candidate.to_lowercase();
        // A short query against a long candidate ("paint" vs "Untitled -
        // Paint") scores near zero on whole-string Levenshtein, so window and
        // Start Menu lookups missed obvious matches. partial_ratio asks the
        // question the caller actually means: does the query appear inside the
        // candidate? Keep the better of the two.
        let whole = ratio(&query_lower, &candidate_lower);
        let score = whole.max(partial_ratio(&query_lower, &candidate_lower));
        // partial_ratio scores every candidate containing the query at 100,
        // so ties are common: "設定" against both "設定" and "Office 言語設定".
        // The first one seen used to win, and candidates often come from a
        // HashMap, so the choice was arbitrary. Break the tie by how much of
        // the candidate the query covers, which puts an exact name first.
        let better = best.is_none_or(|(_, best_score, best_whole)| {
            score > best_score || (score == best_score && whole > best_whole)
        });
        if score >= score_cutoff && better {
            best = Some((candidate, score, whole));
        }
    }
    best.map(|(candidate, score, _)| (candidate, score))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_exact_name_beats_a_longer_one_containing_it() {
        // Both contain the query, so both score 100; order must not decide.
        for candidates in [
            ["office 言語設定", "設定", "設定アプリの既定"],
            ["設定アプリの既定", "設定", "office 言語設定"],
        ] {
            assert_eq!(extract_one("設定", candidates, 70.0).unwrap().0, "設定");
        }
    }

    #[test]
    fn ratio_identical_is_100() {
        assert_eq!(ratio("chrome", "chrome"), 100.0);
    }

    #[test]
    fn ratio_score_cutoff_threshold() {
        // "chrme" vs "chrome": within score_cutoff 70 used for App/window matching.
        assert!(ratio("chrme", "chrome") >= 70.0);
        // Unrelated strings should fall well below the cutoff.
        assert!(ratio("notepad", "calculator") < 70.0);
    }

    #[test]
    fn partial_ratio_substring_scores_high() {
        // Process name filter uses partial_ratio > 60.
        assert!(partial_ratio("chrome", "googlechromedev.exe") > 60.0);
    }

    #[test]
    fn extract_one_matches_short_query_inside_long_window_title() {
        // App switch/resize: the user says "paint", the window is "Untitled - Paint".
        let titles = ["Untitled - Paint", "Document1 - Word"];
        assert_eq!(
            extract_one("paint", titles, 70.0).map(|(n, _)| n),
            Some("Untitled - Paint")
        );
    }

    #[test]
    fn extract_one_still_rejects_unrelated_candidates() {
        let titles = ["Untitled - Paint", "Document1 - Word"];
        assert!(extract_one("spreadsheet", titles, 70.0).is_none());
    }

    #[test]
    fn extract_one_respects_score_cutoff() {
        let candidates = ["Notepad", "Calculator", "Command Prompt"];
        let best = extract_one("notepad", candidates, 70.0);
        assert_eq!(best.map(|(name, _)| name), Some("Notepad"));

        let none = extract_one("xyzxyzxyz", candidates, 70.0);
        assert!(none.is_none());
    }
}
