//! Docker/Podman dynamic completion: images, containers (running or all),
//! and `docker compose`/`podman-compose` service names (cached, or freshly
//! parsed from the selected compose file).
use super::{
    CachePolicy, CandidateType, CompletionContext, DynamicCompletionProvider, EnhancedCandidate,
    ParsedCommandLine, canonicalize_path, dedup_sorted, docker_compose_words,
    file_metadata_signature, find_compose_file, matches_prefix, parse_non_empty_lines,
    run_command_lines, selected_docker_compose_command, selected_docker_compose_file,
};
use std::path::Path;
use tracing::warn;

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

    fn collect_docker_image_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        self.collect_container_image_candidates("docker", current_dir, current_token, cached_only)
    }
    fn collect_docker_container_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        include_stopped: bool,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        self.collect_container_container_candidates(
            "docker",
            current_dir,
            current_token,
            include_stopped,
            cached_only,
        )
    }
    fn collect_container_image_candidates(
        &self,
        executable: &'static str,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        self.collect_container_object_candidates(
            executable,
            "image",
            current_dir,
            current_token,
            &format!("{executable} image"),
            &["images", "--format", "{{.Repository}}:{{.Tag}}"],
            parse_images,
            cached_only,
        )
    }
    fn collect_container_container_candidates(
        &self,
        executable: &'static str,
        current_dir: &Path,
        current_token: &str,
        include_stopped: bool,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let current_dir = current_dir.to_path_buf();
        let value_kind = if include_stopped {
            "container-all"
        } else {
            "container-running"
        };
        let args: &'static [&'static str] = if include_stopped {
            &["ps", "-a", "--format", "{{.Names}}"]
        } else {
            &["ps", "--format", "{{.Names}}"]
        };
        self.collect_container_object_candidates(
            executable,
            value_kind,
            &current_dir,
            current_token,
            &format!("{executable} container"),
            args,
            parse_non_empty_lines,
            cached_only,
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn collect_container_object_candidates(
        &self,
        executable: &'static str,
        value_kind: &'static str,
        current_dir: &Path,
        current_token: &str,
        description: &str,
        args: &'static [&'static str],
        parser: fn(&[String]) -> Vec<String>,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path(executable);
        let current_dir = current_dir.to_path_buf();
        self.collect_cached_value_candidates(
            executable,
            value_kind,
            canonicalize_path(&current_dir),
            current_token,
            description,
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                Ok(parser(&run_command_lines(
                    &command_path,
                    args,
                    &current_dir,
                )?))
            },
        )
    }
    pub(super) fn collect_compose_service_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        compose_file_override: Option<&Path>,
    ) -> Vec<EnhancedCandidate> {
        match self.load_compose_services(current_dir, compose_file_override) {
            Ok(Some((compose_file, services))) => services
                .into_iter()
                .filter(|service| matches_prefix(current_token, service))
                .map(|service| EnhancedCandidate {
                    text: service,
                    description: Some(format!("compose service ({})", compose_file.display())),
                    candidate_type: CandidateType::Argument,
                    priority: 125,
                })
                .collect(),
            Ok(None) => Vec::new(),
            Err(err) => {
                warn!(
                    "Failed to load compose services from {:?}: {}",
                    current_dir, err
                );
                Vec::new()
            }
        }
    }
    fn collect_compose_service_candidates_cached(
        &self,
        current_dir: &Path,
        current_token: &str,
        compose_file_override: Option<&Path>,
    ) -> Vec<EnhancedCandidate> {
        let compose_file = if let Some(path) = compose_file_override {
            canonicalize_path(path)
        } else {
            let Some(compose_file) = find_compose_file(current_dir) else {
                return Vec::new();
            };
            canonicalize_path(&compose_file)
        };
        let signature = file_metadata_signature(&compose_file);
        self.lookup_compose_cache(&compose_file, &signature)
            .unwrap_or_default()
            .into_iter()
            .filter(|service| matches_prefix(current_token, service))
            .map(|service| EnhancedCandidate {
                text: service,
                description: Some(format!("compose service ({})", compose_file.display())),
                candidate_type: CandidateType::Argument,
                priority: 125,
            })
            .collect()
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
