use ::fuzzy_matcher::FuzzyMatcher;
use ::fuzzy_matcher::skim::SkimMatcherV2;
use std::sync::LazyLock;

/// Singleton fuzzy matcher to avoid repeated allocation
static FUZZY_MATCHER: LazyLock<SkimMatcherV2> = LazyLock::new(SkimMatcherV2::default);

pub fn fuzzy_match_score(choice: &str, pattern: &str) -> Option<i64> {
    if pattern.is_empty() {
        return Some(0);
    }
    FUZZY_MATCHER.fuzzy_match(choice, pattern)
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
}
