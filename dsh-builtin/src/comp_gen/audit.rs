//! Walking a directory of completion JSON definitions and reporting what is in
//! it: how many commands, how many string versus dynamic argument types, which
//! dynamic providers are used, which are not registered, and which definitions
//! are empty.
use super::*;

#[derive(Debug, Default)]
struct CompletionAudit {
    command_count: usize,
    string_count: usize,
    dynamic_count: usize,
    unknown_providers: BTreeMap<String, usize>,
    used_providers: BTreeMap<String, usize>,
    empty_definitions: Vec<String>,
}

pub(super) fn audit_completion_dir(dir: &Path) -> Result<String> {
    let mut audit = CompletionAudit::default();
    let mut entries = fs::read_dir(dir)
        .with_context(|| format!("Failed to read completion dir '{}'", dir.display()))?
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let json = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read '{}'", path.display()))?;
        let value: Value = serde_json::from_str(&json)
            .with_context(|| format!("Invalid JSON in '{}'", path.display()))?;
        audit.command_count += 1;
        if completion_definition_is_empty(&value) {
            audit.empty_definitions.push(
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("<unknown>")
                    .to_string(),
            );
        }
        audit_command_value(&value, &mut audit);
    }

    let unused_providers = DYNAMIC_COMPLETION_PROVIDERS
        .iter()
        .filter(|provider| !audit.used_providers.contains_key(**provider))
        .copied()
        .collect::<Vec<_>>();

    let mut lines = vec![
        format!("commands={}", audit.command_count),
        format!("string_types={}", audit.string_count),
        format!("dynamic_types={}", audit.dynamic_count),
        format!("unknown_providers={}", audit.unknown_providers.len()),
        format!("unused_providers={}", unused_providers.len()),
        format!("empty_definitions={}", audit.empty_definitions.len()),
    ];
    for (provider, count) in audit.unknown_providers {
        lines.push(format!("unknown_provider {provider} count={count}"));
    }
    for provider in unused_providers {
        lines.push(format!("unused_provider {provider}"));
    }
    for command in audit.empty_definitions {
        lines.push(format!("empty_definition {command}"));
    }
    Ok(lines.join("\n"))
}

fn completion_definition_is_empty(value: &Value) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    ["global_options", "options", "arguments", "subcommands"]
        .iter()
        .all(|key| {
            obj.get(*key)
                .and_then(Value::as_array)
                .is_none_or(|values| values.is_empty())
        })
}

fn audit_command_value(value: &Value, audit: &mut CompletionAudit) {
    let Some(obj) = value.as_object() else {
        return;
    };
    if let Some(options) = obj.get("global_options").and_then(Value::as_array) {
        for option in options {
            audit_option_value(option, audit);
        }
    }
    if let Some(options) = obj.get("options").and_then(Value::as_array) {
        for option in options {
            audit_option_value(option, audit);
        }
    }
    if let Some(arguments) = obj.get("arguments").and_then(Value::as_array) {
        for argument in arguments {
            audit_argument_value(argument, audit);
        }
    }
    if let Some(subcommands) = obj.get("subcommands").and_then(Value::as_array) {
        for subcommand in subcommands {
            audit_subcommand_value(subcommand, audit);
        }
    }
}

fn audit_subcommand_value(value: &Value, audit: &mut CompletionAudit) {
    let Some(obj) = value.as_object() else {
        return;
    };
    if let Some(options) = obj.get("options").and_then(Value::as_array) {
        for option in options {
            audit_option_value(option, audit);
        }
    }
    if let Some(arguments) = obj.get("arguments").and_then(Value::as_array) {
        for argument in arguments {
            audit_argument_value(argument, audit);
        }
    }
    if let Some(subcommands) = obj.get("subcommands").and_then(Value::as_array) {
        for subcommand in subcommands {
            audit_subcommand_value(subcommand, audit);
        }
    }
}

fn audit_option_value(value: &Value, audit: &mut CompletionAudit) {
    let Some(obj) = value.as_object() else {
        return;
    };
    if let Some(value_type) = obj.get("value_type") {
        audit_argument_type_value(value_type, audit);
    }
    if let Some(argument) = obj.get("argument") {
        audit_argument_value(argument, audit);
    }
}

fn audit_argument_value(value: &Value, audit: &mut CompletionAudit) {
    let Some(obj) = value.as_object() else {
        return;
    };
    if let Some(arg_type) = obj.get("type") {
        audit_argument_type_value(arg_type, audit);
    }
}

fn audit_argument_type_value(value: &Value, audit: &mut CompletionAudit) {
    let Some(type_name) = value.get("type").and_then(Value::as_str) else {
        return;
    };
    match type_name {
        "String" => audit.string_count += 1,
        "Dynamic" => {
            audit.dynamic_count += 1;
            let provider = value
                .get("data")
                .and_then(|data| data.get("provider"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if !is_known_dynamic_completion_provider(provider) {
                *audit
                    .unknown_providers
                    .entry(provider.to_string())
                    .or_insert(0) += 1;
            }
            *audit
                .used_providers
                .entry(provider.to_string())
                .or_insert(0) += 1;
        }
        _ => {}
    }
}
