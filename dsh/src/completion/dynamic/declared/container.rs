pub(in super::super) fn collect(
    collector: &super::super::DynamicCompletionProvider,
    request: &super::super::registry::DynamicProviderRequest<'_>,
) -> Option<Vec<super::super::EnhancedCandidate>> {
    use super::super::*;

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
