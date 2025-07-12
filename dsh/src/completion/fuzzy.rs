use crate::completion::display::Candidate;
use fuzzy_matcher::{FuzzyMatcher, skim::SkimMatcherV2};
use std::cmp::Ordering;

/// Fuzzy search completion with scoring
#[allow(dead_code)]
pub struct FuzzyCompletion {
    matcher: SkimMatcherV2,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ScoredCandidate {
    pub candidate: Candidate,
    pub score: i64,
    pub matched_indices: Vec<usize>,
}

#[allow(dead_code)]
impl FuzzyCompletion {
    pub fn new() -> Self {
        Self {
            matcher: SkimMatcherV2::default(),
        }
    }

    /// Filter and score candidates based on fuzzy matching
    pub fn filter_candidates(
        &self,
        candidates: Vec<Candidate>,
        query: &str,
    ) -> Vec<ScoredCandidate> {
        if query.is_empty() {
            return candidates
                .into_iter()
                .map(|candidate| ScoredCandidate {
                    candidate,
                    score: 0,
                    matched_indices: vec![],
                })
                .collect();
        }

        let mut scored_candidates: Vec<ScoredCandidate> = candidates
            .into_iter()
            .filter_map(|candidate| {
                let text = candidate.get_display_name();
                if let Some((score, indices)) = self.matcher.fuzzy_indices(text, query) {
                    Some(ScoredCandidate {
                        candidate,
                        score,
                        matched_indices: indices,
                    })
                } else {
                    None
                }
            })
            .collect();

        // Sort by score (higher is better)
        scored_candidates.sort_by(|a, b| b.score.cmp(&a.score));

        scored_candidates
    }

    /// Get the best matching candidate
    pub fn get_best_match(&self, candidates: Vec<Candidate>, query: &str) -> Option<Candidate> {
        let scored = self.filter_candidates(candidates, query);
        scored.into_iter().next().map(|sc| sc.candidate)
    }

    /// Filter candidates with a minimum score threshold
    pub fn filter_with_threshold(
        &self,
        candidates: Vec<Candidate>,
        query: &str,
        min_score: i64,
    ) -> Vec<ScoredCandidate> {
        self.filter_candidates(candidates, query)
            .into_iter()
            .filter(|sc| sc.score >= min_score)
            .collect()
    }
}

impl Default for FuzzyCompletion {
    fn default() -> Self {
        Self::new()
    }
}

impl PartialEq for ScoredCandidate {
    fn eq(&self, other: &Self) -> bool {
        self.score == other.score
    }
}

impl Eq for ScoredCandidate {}

impl PartialOrd for ScoredCandidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ScoredCandidate {
    fn cmp(&self, other: &Self) -> Ordering {
        // Higher scores come first
        other.score.cmp(&self.score)
    }
}

/// Smart completion that combines exact matches with fuzzy matches
#[allow(dead_code)]
pub struct SmartCompletion {
    fuzzy: FuzzyCompletion,
}

#[allow(dead_code)]
impl SmartCompletion {
    pub fn new() -> Self {
        Self {
            fuzzy: FuzzyCompletion::new(),
        }
    }

    /// Perform smart completion with prioritized results
    pub fn complete(&self, candidates: Vec<Candidate>, query: &str) -> Vec<Candidate> {
        if query.is_empty() {
            return candidates;
        }

        let mut exact_matches = Vec::new();
        let mut prefix_matches = Vec::new();
        let mut fuzzy_matches = Vec::new();

        for candidate in candidates {
            let text = candidate.get_display_name();
            let text_lower = text.to_lowercase();
            let query_lower = query.to_lowercase();

            if text_lower == query_lower {
                // Exact match (highest priority)
                exact_matches.push(candidate);
            } else if text_lower.starts_with(&query_lower) {
                // Prefix match (medium priority)
                prefix_matches.push(candidate);
            } else if let Some((score, _)) = self.fuzzy.matcher.fuzzy_indices(text, query) {
                // Fuzzy match (lowest priority)
                fuzzy_matches.push(ScoredCandidate {
                    candidate,
                    score,
                    matched_indices: vec![],
                });
            }
        }

        // Sort fuzzy matches by score
        fuzzy_matches.sort_by(|a, b| b.score.cmp(&a.score));

        // Combine results in priority order
        let mut result = Vec::new();
        result.extend(exact_matches);
        result.extend(prefix_matches);
        result.extend(fuzzy_matches.into_iter().map(|sc| sc.candidate));

        result
    }
}

impl Default for SmartCompletion {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fuzzy_completion() {
        let fuzzy = FuzzyCompletion::new();
        let candidates = vec![
            Candidate::Basic("git commit".to_string()),
            Candidate::Basic("git checkout".to_string()),
            Candidate::Basic("git clone".to_string()),
            Candidate::Basic("cargo build".to_string()),
        ];

        let results = fuzzy.filter_candidates(candidates, "gc");
        assert!(!results.is_empty());

        // Should match git commands
        let names: Vec<String> = results
            .iter()
            .map(|sc| sc.candidate.get_display_name().to_string())
            .collect();
        assert!(names.iter().any(|name| name.contains("git")));
    }

    #[test]
    fn test_smart_completion_priority() {
        let smart = SmartCompletion::new();
        let candidates = vec![
            Candidate::Basic("test".to_string()),
            Candidate::Basic("testing".to_string()),
            Candidate::Basic("contest".to_string()),
            Candidate::Basic("latest".to_string()),
        ];

        let results = smart.complete(candidates, "test");

        // Exact match should come first
        assert_eq!(results[0].get_display_name(), "test");
        // Prefix match should come second
        assert_eq!(results[1].get_display_name(), "testing");
    }

    #[test]
    fn test_empty_query() {
        let fuzzy = FuzzyCompletion::new();
        let candidates = vec![
            Candidate::Basic("one".to_string()),
            Candidate::Basic("two".to_string()),
        ];

        let results = fuzzy.filter_candidates(candidates.clone(), "");
        assert_eq!(results.len(), candidates.len());
    }
}
