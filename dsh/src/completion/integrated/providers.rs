//! The command-name -> dynamic-provider dispatch table
//! (`DYNAMIC_PROVIDER_SPECS`) and one adapter function per entry, each
//! normalizing a provider-specific method on `IntegratedCompletionEngine`/
//! `DynamicCompletionProvider` to the shared `DynamicProviderFn` signature.
//!
//! This is the command-name-keyed dynamic-provider path
//! (`invariants.md`'s "Completion 定義"); it runs before, and its results are
//! `extend`ed by, the declarative JSON `Dynamic` providers.
use super::*;
use dsh_types::mcp::McpTransport;

pub(super) type DynamicProviderFn = for<'a> fn(
    &IntegratedCompletionEngine,
    &CompletionRequest<'a>,
    &ParsedCommandLine,
    CachePolicy,
) -> Vec<EnhancedCandidate>;

pub(super) struct DynamicProviderSpec {
    pub(super) command: &'static str,
    pub(super) collect: DynamicProviderFn,
}

pub(super) const DYNAMIC_PROVIDER_SPECS: &[DynamicProviderSpec] = &[
    DynamicProviderSpec {
        command: "task",
        collect: collect_task_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "pm",
        collect: collect_pm_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "pj",
        collect: collect_pj_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "mcp",
        collect: collect_mcp_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "skill",
        collect: collect_skill_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "git",
        collect: collect_git_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "docker",
        collect: collect_docker_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "kubectl",
        collect: collect_kubectl_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "cargo",
        collect: collect_cargo_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "systemctl",
        collect: collect_systemctl_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "journalctl",
        collect: collect_journalctl_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "ssh",
        collect: collect_ssh_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "scp",
        collect: collect_scp_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "rsync",
        collect: collect_rsync_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "tmux",
        collect: collect_tmux_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "screen",
        collect: collect_screen_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "pgrep",
        collect: collect_pgrep_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "pkill",
        collect: collect_pkill_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "pip",
        collect: collect_pip_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "pip3",
        collect: collect_pip3_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "rustup",
        collect: collect_rustup_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "gh",
        collect: collect_gh_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "nmcli",
        collect: collect_nmcli_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "pacman",
        collect: collect_pacman_dynamic_candidates,
    },
    // AUR helpers take the same operation flags and share the pacman package
    // provider in their JSON definitions, so they need the same bundled-flag
    // handling (`yay -Rns`, `paru -Syu`).
    DynamicProviderSpec {
        command: "yay",
        collect: collect_pacman_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "paru",
        collect: collect_pacman_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "mount",
        collect: collect_mount_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "umount",
        collect: collect_umount_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "modprobe",
        collect: collect_modprobe_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "tcpdump",
        collect: collect_tcpdump_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "npm",
        collect: collect_npm_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "pnpm",
        collect: collect_npm_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "yarn",
        collect: collect_yarn_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "deno",
        collect: collect_deno_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "just",
        collect: collect_just_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "make",
        collect: collect_make_dynamic_candidates,
    },
];

pub(super) fn collect_task_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    if cache_policy.is_cached_only() {
        Vec::new()
    } else {
        engine
            .dynamic
            .collect_task_candidates(parsed, request.current_dir, cache_policy)
    }
}

pub(super) fn collect_pm_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    _request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    if cache_policy.is_cached_only() {
        Vec::new()
    } else {
        engine.collect_pm_candidates(parsed)
    }
}

pub(super) fn collect_pj_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    _request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    if cache_policy.is_cached_only() {
        Vec::new()
    } else {
        engine.collect_pj_candidates(parsed)
    }
}

/// `skill show|path|remove|archive|unarchive|pin|unpin <TAB>` (skill names)
/// and `skill diff|approve|reject <TAB>` (pending proposal ids).
///
/// Both are chosen by the model, not by the person typing, so without this
/// the only way to learn either is to run `skill list`/`skill pending` first.
pub(super) fn collect_skill_dynamic_candidates(
    _engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    use parser::CompletionContext;

    // Reading two directories is not work to do on a cached-only pass.
    if cache_policy.is_cached_only() {
        return Vec::new();
    }
    if !matches!(
        parsed.completion_context,
        CompletionContext::Argument { .. }
    ) {
        return Vec::new();
    }
    let Some(subcommand) = parsed.subcommand_path.first() else {
        return Vec::new();
    };
    let current_token = parsed.current_token.as_str();

    if matches!(subcommand.as_str(), "diff" | "approve" | "reject") {
        return dsh_builtin::pending_proposal_ids()
            .into_iter()
            .filter(|(id, _)| matches_prefix(current_token, id))
            .map(|(id, summary)| EnhancedCandidate {
                text: id,
                description: Some(summary),
                candidate_type: CandidateType::Argument,
                priority: 90,
            })
            .collect();
    }

    if !matches!(
        subcommand.as_str(),
        "show" | "path" | "remove" | "rm" | "archive" | "unarchive" | "pin" | "unpin"
    ) {
        return Vec::new();
    }

    dsh_builtin::installed_skill_names(Some(request.current_dir))
        .into_iter()
        .filter(|(name, _)| matches_prefix(current_token, name))
        .map(|(name, summary)| EnhancedCandidate {
            text: name,
            description: Some(summary),
            candidate_type: CandidateType::Argument,
            priority: 90,
        })
        .collect()
}

pub(super) fn collect_mcp_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    _request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    if cache_policy.is_cached_only() {
        Vec::new()
    } else {
        engine.collect_mcp_candidates(parsed)
    }
}

pub(super) fn collect_git_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_git_candidates(parsed, request.current_dir, cache_policy)
}

pub(super) fn collect_docker_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_docker_candidates(parsed, request.current_dir, cache_policy)
}

pub(super) fn collect_kubectl_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_kubectl_candidates(parsed, request.current_dir, cache_policy)
}

pub(super) fn collect_cargo_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_cargo_candidates(parsed, request.current_dir, cache_policy)
}

pub(super) fn collect_systemctl_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_systemctl_candidates(parsed, request.current_dir, cache_policy)
}

pub(super) fn collect_journalctl_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_journalctl_candidates(parsed, request.current_dir, cache_policy)
}

pub(super) fn collect_ssh_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_ssh_host_candidates(parsed, request.current_dir, "ssh", cache_policy)
}

pub(super) fn collect_scp_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_ssh_host_candidates(parsed, request.current_dir, "scp", cache_policy)
}

pub(super) fn collect_rsync_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_ssh_host_candidates(parsed, request.current_dir, "rsync", cache_policy)
}

pub(super) fn collect_tmux_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_tmux_candidates(parsed, request.current_dir, cache_policy)
}

pub(super) fn collect_screen_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_screen_candidates(parsed, request.current_dir, cache_policy)
}

pub(super) fn collect_pgrep_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    _request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_process_name_candidates(parsed, "pgrep", cache_policy)
}

pub(super) fn collect_pkill_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    _request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_process_name_candidates(parsed, "pkill", cache_policy)
}

pub(super) fn collect_pip_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_pip_candidates(parsed, request.current_dir, "pip", cache_policy)
}

pub(super) fn collect_pip3_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_pip_candidates(parsed, request.current_dir, "pip3", cache_policy)
}

pub(super) fn collect_rustup_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_rustup_candidates(parsed, request.current_dir, cache_policy)
}

pub(super) fn collect_gh_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_gh_candidates(parsed, request.current_dir, cache_policy)
}

pub(super) fn collect_nmcli_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_nmcli_candidates(parsed, request.current_dir, cache_policy)
}

pub(super) fn collect_pacman_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_pacman_candidates(parsed, request.current_dir, cache_policy)
}

pub(super) fn collect_mount_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_mount_candidates(parsed, request.current_dir, cache_policy)
}

pub(super) fn collect_umount_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_umount_candidates(parsed, request.current_dir, cache_policy)
}

pub(super) fn collect_modprobe_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    _request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_modprobe_candidates(parsed, cache_policy)
}

pub(super) fn collect_tcpdump_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    _request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_tcpdump_candidates(parsed, cache_policy)
}

pub(super) fn collect_npm_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    let mut candidates = if cache_policy.is_cached_only() {
        Vec::new()
    } else {
        engine.collect_package_run_candidates(parsed, request.current_dir)
    };
    let completes_dependency = match parsed.command.as_str() {
        "pnpm" => {
            leading_completion_words_match(parsed, &["remove"])
                || leading_completion_words_match(parsed, &["update"])
                || leading_completion_words_match(parsed, &["why"])
        }
        _ => {
            leading_completion_words_match(parsed, &["uninstall"])
                || leading_completion_words_match(parsed, &["update"])
        }
    };
    if completes_dependency {
        candidates.extend(engine.dynamic.collect_js_dependency_candidates(
            parsed,
            request.current_dir,
            parsed.command.as_str(),
            cache_policy.is_cached_only(),
        ));
    }
    candidates
}

pub(super) fn collect_yarn_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    let mut candidates = if cache_policy.is_cached_only() {
        Vec::new()
    } else {
        engine.collect_yarn_script_candidates(parsed, request.current_dir)
    };
    if leading_completion_words_match(parsed, &["remove"])
        || leading_completion_words_match(parsed, &["why"])
        || leading_completion_words_match(parsed, &["upgrade"])
    {
        candidates.extend(engine.dynamic.collect_js_dependency_candidates(
            parsed,
            request.current_dir,
            "yarn",
            cache_policy.is_cached_only(),
        ));
    }
    candidates
}

pub(super) fn collect_deno_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    if cache_policy.is_cached_only() {
        Vec::new()
    } else {
        engine.collect_deno_task_candidates(parsed, request.current_dir)
    }
}

pub(super) fn collect_just_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    if cache_policy.is_cached_only() {
        Vec::new()
    } else {
        engine.collect_top_level_task_candidates(parsed, request.current_dir, JUST_TASK_SOURCES)
    }
}

pub(super) fn collect_make_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    if cache_policy.is_cached_only() {
        Vec::new()
    } else {
        engine.collect_top_level_task_candidates(parsed, request.current_dir, MAKE_TASK_SOURCES)
    }
}

pub(super) fn leading_completion_words(parsed: &ParsedCommandLine) -> Vec<&str> {
    let mut words: Vec<&str> = if parsed.subcommand_path.is_empty() {
        parsed
            .specified_arguments
            .iter()
            .map(String::as_str)
            .collect()
    } else {
        parsed.subcommand_path.iter().map(String::as_str).collect()
    };

    if words.last().copied() == Some(parsed.current_token.as_str()) {
        words.pop();
    }

    words
}

pub(super) fn leading_completion_words_match(
    parsed: &ParsedCommandLine,
    expected: &[&str],
) -> bool {
    leading_completion_words(parsed).as_slice() == expected
}

pub(super) fn pm_subcommand_candidates(current_token: &str) -> Vec<EnhancedCandidate> {
    let items = [
        ("init", "Register the current project root"),
        ("status", "Show current project status"),
        ("st", "Alias for status"),
        ("add", "Register a project"),
        ("list", "List registered projects"),
        ("ls", "Alias for list"),
        ("remove", "Remove a project"),
        ("rm", "Alias for remove"),
        ("work", "Switch to a project"),
        ("jump", "Select a project interactively"),
        ("activate", "Activate current project environment"),
    ];

    items
        .iter()
        .filter(|(name, _)| matches_prefix(current_token, name))
        .map(|(name, desc)| EnhancedCandidate {
            text: (*name).to_string(),
            description: Some((*desc).to_string()),
            candidate_type: CandidateType::SubCommand,
            priority: 110,
        })
        .collect()
}

pub(super) fn mcp_subcommand_candidates(current_token: &str) -> Vec<EnhancedCandidate> {
    let items = [
        ("status", "Show connection status"),
        ("s", "Alias for status"),
        ("connect", "Connect to a MCP server"),
        ("c", "Alias for connect"),
        ("disconnect", "Disconnect a MCP server"),
        ("d", "Alias for disconnect"),
        ("list", "List registered MCP servers"),
        ("l", "Alias for list"),
        ("tools", "List MCP tools"),
        ("t", "Alias for tools"),
        ("help", "Show help"),
    ];

    items
        .iter()
        .filter(|(name, _)| matches_prefix(current_token, name))
        .map(|(name, desc)| EnhancedCandidate {
            text: (*name).to_string(),
            description: Some((*desc).to_string()),
            candidate_type: CandidateType::SubCommand,
            priority: 110,
        })
        .collect()
}

pub(super) fn mcp_description(server: &dsh_types::mcp::McpServerConfig) -> Option<String> {
    if let Some(description) = &server.description
        && !description.trim().is_empty()
    {
        return Some(description.clone());
    }

    match &server.transport {
        McpTransport::Stdio { command, .. } => Some(format!("stdio: {}", command)),
        McpTransport::Sse { url } => Some(format!("sse: {}", url)),
        McpTransport::Http { url, .. } => Some(format!("http: {}", url)),
    }
}
