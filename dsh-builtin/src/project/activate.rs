//! `pm activate`: parses `.env`/`.envrc` and applies them to the shell environment (native), or overlays `mise env --json` (mise), with a `--dry-run` mode that only reports what would change.
use super::*;

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct EnvrcActivation {
    pub(super) vars: Vec<(String, String)>,
    pub(super) path_adds: Vec<String>,
}

pub(super) fn parse_dotenv_file(path: &Path) -> Result<Vec<(String, String)>> {
    let contents = fs::read_to_string(path)?;
    Ok(contents.lines().filter_map(parse_assignment_line).collect())
}

pub(super) fn parse_envrc_file(path: &Path) -> Result<EnvrcActivation> {
    let contents = fs::read_to_string(path)?;
    let mut activation = EnvrcActivation::default();

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let Some((command, rest)) = split_command_line(trimmed) else {
            continue;
        };

        match command.to_ascii_lowercase().as_str() {
            "export" => {
                if let Some(var) = parse_assignment(rest) {
                    activation.vars.push(var);
                }
            }
            "path_add" => {
                let path = unquote(rest.trim());
                if !path.is_empty() {
                    activation.path_adds.push(path);
                }
            }
            _ => {}
        }
    }

    Ok(activation)
}

pub(super) fn parse_assignment_line(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }

    let assignment = trimmed.strip_prefix("export ").unwrap_or(trimmed).trim();
    parse_assignment(assignment)
}

pub(super) fn parse_assignment(assignment: &str) -> Option<(String, String)> {
    let (key, value) = assignment.split_once('=')?;
    let key = key.trim();
    if !is_valid_env_key(key) {
        return None;
    }
    Some((key.to_string(), unquote(value.trim())))
}

pub(super) fn split_command_line(line: &str) -> Option<(&str, &str)> {
    let mut parts = line.splitn(2, char::is_whitespace);
    let command = parts.next()?.trim();
    let rest = parts.next().unwrap_or("").trim();
    Some((command, rest))
}

pub(super) fn is_valid_env_key(key: &str) -> bool {
    let mut chars = key.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

pub(super) fn unquote(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        value[1..value.len() - 1].to_string()
    } else {
        value.to_string()
    }
}

pub(super) fn find_project_venv(root: &Path) -> Option<PathBuf> {
    [".venv", "venv"]
        .into_iter()
        .map(|name| root.join(name))
        .find(|path| path.is_dir())
}

pub(super) fn normalize_activation_path(root: &Path, path: &str) -> PathBuf {
    let expanded = shellexpand::tilde(path).into_owned();
    let path = PathBuf::from(expanded);
    if path.is_absolute() {
        path
    } else {
        root.join(path)
    }
}

pub(super) fn display_activation_path(root: &Path, path: &str) -> String {
    normalize_activation_path(root, path).display().to_string()
}

pub(super) fn prepend_path(proxy: &mut dyn ShellProxy, root: &Path, path: &str) -> bool {
    let path = normalize_activation_path(root, path);
    let path = path.to_string_lossy().into_owned();
    let current_path = proxy
        .get_var("PATH")
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default();

    if current_path.split(':').any(|entry| entry == path) {
        return false;
    }

    let updated = if current_path.is_empty() {
        path
    } else {
        format!("{path}:{current_path}")
    };
    proxy.set_env_var("PATH".to_string(), updated);
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ActivationProvider {
    Auto,
    Native,
    Mise,
}

pub(super) fn activate(ctx: &Context, args: &[String], proxy: &mut dyn ShellProxy) -> Result<()> {
    let mut provider = ActivationProvider::Auto;
    let mut dry_run = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--dry-run" => dry_run = true,
            "--provider" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    return Err(anyhow::anyhow!("--provider requires auto, native, or mise"));
                };
                provider = parse_activation_provider(value)?;
            }
            value if value.starts_with("--provider=") => {
                provider = parse_activation_provider(value.trim_start_matches("--provider="))?;
            }
            value => return Err(anyhow::anyhow!("unknown activate option: {value}")),
        }
        index += 1;
    }

    let current_dir = proxy.get_current_dir()?;
    let project = project_context::resolve_project_context(&current_dir);
    let root = project.project_root;

    if matches!(
        provider,
        ActivationProvider::Auto | ActivationProvider::Native
    ) {
        activate_native(ctx, proxy, dry_run)?;
    }
    if matches!(
        provider,
        ActivationProvider::Auto | ActivationProvider::Mise
    ) {
        let status = MiseStatus::detect(&root);
        if matches!(status.trust.as_str(), "trusted" | "safe") {
            activate_mise(ctx, proxy, &root, &status, dry_run)?;
        } else if provider == ActivationProvider::Mise {
            return Err(anyhow::anyhow!(
                "mise provider is {}. dsh will not trust or install it automatically",
                status.trust
            ));
        } else if status.trust != "not-configured" {
            let _ = ctx.write_stdout(&format!(
                "mise overlay skipped trust={} (dsh never runs `mise trust` automatically)",
                status.trust
            ));
        }
    }
    Ok(())
}

pub(super) fn parse_activation_provider(value: &str) -> Result<ActivationProvider> {
    match value {
        "auto" => Ok(ActivationProvider::Auto),
        "native" => Ok(ActivationProvider::Native),
        "mise" => Ok(ActivationProvider::Mise),
        _ => Err(anyhow::anyhow!(
            "unknown provider `{value}`; expected auto, native, or mise"
        )),
    }
}

pub(super) fn activate_mise(
    ctx: &Context,
    proxy: &mut dyn ShellProxy,
    root: &Path,
    status: &MiseStatus,
    dry_run: bool,
) -> Result<()> {
    let mise = status
        .executable
        .as_deref()
        .context("mise executable unavailable")?;
    let output = mise_output(mise, root, &["--no-hooks", "env", "--json"])?;
    if !output.status.success() {
        return Err(anyhow::anyhow!(
            "mise env failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let value: JsonValue = serde_json::from_slice(&output.stdout)?;
    let object = value
        .as_object()
        .context("mise env --json returned a non-object value")?;
    let mut changed = 0;
    for (key, value) in object {
        let Some(value) = value.as_str() else {
            continue;
        };
        if proxy.get_var(key).as_deref() == Some(value) {
            continue;
        }
        changed += 1;
        if dry_run {
            let _ = ctx.write_stdout(&format!(
                "mise set {}={}",
                key,
                safety_policy::mask_env_value(key, value)
            ));
        } else {
            proxy.set_env_var(key.clone(), value.to_string());
        }
    }
    let mode = if dry_run { "dry-run" } else { "applied" };
    let _ = ctx.write_stdout(&format!(
        "mise overlay {mode} vars={changed} hooks=disabled trust={}",
        status.trust
    ));
    Ok(())
}

pub(super) fn activate_native(
    ctx: &Context,
    proxy: &mut dyn ShellProxy,
    dry_run: bool,
) -> Result<()> {
    if dry_run {
        return activate_dry_run(ctx, proxy);
    }

    let current_dir = proxy.get_current_dir()?;
    let project = project_context::resolve_project_context(&current_dir);
    let root = project.project_root;
    let mut applied = Vec::new();

    let dotenv = root.join(".env");
    if dotenv.exists() {
        let vars = parse_dotenv_file(&dotenv)?;
        for (key, value) in &vars {
            if env_assignment_requires_confirmation(key, value)
                && !proxy.confirm_action(&format!(
                    "Apply sensitive or high-risk environment variable `{key}` from .env? \r\nProceed?"
                ))?
            {
                applied.push(format!(".env skipped {key}"));
                continue;
            }
            proxy.set_env_var(key.clone(), value.clone());
        }
        if !vars.is_empty() {
            applied.push(format!(".env vars={}", vars.len()));
        }
    }

    let envrc = root.join(".envrc");
    if envrc.exists() {
        if proxy.is_direnv_allowed(&root) {
            let plan = parse_envrc_file(&envrc)?;
            for (key, value) in &plan.vars {
                if env_assignment_requires_confirmation(key, value)
                    && !proxy.confirm_action(&format!(
                        "Apply sensitive or high-risk environment variable `{key}` from .envrc? \r\nProceed?"
                    ))?
                {
                    applied.push(format!(".envrc skipped {key}"));
                    continue;
                }
                proxy.set_env_var(key.clone(), value.clone());
            }
            for path in &plan.path_adds {
                if activation_path_outside_root(&root, path)
                    && !proxy.confirm_action(&format!(
                        "Add PATH entry outside project root `{}` from .envrc? \r\nProceed?",
                        display_activation_path(&root, path)
                    ))?
                {
                    applied.push(format!(
                        "path_add skipped {}",
                        display_activation_path(&root, path)
                    ));
                    continue;
                }
                if prepend_path(proxy, &root, path) {
                    applied.push(format!("path_add {}", display_activation_path(&root, path)));
                }
            }
            if !plan.vars.is_empty() {
                applied.push(format!(".envrc vars={}", plan.vars.len()));
            }
        } else {
            let _ = ctx.write_stdout(&format!(
                "Skipped .envrc at {} (not allow-direnv root).",
                envrc.display()
            ));
        }
    }

    if let Some(venv) = find_project_venv(&root) {
        proxy.set_env_var(
            "VIRTUAL_ENV".to_string(),
            venv.to_string_lossy().into_owned(),
        );
        let bin = venv.join("bin");
        if bin.is_dir() && prepend_path(proxy, &root, bin.to_string_lossy().as_ref()) {
            applied.push(format!("venv {}", venv.display()));
        } else {
            applied.push(format!("VIRTUAL_ENV {}", venv.display()));
        }
    }

    if applied.is_empty() {
        let _ = ctx.write_stdout(&format!("No activation files found in {}.", root.display()));
    } else {
        let _ = ctx.write_stdout(&format!(
            "Activated project environment for {}: {}",
            root.display(),
            applied.join(", ")
        ));
    }

    Ok(())
}

pub(super) fn activate_dry_run(ctx: &Context, proxy: &mut dyn ShellProxy) -> Result<()> {
    let current_dir = proxy.get_current_dir()?;
    let project = project_context::resolve_project_context(&current_dir);
    let root = project.project_root;

    let _ = ctx.write_stdout(&format!("activation dry-run root {}", root.display()));

    let dotenv = root.join(".env");
    if dotenv.exists() {
        let vars = parse_dotenv_file(&dotenv)?;
        if vars.is_empty() {
            let _ = ctx.write_stdout(".env vars=0");
        } else {
            for (key, value) in vars {
                let marker = if env_assignment_requires_confirmation(&key, &value) {
                    " confirm"
                } else {
                    ""
                };
                let _ = ctx.write_stdout(&format!(
                    ".env set {}={}{}",
                    key,
                    safety_policy::mask_env_value(&key, &value),
                    marker
                ));
            }
        }
    } else {
        let _ = ctx.write_stdout(".env missing");
    }

    let envrc = root.join(".envrc");
    if envrc.exists() {
        if proxy.is_direnv_allowed(&root) {
            let plan = parse_envrc_file(&envrc)?;
            for (key, value) in plan.vars {
                let marker = if env_assignment_requires_confirmation(&key, &value) {
                    " confirm"
                } else {
                    ""
                };
                let _ = ctx.write_stdout(&format!(
                    ".envrc set {}={}{}",
                    key,
                    safety_policy::mask_env_value(&key, &value),
                    marker
                ));
            }
            for path in plan.path_adds {
                let marker = if activation_path_outside_root(&root, &path) {
                    " confirm-outside-root"
                } else {
                    ""
                };
                let _ = ctx.write_stdout(&format!(
                    ".envrc path_add {}{}",
                    display_activation_path(&root, &path),
                    marker
                ));
            }
        } else {
            let _ = ctx.write_stdout(&format!(".envrc skipped {} not-allowed", envrc.display()));
        }
    } else {
        let _ = ctx.write_stdout(".envrc missing");
    }

    if let Some(venv) = find_project_venv(&root) {
        let _ = ctx.write_stdout(&format!("venv {}", venv.display()));
        let bin = venv.join("bin");
        if bin.is_dir() {
            let _ = ctx.write_stdout(&format!("venv path_add {}", bin.display()));
        }
    } else {
        let _ = ctx.write_stdout("venv missing");
    }

    if let Ok(summary) = activation_safety_summary(&root, proxy) {
        let _ = ctx.write_stdout(&summary);
    }

    Ok(())
}

pub(super) fn activation_safety_summary(root: &Path, proxy: &dyn ShellProxy) -> Result<String> {
    let mut env_vars = 0usize;
    let mut confirm_vars = 0usize;
    let mut outside_paths = 0usize;

    let dotenv = root.join(".env");
    if dotenv.exists() {
        let vars = parse_dotenv_file(&dotenv)?;
        env_vars += vars.len();
        confirm_vars += vars
            .iter()
            .filter(|(key, value)| env_assignment_requires_confirmation(key, value))
            .count();
    }

    let envrc = root.join(".envrc");
    let envrc_state = if envrc.exists() {
        if proxy.is_direnv_allowed(root) {
            let plan = parse_envrc_file(&envrc)?;
            env_vars += plan.vars.len();
            confirm_vars += plan
                .vars
                .iter()
                .filter(|(key, value)| env_assignment_requires_confirmation(key, value))
                .count();
            outside_paths += plan
                .path_adds
                .iter()
                .filter(|path| activation_path_outside_root(root, path))
                .count();
            "allowed"
        } else {
            "not-allowed"
        }
    } else {
        "missing"
    };

    Ok(format!(
        "activation safety env_vars={env_vars} confirm_vars={confirm_vars} envrc={envrc_state} outside_path_adds={outside_paths}"
    ))
}

pub(super) fn env_assignment_requires_confirmation(key: &str, value: &str) -> bool {
    is_high_risk_env_key(key)
        || safety_policy::is_sensitive_key(key)
        || safety_policy::contains_sensitive_text(value)
}

pub(super) fn is_high_risk_env_key(key: &str) -> bool {
    matches!(
        key,
        "LD_PRELOAD"
            | "LD_LIBRARY_PATH"
            | "DYLD_INSERT_LIBRARIES"
            | "PYTHONPATH"
            | "PERL5LIB"
            | "RUBYLIB"
            | "NODE_OPTIONS"
    )
}

pub(super) fn activation_path_outside_root(root: &Path, path: &str) -> bool {
    let root = lexical_normalize(root);
    let normalized = lexical_normalize(&normalize_activation_path(&root, path));
    !normalized.starts_with(&root)
}

pub(super) fn lexical_normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}
