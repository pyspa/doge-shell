//! kubectl/helm dynamic completion: contexts, namespaces, resource types and
//! names (positional and by option), pods for `logs`/`exec`, and helm
//! releases scoped to the selected namespace/context.
use super::{
    CachePolicy, CompletionContext, DynamicCommandCacheKind, DynamicCompletionProvider,
    EnhancedCandidate, canonicalize_path, collect_command_lines, completion_words,
    parse_non_empty_lines, run_command_lines, runner,
};
use crate::completion::parser::ParsedCommandLine;
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
        "kubectl.context" => {
            collector.collect_kubectl_context_candidates(current_dir, current_token, cached_only)
        }
        "kubectl.namespace" | "kubectl.resource_type" | "kubectl.resource_name" => {
            platform::collect_kubectl_declared(
                collector,
                provider,
                scope,
                parsed_command_line,
                current_dir,
                cached_only,
            )
        }
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

impl DynamicCompletionProvider {
    pub(crate) fn collect_kubectl_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cache_policy: CachePolicy,
    ) -> Vec<EnhancedCandidate> {
        let cached_only = cache_policy.is_cached_only();
        let current_token = parsed_command_line.current_token.as_str();
        match &parsed_command_line.completion_context {
            CompletionContext::OptionValue { option_name, .. } => match option_name.as_str() {
                "--context" => {
                    self.collect_kubectl_context_candidates(current_dir, current_token, cached_only)
                }
                "-n" | "--namespace" => self.collect_kubectl_namespace_candidates(
                    current_dir,
                    current_token,
                    cached_only,
                ),
                _ => Vec::new(),
            },
            CompletionContext::SubCommand | CompletionContext::Argument { .. } => {
                let words = positional_words(parsed_command_line);
                if words.len() >= 2 && words[0] == "config" && words[1] == "use-context" {
                    self.collect_kubectl_context_candidates(current_dir, current_token, cached_only)
                } else if matches!(
                    words.first().copied(),
                    Some("get" | "describe" | "delete" | "edit" | "create" | "apply")
                ) {
                    let namespace = selected_namespace(parsed_command_line);
                    if let Some((resource, _)) = split_resource_name_token(current_token) {
                        self.collect_kubectl_resource_name_candidates_for_token(
                            current_dir,
                            resource,
                            current_token,
                            namespace,
                            cached_only,
                        )
                    } else if let Some(resource) = selected_resource(parsed_command_line) {
                        if resource == current_token {
                            self.collect_kubectl_resource_type_candidates(
                                current_dir,
                                current_token,
                                cached_only,
                            )
                        } else {
                            self.collect_kubectl_resource_name_candidates_for_token(
                                current_dir,
                                resource,
                                current_token,
                                namespace,
                                cached_only,
                            )
                        }
                    } else {
                        self.collect_kubectl_resource_type_candidates(
                            current_dir,
                            current_token,
                            cached_only,
                        )
                    }
                } else if matches!(words.first().copied(), Some("logs" | "exec")) {
                    self.collect_kubectl_pod_candidates(current_dir, current_token, cached_only)
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        }
    }

    fn collect_helm_release_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("helm");
        let namespace = selected_namespace(parsed_command_line).map(str::to_string);
        let kube_context = selected_helm_context(parsed_command_line).map(str::to_string);
        let value_kind = format!(
            "release:{}:{}",
            namespace.as_deref().unwrap_or("_"),
            kube_context.as_deref().unwrap_or("_")
        );
        let current_dir = current_dir.to_path_buf();
        self.collect_cached_value_candidates(
            "helm",
            &value_kind,
            current_dir.clone(),
            current_token,
            "Helm release",
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                let mut command = runner::command(&command_path);
                command.arg("list").arg("--short").current_dir(&current_dir);
                if let Some(namespace) = namespace.as_deref() {
                    command.arg("--namespace").arg(namespace);
                }
                if let Some(kube_context) = kube_context.as_deref() {
                    command.arg("--kube-context").arg(kube_context);
                }
                Ok(parse_non_empty_lines(&collect_command_lines(command)?))
            },
        )
    }
    fn collect_kubectl_resource_type_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("kubectl");
        let current_dir = current_dir.to_path_buf();
        self.collect_cached_value_candidates(
            "kubectl",
            "resource-type",
            canonicalize_path(&current_dir),
            current_token,
            "kubectl resource",
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                Ok(run_command_lines(
                    &command_path,
                    &["api-resources", "--namespaced=true", "-o", "name"],
                    &current_dir,
                )?
                .into_iter()
                .filter_map(|resource| resource.split('/').next().map(str::to_string))
                .collect())
            },
        )
    }
    fn collect_kubectl_resource_name_candidates(
        &self,
        current_dir: &Path,
        resource: &str,
        current_token: &str,
        namespace: Option<&str>,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("kubectl");
        let current_dir = current_dir.to_path_buf();
        let resource = resource.to_string();
        let namespace = namespace.map(str::to_string);
        let value_kind = namespace
            .as_deref()
            .map(|namespace| format!("resource-name:{namespace}:{resource}"))
            .unwrap_or_else(|| format!("resource-name:{resource}"));
        self.collect_cached_value_candidates(
            "kubectl",
            &value_kind,
            canonicalize_path(&current_dir),
            current_token,
            "kubectl resource name",
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                let mut args = vec!["get"];
                if let Some(namespace) = namespace.as_deref() {
                    args.push("-n");
                    args.push(namespace);
                }
                args.push(&resource);
                args.push("-o");
                args.push("jsonpath={range .items[*]}{.metadata.name}{\"\\n\"}{end}");
                run_command_lines(&command_path, &args, &current_dir)
            },
        )
    }
    fn collect_kubectl_resource_name_candidates_for_token(
        &self,
        current_dir: &Path,
        resource: &str,
        current_token: &str,
        namespace: Option<&str>,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        if let Some((token_resource, name_prefix)) = split_resource_name_token(current_token) {
            return self
                .collect_kubectl_resource_name_candidates(
                    current_dir,
                    token_resource,
                    name_prefix,
                    namespace,
                    cached_only,
                )
                .into_iter()
                .map(|mut candidate| {
                    candidate.text = format!("{token_resource}/{}", candidate.text);
                    candidate
                })
                .collect();
        }

        self.collect_kubectl_resource_name_candidates(
            current_dir,
            resource,
            current_token,
            namespace,
            cached_only,
        )
    }
    fn collect_kubectl_pod_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("kubectl");
        let current_dir = current_dir.to_path_buf();
        self.collect_cached_value_candidates(
            "kubectl",
            "pod",
            canonicalize_path(&current_dir),
            current_token,
            "kubectl pod",
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                run_command_lines(
                    &command_path,
                    &[
                        "get",
                        "pods",
                        "-o",
                        "jsonpath={range .items[*]}{.metadata.name}{\"\\n\"}{end}",
                    ],
                    &current_dir,
                )
            },
        )
    }
    fn collect_kubectl_context_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("kubectl");
        self.collect_cached_command_candidates(
            DynamicCommandCacheKind::KubectlContext,
            canonicalize_path(current_dir),
            current_token,
            "kubectl context",
            cached_only,
            {
                let current_dir = current_dir.to_path_buf();
                move || {
                    let Some(command_path) = command_path else {
                        return Ok(Vec::new());
                    };

                    run_command_lines(
                        &command_path,
                        &["config", "get-contexts", "-o", "name"],
                        &current_dir,
                    )
                }
            },
        )
    }
    fn collect_kubectl_namespace_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("kubectl");
        self.collect_cached_command_candidates(
            DynamicCommandCacheKind::KubectlNamespace,
            canonicalize_path(current_dir),
            current_token,
            "kubectl namespace",
            cached_only,
            {
                let current_dir = current_dir.to_path_buf();
                move || {
                    let Some(command_path) = command_path else {
                        return Ok(Vec::new());
                    };

                    run_command_lines(
                        &command_path,
                        &[
                            "get",
                            "namespaces",
                            "-o",
                            "jsonpath={range .items[*]}{.metadata.name}{\"\\n\"}{end}",
                        ],
                        &current_dir,
                    )
                }
            },
        )
    }
}

pub(super) fn positional_words(parsed_command_line: &ParsedCommandLine) -> Vec<&str> {
    let mut positionals = Vec::new();
    let mut skip_next_value = false;
    for token in completion_words(parsed_command_line) {
        if skip_next_value {
            skip_next_value = false;
            continue;
        }
        if option_takes_value(token) {
            skip_next_value = true;
            continue;
        }
        if is_inline_option_value(token) || token.starts_with('-') {
            continue;
        }
        positionals.push(token);
    }
    positionals
}

pub(super) fn selected_resource(parsed_command_line: &ParsedCommandLine) -> Option<&str> {
    let current_token = parsed_command_line.current_token.as_str();
    let words = positional_words(parsed_command_line);
    let command = words.first().copied()?;
    if !matches!(
        command,
        "get" | "describe" | "delete" | "edit" | "create" | "apply"
    ) {
        return None;
    }
    let resource = words.get(1).copied()?;
    if resource == current_token || resource.contains('/') {
        return None;
    }
    Some(resource)
}

pub(super) fn selected_namespace(parsed_command_line: &ParsedCommandLine) -> Option<&str> {
    selected_option_value(parsed_command_line, &["-n", "--namespace"])
}

fn selected_option_value<'a>(
    parsed_command_line: &'a ParsedCommandLine,
    option_names: &[&str],
) -> Option<&'a str> {
    let words = completion_words(parsed_command_line);
    for (index, token) in words.iter().enumerate() {
        if option_names.contains(token) {
            let Some(value) = words.get(index + 1).copied() else {
                continue;
            };
            if !value.is_empty() && !value.starts_with('-') {
                return Some(value);
            }
        }
        for option_name in option_names {
            if let Some(value) = token
                .strip_prefix(option_name)
                .and_then(|value| value.strip_prefix('='))
                .filter(|value| !value.is_empty())
            {
                return Some(value);
            }
            if *option_name == "-n"
                && let Some(value) = token.strip_prefix("-n").filter(|value| !value.is_empty())
            {
                return Some(value);
            }
        }
    }
    None
}

pub(super) fn selected_helm_context(parsed_command_line: &ParsedCommandLine) -> Option<&str> {
    let words = completion_words(parsed_command_line);
    for (index, token) in words.iter().enumerate() {
        if *token == "--kube-context" {
            let Some(value) = words.get(index + 1).copied() else {
                continue;
            };
            if !value.is_empty() && !value.starts_with('-') {
                return Some(value);
            }
        }
        if let Some(value) = token.strip_prefix("--kube-context=")
            && !value.is_empty()
        {
            return Some(value);
        }
    }
    None
}

pub(super) fn split_resource_name_token(token: &str) -> Option<(&str, &str)> {
    let (resource, name_prefix) = token.split_once('/')?;
    if resource.is_empty() {
        return None;
    }
    Some((resource, name_prefix))
}

fn option_takes_value(token: &str) -> bool {
    matches!(
        token,
        "-n" | "--namespace"
            | "--context"
            | "--kubeconfig"
            | "-o"
            | "--output"
            | "-l"
            | "--selector"
            | "--field-selector"
            | "-f"
            | "--filename"
            | "-k"
            | "--kustomize"
            | "--as"
            | "--as-group"
            | "--cluster"
            | "--server"
            | "--token"
            | "--user"
    )
}

fn is_inline_option_value(token: &str) -> bool {
    token.starts_with("--namespace=")
        || token.starts_with("-n=")
        || (token.starts_with("-n") && token.len() > 2)
        || token.starts_with("--context=")
        || token.starts_with("--kubeconfig=")
        || token.starts_with("--output=")
        || token.starts_with("--selector=")
        || token.starts_with("--field-selector=")
        || token.starts_with("--filename=")
        || token.starts_with("--kustomize=")
        || token.starts_with("--as=")
        || token.starts_with("--as-group=")
        || token.starts_with("--cluster=")
        || token.starts_with("--server=")
        || token.starts_with("--token=")
        || token.starts_with("--user=")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion::parser::CommandLineParser;

    #[test]
    fn kubectl_context_parser_keeps_resource_and_namespace_separate() {
        let input = "kubectl get pods --namespace=staging ";
        let parsed = CommandLineParser::new().parse(input, input.len());

        assert_eq!(selected_resource(&parsed), Some("pods"));
        assert_eq!(selected_namespace(&parsed), Some("staging"));
        assert_eq!(split_resource_name_token("pod/api"), Some(("pod", "api")));
    }
}
