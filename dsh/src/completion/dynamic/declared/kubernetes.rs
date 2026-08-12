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
        "kind.cluster" => collector.collect_kind_cluster_candidates(current_token, cached_only),
        "k3d.cluster" => collector.collect_k3d_cluster_candidates(current_token, cached_only),
        "minikube.profile" => {
            collector.collect_minikube_profile_candidates(current_token, cached_only)
        }
        "kubectl.context" => {
            collector.collect_kubectl_context_candidates(current_dir, current_token, cached_only)
        }
        "kubectl.namespace" => platform::collect_kubectl_declared(
            collector,
            provider,
            scope,
            parsed_command_line,
            current_dir,
            cached_only,
        ),
        "kubectl.resource_type" | "kubectl.resource_name" => platform::collect_kubectl_declared(
            collector,
            provider,
            scope,
            parsed_command_line,
            current_dir,
            cached_only,
        ),
        "helm.release" => collector.collect_helm_release_candidates(
            parsed_command_line,
            current_dir,
            current_token,
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
