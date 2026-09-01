//! Where the agent is working, and what this project can be asked to do.
//!
//! The environment snapshot on every request can only afford a few lines, so
//! the things that matter occasionally but cost real tokens - the project root,
//! its runtimes, the build and test commands it actually defines, the user's
//! aliases - live here and are fetched when they are needed. Guessing the test
//! command is one of the more expensive mistakes an agent makes, and this
//! turns it into a lookup.

use crate::ShellProxy;
use crate::{project_context, task};
use serde_json::{Value, json};

pub(crate) const NAME: &str = "shell_context";

/// Tasks listed before the rest are summarised as a count.
const MAX_TASKS: usize = 25;
const MAX_ALIASES: usize = 40;

pub(crate) fn definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": NAME,
            "description": "Describe the project the shell is in: its root directory, the markers that identify it, the language runtimes in use, the build/test/lint tasks it defines, and the user's shell aliases. Call this before guessing how to build or test the project.",
            "parameters": {
                "type": "object",
                "properties": {},
                "required": [],
                "additionalProperties": false
            }
        }
    })
}

pub(crate) fn run(_arguments: &str, proxy: &mut dyn ShellProxy) -> Result<String, String> {
    let current_dir = proxy
        .get_current_dir()
        .map_err(|err| format!("chat: failed to resolve current directory: {err}"))?;

    let project = project_context::resolve_project_context(&current_dir);

    let mut out = String::new();
    out.push_str(&format!("Current directory: {}\n", project.cwd.display()));
    out.push_str(&format!(
        "Project root: {}\n",
        project.project_root.display()
    ));

    if !project.project_markers.is_empty() {
        out.push_str(&format!(
            "Project markers: {}\n",
            project.project_markers.join(", ")
        ));
    }

    if !project.runtimes.is_empty() {
        out.push_str("Runtimes:\n");
        for runtime in &project.runtimes {
            let version = runtime.version.as_deref().unwrap_or("version unknown");
            out.push_str(&format!(
                "- {} {} (from {})\n",
                runtime.name, version, runtime.source
            ));
        }
    }

    out.push_str(&render_tasks(&current_dir));
    out.push_str(&render_aliases(proxy));
    Ok(out)
}

fn render_tasks(current_dir: &std::path::Path) -> String {
    match task::list_tasks_in_dir(current_dir) {
        Ok(tasks) if tasks.is_empty() => {
            "\nTasks: none detected (no Makefile, package.json scripts, justfile, ...)\n"
                .to_string()
        }
        Ok(tasks) => {
            let total = tasks.len();
            let mut out = String::from("\nTasks defined by this project:\n");
            for task in tasks.iter().take(MAX_TASKS) {
                let description = task
                    .description
                    .as_deref()
                    .map(|text| format!(" - {text}"))
                    .unwrap_or_default();
                out.push_str(&format!(
                    "- {} [{}]: `{}`{description}\n",
                    task.name, task.source, task.command
                ));
            }
            if total > MAX_TASKS {
                out.push_str(&format!("- (+{} more)\n", total - MAX_TASKS));
            }
            out
        }
        // A task file this build cannot parse is worth saying out loud: the
        // alternative is an agent concluding the project has no test command.
        Err(err) => format!("\nTasks: could not be detected ({err})\n"),
    }
}

fn render_aliases(proxy: &mut dyn ShellProxy) -> String {
    let mut aliases: Vec<(String, String)> = proxy.list_aliases().into_iter().collect();
    if aliases.is_empty() {
        return "\nAliases: none\n".to_string();
    }

    aliases.sort_by(|a, b| a.0.cmp(&b.0));
    let total = aliases.len();

    let mut out = String::from("\nAliases:\n");
    for (name, command) in aliases.iter().take(MAX_ALIASES) {
        out.push_str(&format!("- {name} = {command}\n"));
    }
    if total > MAX_ALIASES {
        out.push_str(&format!("- (+{} more)\n", total - MAX_ALIASES));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestShellProxy;
    use tempfile::tempdir;

    #[test]
    fn reports_the_project_root_above_the_current_directory() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        let nested = root.join("src/deep");
        std::fs::create_dir_all(&nested).unwrap();

        let mut proxy = TestShellProxy {
            current_dir: nested.clone(),
            ..TestShellProxy::default()
        };

        let rendered = run("{}", &mut proxy).unwrap();

        assert!(rendered.contains("Project root:"), "{rendered}");
        assert!(rendered.contains("Cargo.toml"), "{rendered}");
    }

    /// "No aliases" has to be said, or the agent assumes the list was omitted.
    #[test]
    fn an_empty_alias_table_is_stated_rather_than_left_out() {
        let dir = tempdir().unwrap();
        let mut proxy = TestShellProxy {
            current_dir: dir.path().to_path_buf(),
            ..TestShellProxy::default()
        };

        let rendered = run("{}", &mut proxy).unwrap();

        assert!(rendered.contains("Aliases: none"), "{rendered}");
    }

    #[test]
    fn aliases_are_listed_in_a_stable_order() {
        let dir = tempdir().unwrap();
        let mut proxy = TestShellProxy {
            current_dir: dir.path().to_path_buf(),
            ..TestShellProxy::default()
        };
        proxy
            .aliases
            .insert("gs".to_string(), "git status".to_string());
        proxy
            .aliases
            .insert("ga".to_string(), "git add".to_string());

        let rendered = run("{}", &mut proxy).unwrap();
        let ga = rendered.find("- ga = ").unwrap();
        let gs = rendered.find("- gs = ").unwrap();

        assert!(ga < gs, "{rendered}");
    }
}
