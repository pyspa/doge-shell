//! `doctor setup` / `doctor fix`: first-run directory and config-file state.
use crate::ShellProxy;
use crate::project_context;
use crate::task;
use dsh_types::Context;
use std::fs;
use std::path::Path;

use super::*;
pub(super) fn check_setup(
    ctx: &Context,
    proxy: &mut dyn ShellProxy,
    current_dir: &Path,
    fix: bool,
) {
    // `doctor fix` creates files here, so this must be the directory the shell
    // actually loads from - `dirs::config_dir()` on macOS is not.
    let config_root = crate::config_paths::config_home();

    ensure_setup_dir(ctx, &config_root, "config-root", fix);
    ensure_setup_dir(
        ctx,
        &crate::config_paths::skills_dir(),
        "runtime-skills",
        fix,
    );
    ensure_setup_dir(ctx, &config_root.join("completions"), "completion-dir", fix);
    ensure_setup_dir(
        ctx,
        &config_root.join("output-schemas"),
        "output-schema-dir",
        fix,
    );
    ensure_config_file(ctx, &crate::config_paths::config_file("config.lisp"), fix);

    let config = crate::chatgpt::load_openai_config(proxy);
    if let Some(api_key) = config.api_key() {
        let _ = ctx.write_stdout(&format!(
            "ok ai-key {}",
            mask_secret(Some(api_key.to_string()))
        ));
    } else {
        let _ = ctx.write_stdout(&format!(
            "warn ai-key missing {}",
            dsh_openai::API_KEY_SETUP_HINT
        ));
    }

    let mcp_count = proxy.list_mcp_servers().len();
    if mcp_count == 0 {
        let _ = ctx.write_stdout("skip mcp no configured servers");
    } else {
        let _ = ctx.write_stdout(&format!("ok mcp configured={mcp_count}"));
    }

    let project = project_context::resolve_project_context(current_dir);
    let _ = ctx.write_stdout(&format!(
        "ok project-root {}",
        project.project_root.display()
    ));
    if project.project_markers.is_empty() {
        let _ = ctx.write_stdout("warn project-markers none");
    } else {
        let _ = ctx.write_stdout(&format!(
            "ok project-markers {}",
            project.project_markers.join(", ")
        ));
    }

    if project.activations.is_empty() {
        let _ = ctx.write_stdout("skip activation no .env, .envrc, .venv, or venv found");
    } else {
        for activation in &project.activations {
            let _ = ctx.write_stdout(&format!(
                "ok activation {} {}",
                activation.kind,
                activation.path.display()
            ));
        }
        if project.project_root.join(".envrc").exists()
            && !proxy.is_direnv_allowed(&project.project_root)
        {
            let _ = ctx.write_stdout("warn envrc not allow-listed; use (allow-direnv \"<project-root>\") in config.lisp if trusted");
        }
        let _ = ctx.write_stdout("hint run `pm activate` to apply safe project activation");
    }

    match task::summarize_tasks_in_dir_metadata_only(&project.project_root) {
        Ok(summary) if summary.tasks.is_empty() && summary.deferred_sources.is_empty() => {
            let _ = ctx.write_stdout("skip tasks none detected");
        }
        Ok(summary) => {
            if !summary.tasks.is_empty() {
                let _ = ctx.write_stdout(&format!(
                    "ok tasks metadata-detected={}",
                    summary.tasks.len()
                ));
            }
            if !summary.deferred_sources.is_empty() {
                let _ = ctx.write_stdout(&format!(
                    "skip tasks dynamic-probe sources={} run `task --list` for full detection",
                    summary.deferred_sources.join(", ")
                ));
            }
            let _ = ctx.write_stdout("hint run `task` to select a project task");
        }
        Err(err) => {
            let _ = ctx.write_stdout(&format!("warn tasks unavailable {err}"));
        }
    }

    let _ = ctx.write_stdout(
        "hint run `help ai`, `help project`, or `help --search <keyword>` to discover commands",
    );
}

pub(super) fn ensure_setup_dir(ctx: &Context, path: &Path, label: &str, fix: bool) {
    if path.is_dir() {
        let _ = ctx.write_stdout(&format!("ok {label} {}", path.display()));
        return;
    }

    if fix {
        match fs::create_dir_all(path) {
            Ok(()) => {
                let _ = ctx.write_stdout(&format!("fixed {label} created {}", path.display()));
            }
            Err(err) => {
                let _ = ctx.write_stdout(&format!("warn {label} create-failed {err}"));
            }
        }
    } else {
        let _ = ctx.write_stdout(&format!("warn {label} missing {}", path.display()));
    }
}

pub(super) fn ensure_config_file(ctx: &Context, path: &Path, fix: bool) {
    if path.is_file() {
        let _ = ctx.write_stdout(&format!("ok config {}", path.display()));
        return;
    }

    if !fix {
        let _ = ctx.write_stdout(&format!("warn config missing {}", path.display()));
        return;
    }

    if let Some(parent) = path.parent()
        && let Err(err) = fs::create_dir_all(parent)
    {
        let _ = ctx.write_stdout(&format!("warn config parent-create-failed {err}"));
        return;
    }

    match fs::write(path, default_config_lisp()) {
        Ok(()) => {
            let _ = ctx.write_stdout(&format!("fixed config created {}", path.display()));
        }
        Err(err) => {
            let _ = ctx.write_stdout(&format!("warn config create-failed {err}"));
        }
    }
}

pub(super) fn default_config_lisp() -> &'static str {
    concat!(
        ";; doge-shell config.lisp\n",
        ";; This file was created by `doctor fix`.\n",
        "\n",
        ";; Common aliases\n",
        "(alias \"ll\" \"ls -alF\")\n",
        "(alias \"la\" \"ls -A\")\n",
        "\n",
        ";; AI execute-tool allowlist for low-risk read-only commands.\n",
        "(chat-execute-clear)\n",
        "(chat-execute-add \"ls\" \"cat\" \"echo\" \"grep\" \"find\")\n",
        "\n",
        ";; Uncomment after reviewing a trusted project root with .envrc.\n",
        ";; (allow-direnv \"/path/to/project\")\n",
    )
}
