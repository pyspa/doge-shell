//! Turning collected candidates into the answer: the scalar-option-value
//! narrowing, conversion to `EnhancedCandidate` and back to display
//! `Candidate`s, the result cache, and the dedup/sort/truncate pass.
use super::*;

impl IntegratedCompletionEngine {
    pub(super) fn scalar_option_value_context(
        &self,
        parsed_command_line: &ParsedCommandLine,
    ) -> bool {
        if !matches!(
            parsed_command_line.completion_context,
            parser::CompletionContext::OptionValue { .. }
        ) {
            return false;
        }

        matches!(
            self.argument_type_for_completion_context(parsed_command_line),
            Some(
                ArgumentType::String
                    | ArgumentType::Number
                    | ArgumentType::Url
                    | ArgumentType::Regex
            )
        )
    }

    /// Convert CompletionCandidate to EnhancedCandidate
    pub(super) fn convert_to_enhanced_candidate(
        &self,
        candidate: CompletionCandidate,
    ) -> EnhancedCandidate {
        EnhancedCandidate {
            text: candidate.text,
            description: candidate.description,
            candidate_type: match candidate.completion_type {
                crate::completion::command::CompletionType::SubCommand => CandidateType::SubCommand,
                crate::completion::command::CompletionType::ShortOption => {
                    CandidateType::ShortOption
                }
                crate::completion::command::CompletionType::LongOption => CandidateType::LongOption,
                crate::completion::command::CompletionType::Argument => CandidateType::Argument,
                crate::completion::command::CompletionType::File => CandidateType::File,
                crate::completion::command::CompletionType::Directory => CandidateType::Directory,
                crate::completion::command::CompletionType::Process => CandidateType::Process,
            },
            priority: candidate.priority,
        }
    }

    /// Convert EnhancedCandidate list to Candidate list for skim display
    pub fn to_candidates(&self, enhanced_candidates: Vec<EnhancedCandidate>) -> Vec<Candidate> {
        enhanced_candidates
            .into_iter()
            .map(|ec| ec.to_candidate())
            .collect()
    }

    pub(super) fn store_in_cache(
        &self,
        scope: u64,
        key: &str,
        candidates: &[EnhancedCandidate],
        framework: CompletionFrameworkKind,
    ) {
        if key.is_empty() || candidates.is_empty() {
            return;
        }
        debug!("scoped cache set for '{}'. len: {}", key, candidates.len());

        self.cache
            .set_scoped(scope, key.to_string(), candidates.to_vec());
        self.framework_cache
            .write()
            .insert((scope, key.to_string()), framework);
    }

    pub(super) fn lookup_cached_framework(
        &self,
        scope: u64,
        key: &str,
    ) -> Option<CompletionFrameworkKind> {
        self.framework_cache
            .read()
            .get(&(scope, key.to_string()))
            .copied()
    }

    /// Deduplication and sorting
    pub(super) fn deduplicate_and_sort(
        &self,
        mut candidates: Vec<EnhancedCandidate>,
        max_results: usize,
        history: Option<&Arc<parking_lot::Mutex<crate::history::History>>>,
        command_context: Option<&str>,
    ) -> Vec<EnhancedCandidate> {
        // Boost priority based on history
        if let Some(history_arc) = history
            && let Some(history) = history_arc.try_lock()
        {
            let boosts = history_boost_scores(&candidates, &history, command_context);
            for (candidate, score) in candidates.iter_mut().zip(boosts) {
                if score == 0 {
                    continue;
                }
                candidate.priority = candidate.priority.saturating_add(score);
            }
        }

        // Sorting before dedup keeps the best-ranked candidate for duplicate text.
        candidates.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| {
                    a.candidate_type
                        .sort_order()
                        .cmp(&b.candidate_type.sort_order())
                })
                .then_with(|| a.text.cmp(&b.text))
        });
        // Normalize away a trailing path separator before deduping: different
        // candidate sources (e.g. the JSON/FileSystemGenerator stage vs. the
        // fish-fallback stage) disagree on whether a directory's text ends in
        // `/`, so comparing raw text would let the same directory survive twice.
        // The candidate_type is kept as part of the key so this normalization
        // can't accidentally merge an unrelated candidate (e.g. a git branch or
        // history entry) that happens to share text with a trimmed directory.
        let mut seen = HashSet::with_capacity(candidates.len());
        candidates.retain(|candidate| {
            let key = candidate.text.trim_end_matches(['/', '\\']);
            seen.insert((candidate.candidate_type.clone(), key.to_string()))
        });

        candidates.truncate(max_results);
        candidates
    }
}
