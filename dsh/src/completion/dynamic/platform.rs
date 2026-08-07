use super::{
    CommandQueryPolicy, DynamicCompletionProvider, EnhancedCandidate, ParsedCommandLine,
    canonicalize_path, completion_words, dedup_sorted, runner,
};
use anyhow::Result;
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy)]
enum OutputParser {
    Lines,
    Whitespace,
    FirstColumn,
    JsonField(&'static str),
    JsonArrayValues,
    JsonObjectKeys,
    ResticSnapshots,
}

struct RemoteSpec {
    executable: &'static str,
    args: &'static [&'static str],
    parser: OutputParser,
    description: &'static str,
    context_options: &'static [&'static str],
}

pub(super) fn collect(
    provider: &DynamicCompletionProvider,
    provider_id: &str,
    parsed: &ParsedCommandLine,
    current_dir: &Path,
    cached_only: bool,
) -> Option<Vec<EnhancedCandidate>> {
    let spec = remote_spec(provider_id)?;
    let executable = if provider_id == "terraform.resource" {
        parsed.command.as_str()
    } else {
        spec.executable
    };
    let command_path = provider.resolve_command_path(executable);
    let (context_args, context_key) = selected_context_args(parsed, spec.context_options);
    let base_args = spec.args.iter().map(|arg| (*arg).to_string());
    let args = if provider_id == "terraform.resource" {
        context_args
            .into_iter()
            .chain(base_args)
            .collect::<Vec<_>>()
    } else {
        base_args.chain(context_args).collect::<Vec<_>>()
    };
    let workdir = current_dir.to_path_buf();
    let scope_dir = remote_scope(provider, provider_id, current_dir);
    let value_kind = if context_key.is_empty() {
        provider_id.to_string()
    } else {
        format!("{provider_id}:{context_key}")
    };
    let parser = spec.parser;

    Some(provider.collect_cached_value_candidates_with_policy(
        parsed.command.as_str(),
        &value_kind,
        scope_dir,
        parsed.current_token.as_str(),
        spec.description,
        cached_only,
        CommandQueryPolicy::REMOTE,
        move || {
            let Some(command_path) = command_path else {
                return Ok(Vec::new());
            };
            run_and_parse(&command_path, &args, &workdir, parser)
        },
    ))
}

pub(super) fn collect_kubectl_declared(
    provider: &DynamicCompletionProvider,
    provider_id: &str,
    scope: Option<&str>,
    parsed: &ParsedCommandLine,
    current_dir: &Path,
    cached_only: bool,
) -> Vec<EnhancedCandidate> {
    let mut candidate_prefix = None;
    let mut query_token = parsed.current_token.as_str();
    let mut resource_key = scope.unwrap_or("_").to_string();
    let (base_args, description, context_options): (Vec<String>, &str, &[&str]) = match provider_id
    {
        "kubectl.namespace" => (
            vec![
                "get".into(),
                "namespaces".into(),
                "-o".into(),
                "jsonpath={range .items[*]}{.metadata.name}{\"\\n\"}{end}".into(),
            ],
            "kubectl namespace",
            &["--context", "--kubeconfig"],
        ),
        "kubectl.resource_type" => (
            vec![
                "api-resources".into(),
                "--namespaced=true".into(),
                "-o".into(),
                "name".into(),
            ],
            "kubectl resource",
            &["--context", "--kubeconfig"],
        ),
        "kubectl.resource_name" => {
            let split_token = super::kubernetes::split_resource_name_token(query_token);
            let Some(resource) = scope
                .or_else(|| split_token.map(|(resource, _)| resource))
                .or_else(|| super::kubernetes::selected_resource(parsed))
            else {
                return Vec::new();
            };
            let mut args = vec!["get".into(), resource.to_string()];
            let namespace = super::kubernetes::selected_namespace(parsed);
            if let Some(namespace) = namespace {
                args.extend(["--namespace".into(), namespace.to_string()]);
            }
            resource_key = format!("{}:{resource}", namespace.unwrap_or("_"));
            if let Some((token_resource, name_prefix)) = split_token {
                candidate_prefix = Some(format!("{token_resource}/"));
                query_token = name_prefix;
            }
            args.extend([
                "-o".into(),
                "jsonpath={range .items[*]}{.metadata.name}{\"\\n\"}{end}".into(),
            ]);
            (
                args,
                "kubectl resource name",
                &["--context", "--kubeconfig"],
            )
        }
        _ => return Vec::new(),
    };

    let command_path = provider.resolve_command_path("kubectl");
    let (context_args, context_key) = selected_context_args(parsed, context_options);
    let mut args = base_args;
    args.extend(context_args);
    let value_kind = format!("{provider_id}:{context_key}:{resource_key}");
    let workdir = current_dir.to_path_buf();
    let candidates = provider.collect_cached_value_candidates_with_policy(
        "kubectl",
        &value_kind,
        canonicalize_path(current_dir),
        query_token,
        description,
        cached_only,
        CommandQueryPolicy::REMOTE,
        move || {
            let Some(command_path) = command_path else {
                return Ok(Vec::new());
            };
            run_and_parse(&command_path, &args, &workdir, OutputParser::Lines)
        },
    );
    if let Some(candidate_prefix) = candidate_prefix {
        candidates
            .into_iter()
            .map(|mut candidate| {
                candidate.text = format!("{candidate_prefix}{}", candidate.text);
                candidate
            })
            .collect()
    } else {
        candidates
    }
}

fn remote_scope(
    provider: &DynamicCompletionProvider,
    provider_id: &str,
    current_dir: &Path,
) -> PathBuf {
    if matches!(
        provider_id,
        "gh.issue"
            | "gh.pull_request"
            | "gh.run"
            | "gh.workflow"
            | "glab.issue"
            | "glab.merge_request"
            | "glab.pipeline"
            | "terraform.resource"
    ) {
        provider.cached_project_root(current_dir)
    } else {
        canonicalize_path(current_dir)
    }
}

fn remote_spec(provider: &str) -> Option<RemoteSpec> {
    let spec = match provider {
        "gh.repository" => RemoteSpec::new(
            "gh",
            &[
                "repo",
                "list",
                "--limit",
                "100",
                "--json",
                "nameWithOwner",
                "--jq",
                ".[].nameWithOwner",
            ],
            OutputParser::Lines,
            "GitHub repository",
            &[],
        ),
        "gh.pull_request" => RemoteSpec::new(
            "gh",
            &[
                "pr",
                "list",
                "--limit",
                "100",
                "--json",
                "number",
                "--jq",
                ".[].number",
            ],
            OutputParser::Lines,
            "GitHub pull request",
            &["-R", "--repo"],
        ),
        "gh.issue" => RemoteSpec::new(
            "gh",
            &[
                "issue",
                "list",
                "--limit",
                "100",
                "--json",
                "number",
                "--jq",
                ".[].number",
            ],
            OutputParser::Lines,
            "GitHub issue",
            &["-R", "--repo"],
        ),
        "gh.workflow" => RemoteSpec::new(
            "gh",
            &[
                "workflow", "list", "--limit", "100", "--json", "name", "--jq", ".[].name",
            ],
            OutputParser::Lines,
            "GitHub workflow",
            &["-R", "--repo"],
        ),
        "gh.run" => RemoteSpec::new(
            "gh",
            &[
                "run",
                "list",
                "--limit",
                "100",
                "--json",
                "databaseId",
                "--jq",
                ".[].databaseId",
            ],
            OutputParser::Lines,
            "GitHub Actions run",
            &["-R", "--repo"],
        ),
        "glab.project" => RemoteSpec::new(
            "glab",
            &["repo", "list", "--output", "json", "--per-page", "100"],
            OutputParser::JsonField("path_with_namespace"),
            "GitLab project",
            &[],
        ),
        "glab.merge_request" => RemoteSpec::new(
            "glab",
            &["mr", "list", "--output", "json", "--per-page", "100"],
            OutputParser::JsonField("iid"),
            "GitLab merge request",
            &["-R", "--repo"],
        ),
        "glab.issue" => RemoteSpec::new(
            "glab",
            &["issue", "list", "--output", "json", "--per-page", "100"],
            OutputParser::JsonField("iid"),
            "GitLab issue",
            &["-R", "--repo"],
        ),
        "glab.pipeline" => RemoteSpec::new(
            "glab",
            &["ci", "list", "--output", "json", "--per-page", "100"],
            OutputParser::JsonField("id"),
            "GitLab pipeline",
            &["-R", "--repo"],
        ),
        "argocd.context" => RemoteSpec::new(
            "argocd",
            &["context"],
            OutputParser::FirstColumn,
            "Argo CD context",
            &[],
        ),
        "argocd.application" => RemoteSpec::new(
            "argocd",
            &["app", "list", "--output", "name"],
            OutputParser::Lines,
            "Argo CD application",
            &["--argocd-context", "--context", "--server"],
        ),
        "argocd.project" => RemoteSpec::new(
            "argocd",
            &["proj", "list", "--output", "name"],
            OutputParser::Lines,
            "Argo CD project",
            &["--argocd-context", "--context", "--server"],
        ),
        "argocd.cluster" => RemoteSpec::new(
            "argocd",
            &["cluster", "list", "--output", "json"],
            OutputParser::JsonField("server"),
            "Argo CD cluster",
            &["--argocd-context", "--context", "--server"],
        ),
        "argocd.repository" => RemoteSpec::new(
            "argocd",
            &["repo", "list", "--output", "json"],
            OutputParser::JsonField("repo"),
            "Argo CD repository",
            &["--argocd-context", "--context", "--server"],
        ),
        "aws.region" => RemoteSpec::new(
            "aws",
            &[
                "ec2",
                "describe-regions",
                "--query",
                "Regions[].RegionName",
                "--output",
                "text",
            ],
            OutputParser::Whitespace,
            "AWS region",
            &["--profile"],
        ),
        "aws.s3_bucket" => RemoteSpec::new(
            "aws",
            &[
                "s3api",
                "list-buckets",
                "--query",
                "Buckets[].Name",
                "--output",
                "text",
            ],
            OutputParser::Whitespace,
            "S3 bucket",
            &["--profile", "--region"],
        ),
        "aws.eks_cluster" => RemoteSpec::new(
            "aws",
            &[
                "eks",
                "list-clusters",
                "--query",
                "clusters[]",
                "--output",
                "text",
            ],
            OutputParser::Whitespace,
            "EKS cluster",
            &["--profile", "--region"],
        ),
        "gcloud.account" => RemoteSpec::new(
            "gcloud",
            &["auth", "list", "--format=value(account)"],
            OutputParser::Lines,
            "Google Cloud account",
            &["--configuration"],
        ),
        "gcloud.compute_instance" => RemoteSpec::new(
            "gcloud",
            &["compute", "instances", "list", "--format=value(name)"],
            OutputParser::Lines,
            "Compute Engine instance",
            &["--account", "--configuration", "--project"],
        ),
        "gcloud.gke_cluster" => RemoteSpec::new(
            "gcloud",
            &["container", "clusters", "list", "--format=value(name)"],
            OutputParser::Lines,
            "GKE cluster",
            &["--account", "--configuration", "--project"],
        ),
        "az.resource_group" => RemoteSpec::new(
            "az",
            &["group", "list", "--query", "[].name", "--output", "tsv"],
            OutputParser::Lines,
            "Azure resource group",
            &["--subscription"],
        ),
        "az.vm" => RemoteSpec::new(
            "az",
            &["vm", "list", "--query", "[].name", "--output", "tsv"],
            OutputParser::Lines,
            "Azure virtual machine",
            &["--subscription"],
        ),
        "az.aks_cluster" => RemoteSpec::new(
            "az",
            &["aks", "list", "--query", "[].name", "--output", "tsv"],
            OutputParser::Lines,
            "AKS cluster",
            &["--subscription"],
        ),
        "terraform.resource" => RemoteSpec::new(
            "terraform",
            &["state", "list"],
            OutputParser::Lines,
            "Terraform state resource",
            &["-chdir"],
        ),
        "vault.policy" => RemoteSpec::new(
            "vault",
            &["policy", "list", "-format=json"],
            OutputParser::JsonArrayValues,
            "Vault policy",
            &["-address", "--address", "-namespace", "--namespace"],
        ),
        "vault.auth_method" => RemoteSpec::new(
            "vault",
            &["auth", "list", "-format=json"],
            OutputParser::JsonObjectKeys,
            "Vault auth method",
            &["-address", "--address", "-namespace", "--namespace"],
        ),
        "vault.secrets_engine" => RemoteSpec::new(
            "vault",
            &["secrets", "list", "-format=json"],
            OutputParser::JsonObjectKeys,
            "Vault secrets engine",
            &["-address", "--address", "-namespace", "--namespace"],
        ),
        "nomad.namespace" => RemoteSpec::new(
            "nomad",
            &["namespace", "list", "-json"],
            OutputParser::JsonField("Name"),
            "Nomad namespace",
            &["-address", "--address", "-region", "--region"],
        ),
        "nomad.job" => RemoteSpec::new(
            "nomad",
            &["job", "status", "-json"],
            OutputParser::JsonField("ID"),
            "Nomad job",
            &[
                "-address",
                "--address",
                "-region",
                "--region",
                "-namespace",
                "--namespace",
            ],
        ),
        "nomad.node" => RemoteSpec::new(
            "nomad",
            &["node", "status", "-json"],
            OutputParser::JsonField("ID"),
            "Nomad node",
            &["-address", "--address", "-region", "--region"],
        ),
        "nomad.allocation" => RemoteSpec::new(
            "nomad",
            &["alloc", "status", "-json"],
            OutputParser::JsonField("ID"),
            "Nomad allocation",
            &[
                "-address",
                "--address",
                "-region",
                "--region",
                "-namespace",
                "--namespace",
            ],
        ),
        "nomad.volume" => RemoteSpec::new(
            "nomad",
            &["volume", "status", "-json"],
            OutputParser::JsonField("ID"),
            "Nomad volume",
            &[
                "-address",
                "--address",
                "-region",
                "--region",
                "-namespace",
                "--namespace",
            ],
        ),
        "rclone.remote" => RemoteSpec::new(
            "rclone",
            &["listremotes"],
            OutputParser::Lines,
            "rclone remote",
            &["--config"],
        ),
        "restic.snapshot" => RemoteSpec::new(
            "restic",
            &["snapshots", "--json"],
            OutputParser::ResticSnapshots,
            "restic snapshot",
            &["-r", "--repo", "--repository-file"],
        ),
        "flatpak.application" => RemoteSpec::new(
            "flatpak",
            &["list", "--app", "--columns=application"],
            OutputParser::Lines,
            "Flatpak application",
            &["--installation"],
        ),
        "snap.package" => RemoteSpec::new(
            "snap",
            &["list"],
            OutputParser::FirstColumn,
            "installed snap",
            &[],
        ),
        _ => return None,
    };
    Some(spec)
}

impl RemoteSpec {
    const fn new(
        executable: &'static str,
        args: &'static [&'static str],
        parser: OutputParser,
        description: &'static str,
        context_options: &'static [&'static str],
    ) -> Self {
        Self {
            executable,
            args,
            parser,
            description,
            context_options,
        }
    }
}

fn selected_context_args(parsed: &ParsedCommandLine, accepted: &[&str]) -> (Vec<String>, String) {
    let words = completion_words(parsed);
    let mut args = Vec::new();
    let mut key_parts: Vec<String> = Vec::new();
    for accepted_name in accepted {
        let mut selected = None;
        for (index, word) in words.iter().enumerate() {
            if *word == *accepted_name {
                selected = words.get(index + 1).copied();
            } else if let Some(value) = word
                .strip_prefix(accepted_name)
                .and_then(|rest| rest.strip_prefix('='))
            {
                selected = Some(value);
            }
        }
        let Some(value) = selected else {
            continue;
        };
        if value.is_empty() || value == parsed.current_token || value.starts_with('-') {
            continue;
        }
        if key_parts
            .iter()
            .any(|part| part.starts_with(&format!("{}=", accepted_name.trim_start_matches('-'))))
        {
            continue;
        }
        if *accepted_name == "-chdir" {
            args.push(format!("-chdir={value}"));
        } else {
            args.push((*accepted_name).to_string());
            args.push(value.to_string());
        }
        key_parts.push(format!("{}={value}", accepted_name.trim_start_matches('-')));
    }
    key_parts.sort();
    key_parts.dedup();
    (args, key_parts.join(","))
}

fn run_and_parse(
    executable: &str,
    args: &[String],
    current_dir: &Path,
    parser: OutputParser,
) -> Result<Vec<String>> {
    let mut command = runner::command(executable);
    command.args(args).current_dir(current_dir);
    let output = runner::collect_stdout_with_timeout(command, runner::REMOTE_COMMAND_TIMEOUT)?;
    Ok(parse_output(&output, parser))
}

fn parse_output(output: &str, parser: OutputParser) -> Vec<String> {
    let values = match parser {
        OutputParser::Lines => output
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect(),
        OutputParser::Whitespace => output.split_whitespace().map(str::to_string).collect(),
        OutputParser::FirstColumn => output.lines().filter_map(first_data_column).collect(),
        OutputParser::JsonField(field) => serde_json::from_str::<Value>(output)
            .map(|value| {
                let mut values = Vec::new();
                collect_json_field(&value, field, &mut values);
                values
            })
            .unwrap_or_default(),
        OutputParser::JsonObjectKeys => serde_json::from_str::<Value>(output)
            .ok()
            .and_then(|value| value.as_object().cloned())
            .map(|object| object.into_iter().map(|(key, _)| key).collect())
            .unwrap_or_default(),
        OutputParser::JsonArrayValues => serde_json::from_str::<Value>(output)
            .ok()
            .and_then(|value| value.as_array().cloned())
            .map(|values| values.into_iter().filter_map(json_scalar).collect())
            .unwrap_or_default(),
        OutputParser::ResticSnapshots => serde_json::from_str::<Value>(output)
            .map(|value| {
                let mut ids = Vec::new();
                collect_json_field(&value, "id", &mut ids);
                ids.into_iter()
                    .map(|id| id.chars().take(8).collect::<String>())
                    .chain(std::iter::once("latest".to_string()))
                    .collect()
            })
            .unwrap_or_default(),
    };
    dedup_sorted(values)
}

fn first_data_column(line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('-') {
        return None;
    }
    let value = line
        .split_whitespace()
        .find(|part| *part != "*" && *part != "CURRENT")?;
    if matches!(value.to_ascii_lowercase().as_str(), "name" | "context") {
        return None;
    }
    Some(value.to_string())
}

fn collect_json_field(value: &Value, field: &str, values: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            if let Some(value) = object.get(field).and_then(json_scalar_ref) {
                values.push(value);
            }
            for child in object.values() {
                collect_json_field(child, field, values);
            }
        }
        Value::Array(array) => {
            for child in array {
                collect_json_field(child, field, values);
            }
        }
        _ => {}
    }
}

fn json_scalar(value: Value) -> Option<String> {
    json_scalar_ref(&value)
}

fn json_scalar_ref(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion::parser::CommandLineParser;

    #[test]
    fn parses_structured_and_tabular_outputs() {
        assert_eq!(
            parse_output(r#"[{"iid":12},{"iid":7}]"#, OutputParser::JsonField("iid")),
            vec!["12", "7"]
        );
        assert_eq!(
            parse_output("Name Version\nfoo 1\nbar 2\n", OutputParser::FirstColumn),
            vec!["bar", "foo"]
        );
        assert_eq!(
            parse_output(
                r#"[{"id":"1234567890abcdef"}]"#,
                OutputParser::ResticSnapshots
            ),
            vec!["12345678", "latest"]
        );
    }

    #[test]
    fn forwards_non_current_context_values() {
        let parser = CommandLineParser::new();
        let input = "gh pr view --repo owner/repo 12";
        let parsed = parser.parse(input, input.len());
        let (args, key) = selected_context_args(&parsed, &["-R", "--repo"]);
        assert_eq!(args, vec!["--repo", "owner/repo"]);
        assert_eq!(key, "repo=owner/repo");
    }

    #[test]
    fn terraform_chdir_precedes_state_subcommand() {
        let parser = CommandLineParser::new();
        let input = "terraform -chdir=env plan -target ";
        let parsed = parser.parse(input, input.len());
        let spec = remote_spec("terraform.resource").unwrap();
        let (context_args, _) = selected_context_args(&parsed, spec.context_options);
        let args = context_args
            .into_iter()
            .chain(spec.args.iter().map(|arg| (*arg).to_string()))
            .collect::<Vec<_>>();

        assert_eq!(args, ["-chdir=env", "state", "list"]);
    }
}
