use super::completion_words;
use crate::completion::parser::ParsedCommandLine;

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
    let words = completion_words(parsed_command_line);
    for (index, token) in words.iter().enumerate() {
        if *token == "-n" || *token == "--namespace" {
            let Some(value) = words.get(index + 1).copied() else {
                continue;
            };
            if !value.is_empty() && !value.starts_with('-') {
                return Some(value);
            }
        }
        if let Some(value) = token
            .strip_prefix("--namespace=")
            .or_else(|| token.strip_prefix("-n="))
            .or_else(|| token.strip_prefix("-n").filter(|value| !value.is_empty()))
            && !value.is_empty()
        {
            return Some(value);
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
