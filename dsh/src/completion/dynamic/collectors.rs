//! The collectors that build their own invocation before asking for values:
//! journalctl, tmux, screen, the process listings, pip, rustup, gh, nmcli and
//! the mount/umount/modprobe family. Each reads the parsed command line to
//! decide what to run, then goes through the cache engine for the result.
use super::*;

impl DynamicCompletionProvider {
    pub(crate) fn collect_journalctl_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cache_policy: CachePolicy,
    ) -> Vec<EnhancedCandidate> {
        let cached_only = cache_policy.is_cached_only();
        let CompletionContext::OptionValue { option_name, .. } =
            &parsed_command_line.completion_context
        else {
            return Vec::new();
        };
        if !matches!(option_name.as_str(), "-u" | "--unit") {
            return Vec::new();
        }

        self.collect_systemd_unit_candidates(
            current_dir,
            parsed_command_line.current_token.as_str(),
            SystemdUnitQuery::new(
                SystemdUnitListKind::All,
                selected_systemd_manager_scope(parsed_command_line),
                None,
            ),
            "systemd unit",
            cached_only,
        )
    }

    pub(crate) fn collect_tmux_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cache_policy: CachePolicy,
    ) -> Vec<EnhancedCandidate> {
        let cached_only = cache_policy.is_cached_only();
        let completes_session = match &parsed_command_line.completion_context {
            CompletionContext::OptionValue { option_name, .. } => option_name == "-t",
            CompletionContext::SubCommand | CompletionContext::Argument { .. } => matches!(
                parsed_command_line
                    .subcommand_path
                    .first()
                    .map(String::as_str),
                Some("attach-session" | "attach" | "a" | "kill-session")
            ),
            _ => false,
        };
        if !completes_session {
            return Vec::new();
        }

        local::collect_by_id(
            self,
            "tmux.session",
            parsed_command_line,
            current_dir,
            cached_only,
        )
    }

    pub(crate) fn collect_screen_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cache_policy: CachePolicy,
    ) -> Vec<EnhancedCandidate> {
        let cached_only = cache_policy.is_cached_only();
        if !matches!(
            parsed_command_line.completion_context,
            CompletionContext::OptionValue { .. }
                | CompletionContext::SubCommand
                | CompletionContext::Argument { .. }
        ) {
            return Vec::new();
        }

        local::collect_by_id(
            self,
            "screen.session",
            parsed_command_line,
            current_dir,
            cached_only,
        )
    }

    pub(crate) fn collect_process_name_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        command_name: &str,
        cache_policy: CachePolicy,
    ) -> Vec<EnhancedCandidate> {
        let cached_only = cache_policy.is_cached_only();
        if !matches!(
            parsed_command_line.completion_context,
            CompletionContext::SubCommand | CompletionContext::Argument { .. }
        ) {
            return Vec::new();
        }
        self.collect_cached_value_candidates(
            command_name,
            "process-name",
            PathBuf::from("/proc"),
            parsed_command_line.current_token.as_str(),
            "process name",
            cached_only,
            || Ok(load_process_names()),
        )
    }

    pub(super) fn collect_process_pid_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        if !matches!(
            parsed_command_line.completion_context,
            CompletionContext::OptionValue { .. }
                | CompletionContext::SubCommand
                | CompletionContext::Argument { .. }
        ) {
            return Vec::new();
        }
        self.collect_cached_value_candidates(
            "system",
            "process-pid",
            PathBuf::from("/proc"),
            parsed_command_line.current_token.as_str(),
            "process id",
            cached_only,
            || Ok(load_process_ids()),
        )
    }

    pub(crate) fn collect_pip_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        command_name: &str,
        cache_policy: CachePolicy,
    ) -> Vec<EnhancedCandidate> {
        let cached_only = cache_policy.is_cached_only();
        if !matches!(
            parsed_command_line
                .subcommand_path
                .first()
                .map(String::as_str),
            Some("show" | "uninstall")
        ) || !matches!(
            parsed_command_line.completion_context,
            CompletionContext::SubCommand | CompletionContext::Argument { .. }
        ) {
            return Vec::new();
        }

        self.collect_pip_installed_package_candidates(
            current_dir,
            command_name,
            parsed_command_line.current_token.as_str(),
            cached_only,
        )
    }

    pub(crate) fn collect_rustup_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cache_policy: CachePolicy,
    ) -> Vec<EnhancedCandidate> {
        let cached_only = cache_policy.is_cached_only();
        let path = parsed_command_line
            .subcommand_path
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let completes_toolchain =
            matches!(path.as_slice(), ["default"] | ["toolchain", "uninstall"]);
        if !completes_toolchain {
            return Vec::new();
        }
        local::collect_by_id(
            self,
            "rustup.toolchain",
            parsed_command_line,
            current_dir,
            cached_only,
        )
    }

    pub(crate) fn collect_gh_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cache_policy: CachePolicy,
    ) -> Vec<EnhancedCandidate> {
        let cached_only = cache_policy.is_cached_only();
        let path = parsed_command_line
            .subcommand_path
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let (value_kind, args, description) = match path.as_slice() {
            [
                "pr",
                "view" | "checkout" | "close" | "merge" | "ready" | "diff" | "comment",
            ] => (
                "pr-number",
                vec!["pr", "list", "--json", "number", "--jq", ".[].number"],
                "GitHub pull request",
            ),
            ["issue", "view" | "close" | "reopen" | "comment"] => (
                "issue-number",
                vec!["issue", "list", "--json", "number", "--jq", ".[].number"],
                "GitHub issue",
            ),
            ["run", "view" | "watch" | "download" | "rerun" | "cancel"] => (
                "run-id",
                vec![
                    "run",
                    "list",
                    "--json",
                    "databaseId",
                    "--jq",
                    ".[].databaseId",
                ],
                "GitHub Actions run",
            ),
            _ => return Vec::new(),
        };
        let command_path = self.resolve_command_path("gh");
        let current_dir = current_dir.to_path_buf();
        self.collect_cached_value_candidates(
            "gh",
            value_kind,
            self.cached_project_root(&current_dir),
            parsed_command_line.current_token.as_str(),
            description,
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                run_command_lines(&command_path, &args, &current_dir)
            },
        )
    }

    pub(crate) fn collect_nmcli_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cache_policy: CachePolicy,
    ) -> Vec<EnhancedCandidate> {
        let cached_only = cache_policy.is_cached_only();
        let path = parsed_command_line
            .subcommand_path
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let (kind, args, description) = match path.as_slice() {
            ["connection", "up" | "modify" | "delete"] => (
                "connection",
                vec!["-t", "-f", "NAME", "connection", "show"],
                "NetworkManager connection",
            ),
            ["connection", "down"] => (
                "active-connection",
                vec!["-t", "-f", "NAME", "connection", "show", "--active"],
                "active NetworkManager connection",
            ),
            ["device", "show" | "connect"] => (
                "device",
                vec!["-t", "-f", "DEVICE", "device"],
                "NetworkManager device",
            ),
            ["device", "disconnect"] => (
                "connected-device",
                vec!["-t", "-f", "DEVICE,STATE", "device", "status"],
                "connected NetworkManager device",
            ),
            _ => return Vec::new(),
        };
        let parser = if kind == "connected-device" {
            parse_nmcli_connected_devices
        } else {
            parse_nmcli_first_field
        };
        let spec = NmcliCompletionSpec {
            kind,
            args: &args,
            description,
            parser,
        };
        self.collect_nmcli_value_candidates(
            current_dir,
            parsed_command_line.current_token.as_str(),
            spec,
            cached_only,
        )
    }

    pub(crate) fn collect_mount_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cache_policy: CachePolicy,
    ) -> Vec<EnhancedCandidate> {
        let cached_only = cache_policy.is_cached_only();
        if !matches!(
            parsed_command_line.completion_context,
            CompletionContext::SubCommand | CompletionContext::Argument { .. }
        ) {
            return Vec::new();
        }

        let mut candidates = local::collect_by_id(
            self,
            "block.device",
            parsed_command_line,
            current_dir,
            cached_only,
        );
        candidates.extend(local::collect_by_id(
            self,
            "fstab.mountpoint",
            parsed_command_line,
            current_dir,
            cached_only,
        ));
        candidates
    }

    pub(crate) fn collect_umount_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cache_policy: CachePolicy,
    ) -> Vec<EnhancedCandidate> {
        let cached_only = cache_policy.is_cached_only();
        if !matches!(
            parsed_command_line.completion_context,
            CompletionContext::SubCommand | CompletionContext::Argument { .. }
        ) {
            return Vec::new();
        }
        self.collect_mountpoint_candidates(
            current_dir,
            parsed_command_line.current_token.as_str(),
            cached_only,
        )
    }

    pub(crate) fn collect_modprobe_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        cache_policy: CachePolicy,
    ) -> Vec<EnhancedCandidate> {
        let cached_only = cache_policy.is_cached_only();
        if !matches!(
            parsed_command_line.completion_context,
            CompletionContext::SubCommand | CompletionContext::Argument { .. }
        ) {
            return Vec::new();
        }
        // `modprobe -r` unloads, so only modules already in the kernel apply.
        let scope = modprobe_removes_module(parsed_command_line).then_some("loaded");
        self.collect_kernel_module_candidates(
            scope,
            parsed_command_line.current_token.as_str(),
            cached_only,
        )
    }
}
