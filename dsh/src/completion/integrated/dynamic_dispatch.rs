//! Handing a parsed command line to the dynamic providers: choosing which
//! command the providers key off (after unwrapping a wrapper), the declared
//! and cached-only variants, and the argument type the context implies.
use super::*;

impl IntegratedCompletionEngine {
    /// The command line the dynamic providers should be keyed off.
    ///
    /// `sudo pacman -R <TAB>` parses with `command == "sudo"`, and every
    /// dynamic provider lookup keys off the command name, so the pacman package
    /// provider never ran and the completion fell through to the fish fallback
    /// (which lists sync-repository packages only, dropping AUR packages).
    /// Unwrapping here makes the wrapped command reach its provider.
    pub(super) fn dynamic_provider_target<'a>(
        &self,
        parsed: &'a parser::ParsedCommandLine,
    ) -> Cow<'a, parser::ParsedCommandLine> {
        let mut current = Cow::Borrowed(parsed);

        for _ in 0..MAX_COMMAND_WRAPPER_DEPTH {
            let Some(inner) = self.unwrap_command_with_args_once(&current) else {
                break;
            };
            // Each unwrap must consume the wrapper's own tokens. Bail out rather
            // than spin if a malformed definition ever breaks that.
            if inner.raw_args.len() >= current.raw_args.len() {
                break;
            }
            current = Cow::Owned(inner);
        }

        current
    }

    pub(super) fn collect_dynamic_candidates_for(
        &self,
        request: &CompletionRequest,
        parsed_command_line: &parser::ParsedCommandLine,
        cache_policy: CachePolicy,
    ) -> Vec<EnhancedCandidate> {
        let mut candidates = DYNAMIC_PROVIDER_SPECS
            .iter()
            .find(|provider| provider.command == parsed_command_line.command)
            .map(|provider| (provider.collect)(self, request, parsed_command_line, cache_policy))
            .unwrap_or_default();
        candidates.extend(self.collect_declared_dynamic_candidates(
            request,
            parsed_command_line,
            cache_policy,
        ));
        candidates
    }

    pub(super) fn collect_dynamic_candidates(
        &self,
        request: &CompletionRequest,
        parsed_command_line: &parser::ParsedCommandLine,
    ) -> CandidateBatch {
        let target = self.dynamic_provider_target(parsed_command_line);
        let mut candidates =
            self.collect_dynamic_candidates_for(request, &target, CachePolicy::RefreshInBackground);

        // `git checkout`/`git restore` accept BOTH refs and working-tree paths.
        // The dynamic provider only yields branches, and these subcommands are
        // treated as exclusive (so later file stages never run), which would
        // otherwise make `git checkout <file>` impossible to complete. Merge in
        // file/directory candidates so both branches and paths are offered.
        if git_subcommand_accepts_paths(&target) {
            candidates.extend(self.file_candidates_for_token(&target.current_token));
        }

        if dynamic_candidates_are_exclusive(&target) {
            CandidateBatch::exclusive_with_framework(candidates, CompletionFrameworkKind::Skim)
        } else if candidates.is_empty() {
            CandidateBatch::empty()
        } else {
            CandidateBatch::inclusive_with_framework(candidates, CompletionFrameworkKind::Skim)
        }
    }

    /// File/directory candidates for the current token, converted to the
    /// engine's `EnhancedCandidate` form. Errors are swallowed (completion is
    /// best-effort) and yield an empty list.
    pub(super) fn file_candidates_for_token(&self, current_token: &str) -> Vec<EnhancedCandidate> {
        match FileSystemGenerator::generate_file_candidates(current_token) {
            Ok(candidates) => candidates
                .into_iter()
                .map(|c| self.convert_to_enhanced_candidate(c))
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    pub(super) fn collect_declared_dynamic_candidates(
        &self,
        request: &CompletionRequest,
        parsed_command_line: &parser::ParsedCommandLine,
        cache_policy: CachePolicy,
    ) -> Vec<EnhancedCandidate> {
        let Some(ArgumentType::Dynamic { provider, scope }) =
            self.argument_type_for_completion_context(parsed_command_line)
        else {
            return Vec::new();
        };

        if provider == "shell.job" {
            return self.collect_shell_job_candidates(&parsed_command_line.current_token);
        }

        self.dynamic.collect_declared_dynamic_candidates(
            &provider,
            scope.as_deref(),
            parsed_command_line,
            request.current_dir,
            cache_policy,
        )
    }

    pub(super) fn argument_type_for_completion_context(
        &self,
        parsed_command_line: &parser::ParsedCommandLine,
    ) -> Option<ArgumentType> {
        self.ensure_command_completion_loaded(&parsed_command_line.command);
        let db_lock = self.command_completion.lock();
        argument_type_for_completion_context(&db_lock, parsed_command_line)
    }

    pub(super) fn collect_dynamic_candidates_cached(
        &self,
        request: &CompletionRequest,
        parsed_command_line: &parser::ParsedCommandLine,
    ) -> Vec<EnhancedCandidate> {
        let target = self.dynamic_provider_target(parsed_command_line);
        self.collect_dynamic_candidates_for(request, &target, CachePolicy::CachedOnly)
    }
}
