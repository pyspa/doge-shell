//! Where the agent is working, and what this project can be asked to do.
//!
//! The environment snapshot on every request can only afford a few lines, so
//! the things that matter occasionally but cost real tokens - the project root,
//! its runtimes, the build and test commands it actually defines, the user's
//! aliases - live here and are fetched when they are needed. Guessing the test
//! command is one of the more expensive mistakes an agent makes, and this
//! turns it into a lookup.

use super::environment::filter_unscoped_ai_child_environment;
use crate::ShellProxy;
use crate::task::TaskDiscoveryRuntime;
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

    out.push_str(&render_tasks(&current_dir, &snapshot_runtime(proxy)));
    out.push_str(&render_aliases(proxy));
    Ok(out)
}

fn snapshot_runtime(proxy: &dyn ShellProxy) -> TaskDiscoveryRuntime {
    TaskDiscoveryRuntime::new(
        proxy.command_search_paths(),
        filter_unscoped_ai_child_environment(proxy.child_process_environment()),
    )
}

fn render_tasks(current_dir: &std::path::Path, runtime: &TaskDiscoveryRuntime) -> String {
    match task::list_tasks_in_dir(current_dir, runtime) {
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

    /// The agent sees the same catalog as the shell: `shell_context` tasks
    /// come from the proxy's logical PATH and exported environment, not the
    /// process-global environment.
    #[test]
    fn shell_context_lists_tasks_from_proxy_runtime() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("mise.toml"),
            "[tasks.static_one]\nrun = 'echo static'\n",
        )
        .unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let mise = bin.join("mise");
        std::fs::write(
            &mise,
            "#!/bin/sh\nprintf '[{\"name\":\"ctx-%s\"}]' \"${DOGESH_F1_PROVIDER_ONLY-unset}\"\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&mise).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&mise, permissions).unwrap();

        let mut proxy = TestShellProxy {
            current_dir: dir.path().to_path_buf(),
            command_search_paths: vec![bin],
            exported: [(
                "DOGESH_F1_PROVIDER_ONLY".into(),
                "synthetic-provider-value".into(),
            )]
            .into_iter()
            .collect(),
            ..TestShellProxy::default()
        };

        let rendered = run("{}", &mut proxy).unwrap();
        assert!(
            rendered.contains("ctx-unset") && !rendered.contains("synthetic-provider-value"),
            "shell_context must surface provider tasks from proxy runtime: {rendered}"
        );
    }

    #[test]
    fn shell_context_fingerprint_ignores_unallowlisted_exports() {
        use std::collections::HashMap;

        let runtime = |provider_value: &str, path: &str| {
            let proxy = TestShellProxy {
                exported: HashMap::from([
                    ("PATH".to_string(), path.to_string()),
                    (
                        "DOGESH_F1_PROVIDER_ONLY".to_string(),
                        provider_value.to_string(),
                    ),
                ]),
                ..TestShellProxy::default()
            };
            snapshot_runtime(&proxy)
        };

        let first = runtime("synthetic-one", "/logical/bin");
        let changed_secret = runtime("synthetic-two", "/logical/bin");
        let changed_path = runtime("synthetic-two", "/other/logical/bin");

        assert_eq!(first.cache_scope(), changed_secret.cache_scope());
        assert_ne!(first.cache_scope(), changed_path.cache_scope());
    }

    #[test]
    fn shell_context_keeps_only_exact_allowlisted_exports() {
        use std::collections::{BTreeMap, HashMap};

        let proxy = TestShellProxy {
            exported: HashMap::from([
                ("PATH".into(), "/logical/bin".into()),
                ("HOME".into(), "/logical/home".into()),
                ("LANG".into(), "C".into()),
                ("LC_ALL".into(), "C.UTF-8".into()),
                ("TMPDIR".into(), "/logical/tmp".into()),
                ("TERM".into(), "xterm".into()),
                ("LC_CTYPE".into(), "en_US.UTF-8".into()),
                ("XDG_CONFIG_HOME".into(), "/logical/config".into()),
                (
                    "DOGESH_F1_PROVIDER_KEY".into(),
                    "synthetic-provider-key".into(),
                ),
            ]),
            ..TestShellProxy::default()
        };

        let runtime = snapshot_runtime(&proxy);

        assert_eq!(
            runtime.child_env(),
            &BTreeMap::from([
                ("PATH".into(), "/logical/bin".into()),
                ("HOME".into(), "/logical/home".into()),
                ("LANG".into(), "C".into()),
                ("LC_ALL".into(), "C.UTF-8".into()),
                ("TMPDIR".into(), "/logical/tmp".into()),
            ])
        );
    }

    #[test]
    fn shell_context_discovery_routes_receive_only_the_filtered_environment() {
        use std::os::unix::fs::PermissionsExt;

        fn executable(path: &std::path::Path, contents: &str) {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("mise.toml"), "[tasks.static]\nrun = 'true'\n").unwrap();
        std::fs::write(root.join("nx.json"), "{}").unwrap();
        std::fs::write(root.join("turbo.json"), r#"{"tasks":{"build":{}}}"#).unwrap();
        std::fs::write(root.join("Makefile"), "all:\n").unwrap();
        std::fs::write(root.join("Justfile"), "all:\n  echo all\n").unwrap();
        std::fs::write(root.join("build.gradle"), "").unwrap();
        std::fs::write(root.join("gradlew"), "#!/bin/sh\n").unwrap();

        let bin = root.join("bin");
        executable(
            &bin.join("mise"),
            "#!/bin/sh\nprintf '[{\"name\":\"mise-%s\"}]' \"${DOGESH_F1_PROVIDER_ONLY-unset}\"\n",
        );
        executable(
            &bin.join("make"),
            "#!/bin/sh\nprintf 'make-%s:\\n' \"${DOGESH_F1_PROVIDER_ONLY-unset}\"\n",
        );
        executable(
            &bin.join("just"),
            "#!/bin/sh\nprintf 'just-%s\\n' \"${DOGESH_F1_PROVIDER_ONLY-unset}\"\n",
        );
        executable(
            &bin.join("gradle"),
            "#!/bin/sh\nprintf 'gradle-%s - synthetic task\\n' \"${DOGESH_F1_PROVIDER_ONLY-unset}\"\n",
        );
        executable(
            &root.join("node_modules/.bin/nx"),
            "#!/bin/sh\ncase \"$2\" in projects) printf '[\"nx-%s\"]' \"${DOGESH_F1_PROVIDER_ONLY-unset}\";; project) printf '{\"targets\":{\"build\":{}}}';; esac\n",
        );
        executable(
            &root.join("node_modules/.bin/turbo"),
            "#!/bin/sh\nprintf '{\"tasks\":[{\"task\":\"turbo-%s\"}]}' \"${DOGESH_F1_PROVIDER_ONLY-unset}\"\n",
        );

        let mut proxy = TestShellProxy {
            current_dir: root.to_path_buf(),
            command_search_paths: vec![bin],
            exported: [(
                "DOGESH_F1_PROVIDER_ONLY".into(),
                "synthetic-provider-value".into(),
            )]
            .into_iter()
            .collect(),
            ..TestShellProxy::default()
        };

        let rendered = run("{}", &mut proxy).unwrap();
        for task in [
            "mise-unset",
            "nx-unset:build",
            "turbo-unset",
            "make-unset",
            "just-unset",
            "gradle-unset",
        ] {
            assert!(rendered.contains(task), "missing {task}: {rendered}");
        }
        assert!(!rendered.contains("synthetic-provider-value"), "{rendered}");
    }

    #[test]
    fn shell_context_gradlew_wrapper_receives_the_filtered_environment() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"f1-wrapper\"\nversion = \"0.0.0\"\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("build.gradle"), "").unwrap();
        let wrapper = dir.path().join("gradlew");
        std::fs::write(
            &wrapper,
            "#!/bin/sh\nprintf 'wrapper-%s - synthetic task\\n' \"${DOGESH_F1_PROVIDER_ONLY-unset}\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut proxy = TestShellProxy {
            current_dir: dir.path().to_path_buf(),
            exported: [(
                "DOGESH_F1_PROVIDER_ONLY".into(),
                "synthetic-provider-value".into(),
            )]
            .into_iter()
            .collect(),
            ..TestShellProxy::default()
        };

        let rendered = run("{}", &mut proxy).unwrap();
        assert!(rendered.contains("wrapper-unset"), "{rendered}");
        assert!(!rendered.contains("synthetic-provider-value"), "{rendered}");
    }
}
