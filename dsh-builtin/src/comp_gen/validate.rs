//! Validating a `completions/*.json` file against the schema the runtime loader expects: required/optional fields on commands, options, subcommands, and arguments (including the recursive `argument_type`
//! shape), rejecting anything the loader would silently misread.
use super::*;
use serde_json::Map;

pub(crate) fn validate_completion_json(json: &str, expected_command: &str) -> Result<()> {
    let value: Value = serde_json::from_str(json).context("AI returned invalid JSON")?;
    let obj = value
        .as_object()
        .context("Completion JSON must be an object")?;

    let command_value = obj
        .get("command")
        .context("Missing required field: command")?;
    let command = require_non_empty_string(command_value, "command")?;
    if command != expected_command {
        bail!(
            "Command mismatch: expected '{}', got '{}'",
            expected_command,
            command
        );
    }

    if let Some(options) = obj.get("global_options") {
        validate_options_array(options, "global_options")?;
    }
    if let Some(options) = obj.get("options") {
        validate_options_array(options, "options")?;
    }
    if let Some(arguments) = obj.get("arguments") {
        validate_arguments_array(arguments, "arguments")?;
    }
    if let Some(subcommands) = obj.get("subcommands") {
        validate_subcommands_array(subcommands, "subcommands")?;
    }

    Ok(())
}
fn require_non_empty_string<'a>(value: &'a Value, path: &str) -> Result<&'a str> {
    let s = value
        .as_str()
        .with_context(|| format!("{path} must be a string"))?;
    if s.trim().is_empty() {
        bail!("{path} must be a non-empty string");
    }
    Ok(s)
}
fn optional_string<'a>(
    obj: &'a Map<String, Value>,
    key: &str,
    path: &str,
) -> Result<Option<&'a str>> {
    let Some(value) = obj.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let s = value
        .as_str()
        .with_context(|| format!("{path}.{key} must be a string"))?;
    if s.trim().is_empty() {
        bail!("{path}.{key} must be a non-empty string");
    }
    Ok(Some(s))
}
fn validate_options_array(value: &Value, path: &str) -> Result<()> {
    let options = value
        .as_array()
        .with_context(|| format!("{path} must be an array"))?;
    for (idx, option) in options.iter().enumerate() {
        let option_path = format!("{path}[{idx}]");
        let obj = option
            .as_object()
            .with_context(|| format!("{option_path} must be an object"))?;
        let short = optional_string(obj, "short", &option_path)?;
        let long = optional_string(obj, "long", &option_path)?;
        if short.is_none() && long.is_none() {
            bail!("{option_path} must have at least one of 'short' or 'long'");
        }
        if let Some(short) = short
            && !valid_short_option(short)
        {
            bail!("{option_path}.short has invalid option format '{short}'");
        }
        if let Some(long) = long
            && !valid_long_option(long)
        {
            bail!("{option_path}.long has invalid option format '{long}'");
        }
        if let Some(takes_value) = obj.get("takes_value")
            && !takes_value.is_boolean()
        {
            bail!("{option_path}.takes_value must be boolean");
        }
        if let Some(value_type) = obj.get("value_type") {
            validate_argument_type(value_type, &format!("{option_path}.value_type"))?;
        }
        if let Some(argument) = obj.get("argument") {
            validate_argument_object(argument, &format!("{option_path}.argument"))?;
        }
    }
    Ok(())
}
fn option_base(option: &str) -> &str {
    option.split_whitespace().next().unwrap_or("")
}
fn valid_short_option(option: &str) -> bool {
    let base = option_base(option);
    base.starts_with('-') && !base.starts_with("--") && base.len() > 1
}
fn valid_long_option(option: &str) -> bool {
    let base = option_base(option);
    (base.starts_with('-') || base.starts_with('+')) && base.len() > 1 && base != "--"
}
fn validate_arguments_array(value: &Value, path: &str) -> Result<()> {
    let args = value
        .as_array()
        .with_context(|| format!("{path} must be an array"))?;
    for (idx, arg) in args.iter().enumerate() {
        let arg_path = format!("{path}[{idx}]");
        validate_argument_object(arg, &arg_path)?;
    }
    Ok(())
}
fn validate_argument_object(value: &Value, path: &str) -> Result<()> {
    let obj = value
        .as_object()
        .with_context(|| format!("{path} must be an object"))?;
    let name_value = obj
        .get("name")
        .with_context(|| format!("{path}.name is required"))?;
    require_non_empty_string(name_value, &format!("{path}.name"))?;
    if let Some(arg_type) = obj.get("type") {
        validate_argument_type(arg_type, &format!("{path}.type"))?;
    }
    if let Some(required) = obj.get("required")
        && !required.is_boolean()
    {
        bail!("{path}.required must be boolean");
    }
    if let Some(multiple) = obj.get("multiple")
        && !multiple.is_boolean()
    {
        bail!("{path}.multiple must be boolean");
    }
    Ok(())
}
fn validate_subcommands_array(value: &Value, path: &str) -> Result<()> {
    let subs = value
        .as_array()
        .with_context(|| format!("{path} must be an array"))?;
    for (idx, sub) in subs.iter().enumerate() {
        let sub_path = format!("{path}[{idx}]");
        let obj = sub
            .as_object()
            .with_context(|| format!("{sub_path} must be an object"))?;
        let name_value = obj
            .get("name")
            .with_context(|| format!("{sub_path}.name is required"))?;
        require_non_empty_string(name_value, &format!("{sub_path}.name"))?;
        if let Some(options) = obj.get("options") {
            validate_options_array(options, &format!("{sub_path}.options"))?;
        }
        if let Some(arguments) = obj.get("arguments") {
            validate_arguments_array(arguments, &format!("{sub_path}.arguments"))?;
        }
        if let Some(children) = obj.get("subcommands") {
            validate_subcommands_array(children, &format!("{sub_path}.subcommands"))?;
        }
    }
    Ok(())
}
fn validate_argument_type(value: &Value, path: &str) -> Result<()> {
    let obj = value
        .as_object()
        .with_context(|| format!("{path} must be an object"))?;
    let type_value = obj
        .get("type")
        .with_context(|| format!("{path}.type is required"))?;
    let type_name = require_non_empty_string(type_value, &format!("{path}.type"))?;
    if type_name == "Script" {
        bail!("{path}.type 'Script' is not allowed");
    }

    if type_name == "Choice" {
        let data = obj
            .get("data")
            .with_context(|| format!("{path}.data is required for Choice"))?;
        let items = data
            .as_array()
            .with_context(|| format!("{path}.data must be an array of strings"))?;
        for (idx, item) in items.iter().enumerate() {
            if item.as_str().is_none() {
                bail!("{path}.data[{idx}] must be a string");
            }
        }
    }

    if type_name == "File"
        && let Some(data) = obj.get("data")
    {
        let data_obj = data
            .as_object()
            .with_context(|| format!("{path}.data must be an object"))?;
        if let Some(exts) = data_obj.get("extensions") {
            let list = exts
                .as_array()
                .with_context(|| format!("{path}.data.extensions must be an array"))?;
            for (idx, ext) in list.iter().enumerate() {
                if ext.as_str().is_none() {
                    bail!("{path}.data.extensions[{idx}] must be a string");
                }
            }
        }
    }

    if type_name == "Dynamic" {
        let data = obj
            .get("data")
            .with_context(|| format!("{path}.data is required for Dynamic"))?;
        let data_obj = data
            .as_object()
            .with_context(|| format!("{path}.data must be an object"))?;
        let provider = data_obj
            .get("provider")
            .with_context(|| format!("{path}.data.provider is required"))?;
        let provider = require_non_empty_string(provider, &format!("{path}.data.provider"))?;
        if !is_known_dynamic_completion_provider(provider) {
            bail!("{path}.data.provider has unknown Dynamic provider '{provider}'");
        }
        if let Some(scope) = data_obj.get("scope")
            && !scope.is_null()
            && scope.as_str().is_none()
        {
            bail!("{path}.data.scope must be a string or null");
        }
    }

    Ok(())
}
