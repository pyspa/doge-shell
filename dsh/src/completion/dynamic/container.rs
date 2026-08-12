use super::{
    CachePolicy, CompletionContext, DynamicCompletionProvider, EnhancedCandidate,
    ParsedCommandLine, dedup_sorted, docker_compose_words, selected_docker_compose_command,
    selected_docker_compose_file,
};
use std::path::Path;

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
        "docker.network" => collector.collect_container_object_candidates(
            "docker",
            "network",
            current_dir,
            current_token,
            "docker network",
            &["network", "ls", "--format", "{{.Name}}"],
            parse_non_empty_lines,
            cached_only,
        ),
        "docker.volume" => collector.collect_container_object_candidates(
            "docker",
            "volume",
            current_dir,
            current_token,
            "docker volume",
            &["volume", "ls", "--format", "{{.Name}}"],
            parse_non_empty_lines,
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
        "podman.network" => collector.collect_container_object_candidates(
            "podman",
            "network",
            current_dir,
            current_token,
            "podman network",
            &["network", "ls", "--format", "{{.Name}}"],
            parse_non_empty_lines,
            cached_only,
        ),
        "podman.volume" => collector.collect_container_object_candidates(
            "podman",
            "volume",
            current_dir,
            current_token,
            "podman volume",
            &["volume", "ls", "--format", "{{.Name}}"],
            parse_non_empty_lines,
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
