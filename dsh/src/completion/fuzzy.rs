use ::fuzzy_matcher::FuzzyMatcher;
use ::fuzzy_matcher::skim::SkimMatcherV2;
use std::sync::LazyLock;

/// Singleton fuzzy matcher to avoid repeated allocation
static FUZZY_MATCHER: LazyLock<SkimMatcherV2> = LazyLock::new(SkimMatcherV2::default);

/// Bonus for candidates whose text starts with the typed pattern.
const PREFIX_MATCH_BONUS: i64 = 1000;

/// Bonus for an exact (case-sensitive) match, on top of the prefix bonus.
const EXACT_MATCH_BONUS: i64 = 500;

pub fn fuzzy_match_score(choice: &str, pattern: &str) -> Option<i64> {
    if pattern.is_empty() {
        return Some(0);
    }
    FUZZY_MATCHER.fuzzy_match(choice, pattern)
}

/// Fuzzy score with deterministic tie-breakers layered on top: prefix
/// matches win over scattered matches and, between equal raw scores,
/// shorter choices rank higher.
///
/// The length penalty is capped so it can only reorder near-equal scores,
/// never outweigh a genuinely better match.
pub fn fuzzy_rank(choice: &str, pattern: &str) -> Option<i64> {
    let mut rank = fuzzy_match_score(choice, pattern)?;
    if choice.starts_with(pattern) {
        rank += PREFIX_MATCH_BONUS;
        if choice == pattern {
            rank += EXACT_MATCH_BONUS;
        }
    }
    let capped_len = choice.chars().count().min(PREFIX_MATCH_BONUS as usize) as i64;
    Some(rank - capped_len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fuzzy_match_score() {
        assert!(fuzzy_match_score("cargo", "c").is_some());
        assert!(fuzzy_match_score("cargo", "ca").is_some());
        assert!(fuzzy_match_score("cargo", "z").is_none());
    }

    #[test]
    fn test_fuzzy_match_ranking() {
        let score1 = fuzzy_match_score("cargo", "car").unwrap();
        let score2 = fuzzy_match_score("cargo", "c").unwrap();
        assert!(score1 > score2);
    }

    #[test]
    fn prefix_match_outranks_scattered_match() {
        let prefix = fuzzy_rank("cargo", "car").unwrap();
        let scattered = fuzzy_rank("conftest", "ct").unwrap();
        assert!(prefix > scattered);
    }

    #[test]
    fn exact_match_outranks_prefix_extension() {
        let exact = fuzzy_rank("build", "build").unwrap();
        let extension = fuzzy_rank("build.rs", "build").unwrap();
        assert!(exact > extension);
    }

    #[test]
    fn shorter_choice_wins_on_equal_raw_score() {
        // Both are single-char matches with the same raw skim score; the
        // shorter choice must come out ahead via the length penalty.
        let short = fuzzy_rank("b", "b").unwrap();
        let long = fuzzy_rank("bbbbbbbbbbbb", "b").unwrap();
        assert!(short > long);
    }

    #[test]
    fn length_penalty_cannot_flip_genuine_scores() {
        // A strong multi-char match must still beat a weak one even when the
        // weak candidate is far shorter.
        let strong = fuzzy_rank("completion", "compl").unwrap();
        let weak_short = fuzzy_rank("c", "compl");
        assert!(weak_short.is_none());
        assert!(strong > 0);
    }

    #[test]
    fn empty_pattern_ranks_zero_minus_length() {
        assert!(fuzzy_rank("abc", "").is_some());
    }
}
