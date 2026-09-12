use super::{
    CachePolicy, CompletionContext, DynamicCompletionProvider, EnhancedCandidate,
    ParsedCommandLine, dedup_sorted, docker_compose_words, selected_docker_compose_command,
    selected_docker_compose_file,
};
use std::path::Path;

/// This family's rows for `local::collect` - see `local` for what belongs
/// here. Table only; routing is unaffected by which family's table a
/// provider's row lives in.
pub(super) const LOCAL_SPECS: &[super::local::LocalSpec] = &[
    super::local::LocalSpec {
        provider: "docker.network",
        command_name: "docker",
        value_kind: "network",
        scope: super::local::Scope::CurrentDirCanonical,
        source: super::local::Source::Lines {
            executable: "docker",
            args: &["network", "ls", "--format", "{{.Name}}"],
            parser: super::parse_non_empty_lines,
        },
        description: "docker network",
    },
    super::local::LocalSpec {
        provider: "docker.volume",
        command_name: "docker",
        value_kind: "volume",
        scope: super::local::Scope::CurrentDirCanonical,
        source: super::local::Source::Lines {
            executable: "docker",
            args: &["volume", "ls", "--format", "{{.Name}}"],
            parser: super::parse_non_empty_lines,
        },
        description: "docker volume",
    },
    super::local::LocalSpec {
        provider: "podman.network",
        command_name: "podman",
        value_kind: "network",
        scope: super::local::Scope::CurrentDirCanonical,
        source: super::local::Source::Lines {
            executable: "podman",
            args: &["network", "ls", "--format", "{{.Name}}"],
            parser: super::parse_non_empty_lines,
        },
        description: "podman network",
    },
    super::local::LocalSpec {
        provider: "podman.volume",
        command_name: "podman",
        value_kind: "volume",
        scope: super::local::Scope::CurrentDirCanonical,
        source: super::local::Source::Lines {
            executable: "podman",
            args: &["volume", "ls", "--format", "{{.Name}}"],
            parser: super::parse_non_empty_lines,
        },
        description: "podman volume",
    },
];

pub(super) fn collect(
    collector: &super::DynamicCompletionProvider,
    request: &super::registry::DynamicProviderRequest<'_>,
) -> Option<Vec<super::EnhancedCandidate>> {
    use super::*;

    let provider = request.provider.as_str();
    let scope = request.scope;
    let parsed_command_line = request.parsed_command_line;
    let current_dir = request.current_dir;
    let cached_only = request.cache_policy.is_cached_only();
    let current_token = parsed_command_line.current_token.as_str();

    Some(match provider {
        "docker.image" => {
            collector.collect_docker_image_candidates(current_dir, current_token, cached_only)
        }
        "docker.container" => collector.collect_docker_container_candidates(
            current_dir,
            current_token,
            scope != Some("running"),
            cached_only,
        ),
        "docker.compose_service" => {
            let compose_file = selected_docker_compose_file(parsed_command_line, current_dir);
            if cached_only {
                collector.collect_compose_service_candidates_cached(
                    current_dir,
                    current_token,
                    compose_file.as_deref(),
                )
            } else {
                collector.collect_compose_service_candidates(
                    current_dir,
                    current_token,
                    compose_file.as_deref(),
                )
            }
        }
        "podman.image" => collector.collect_container_image_candidates(
            "podman",
            current_dir,
            current_token,
            cached_only,
        ),
        "podman.container" => collector.collect_container_container_candidates(
            "podman",
            current_dir,
            current_token,
            scope != Some("running"),
            cached_only,
        ),
        _ => {
            return platform::collect(
                collector,
                provider,
                parsed_command_line,
                current_dir,
                cached_only,
            );
        }
    })
}

impl DynamicCompletionProvider {
    pub(crate) fn collect_docker_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cache_policy: CachePolicy,
    ) -> Vec<EnhancedCandidate> {
        let cached_only = cache_policy.is_cached_only();
        let primary = parsed_command_line
            .subcommand_path
            .first()
            .map(String::as_str);

        let compose_invocation = primary == Some("compose")
            || (parsed_command_line.command == "docker"
                && !docker_compose_words(parsed_command_line).is_empty());

        if !compose_invocation {
            let Some(primary) = primary else {
                return Vec::new();
            };
            return self.collect_docker_object_candidates(
                primary,
                parsed_command_line,
                current_dir,
                cached_only,
            );
        }

        let Some(command_name) = selected_docker_compose_command(parsed_command_line) else {
            return Vec::new();
        };

        match parsed_command_line.completion_context {
            CompletionContext::SubCommand | CompletionContext::Argument { .. } => {
                let service_commands = [
                    "build", "cp", "create", "down", "exec", "kill", "logs", "pause", "port", "ps",
                    "pull", "push", "restart", "rm", "run", "scale", "start", "stop", "top",
                    "unpause", "up", "wait",
                ];

                if service_commands.contains(&command_name) {
                    let current_token = parsed_command_line.current_token.as_str();
                    let compose_file =
                        selected_docker_compose_file(parsed_command_line, current_dir);
                    if cached_only {
                        self.collect_compose_service_candidates_cached(
                            current_dir,
                            current_token,
                            compose_file.as_deref(),
                        )
                    } else {
                        self.collect_compose_service_candidates(
                            current_dir,
                            current_token,
                            compose_file.as_deref(),
                        )
                    }
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        }
    }

    fn collect_docker_object_candidates(
        &self,
        subcommand: &str,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        match parsed_command_line.completion_context {
            CompletionContext::SubCommand | CompletionContext::Argument { .. } => {}
            _ => return Vec::new(),
        }

        match subcommand {
            "run" | "rmi" | "push" | "tag" => self.collect_docker_image_candidates(
                current_dir,
                parsed_command_line.current_token.as_str(),
                cached_only,
            ),
            "stop" | "restart" | "kill" | "logs" | "exec" | "attach" | "top" => self
                .collect_docker_container_candidates(
                    current_dir,
                    parsed_command_line.current_token.as_str(),
                    false,
                    cached_only,
                ),
            "rm" | "start" => self.collect_docker_container_candidates(
                current_dir,
                parsed_command_line.current_token.as_str(),
                true,
                cached_only,
            ),
            "inspect" => {
                let mut candidates = self.collect_docker_container_candidates(
                    current_dir,
                    parsed_command_line.current_token.as_str(),
                    true,
                    cached_only,
                );
                candidates.extend(self.collect_docker_image_candidates(
                    current_dir,
                    parsed_command_line.current_token.as_str(),
                    cached_only,
                ));
                candidates
            }
            _ => Vec::new(),
        }
    }
}

pub(super) fn parse_images(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .map(|line| line.trim())
            .filter(|image| !image.contains("<none>"))
            .map(str::to_string)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_parser_deduplicates_and_ignores_dangling_images() {
        assert_eq!(
            parse_images(&[
                "repo/app:latest".to_string(),
                "<none>:<none>".to_string(),
                "repo/app:latest".to_string(),
            ]),
            vec!["repo/app:latest"]
        );
    }
}
