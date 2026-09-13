//! `doctor project`: project markers, detected runtimes, and activations.
use crate::ShellProxy;
use crate::project_context;
use dsh_types::Context;
use std::path::Path;

pub(super) fn check_project(ctx: &Context, proxy: &mut dyn ShellProxy, current_dir: &Path) {
    let project = project_context::resolve_project_context(current_dir);

    let _ = ctx.write_stdout(&format!("ok cwd {}", current_dir.display()));
    let _ = ctx.write_stdout(&format!(
        "ok project-root {}",
        project.project_root.display()
    ));

    if project.project_markers.is_empty() {
        let _ = ctx.write_stdout("warn markers none");
    } else {
        let _ = ctx.write_stdout(&format!(
            "ok markers {}",
            project.project_markers.join(", ")
        ));
    }

    if project.runtimes.is_empty() {
        let _ = ctx.write_stdout("skip runtime none");
    } else {
        for runtime in project.runtimes {
            let version = runtime.version.unwrap_or_else(|| "-".to_string());
            let _ = ctx.write_stdout(&format!(
                "ok runtime {} source={} version={} path={}",
                runtime.name,
                runtime.source,
                version,
                runtime.path.display()
            ));
        }
    }

    if project.activations.is_empty() {
        let _ = ctx.write_stdout("skip activation none");
    } else {
        for activation in project.activations {
            let _ = ctx.write_stdout(&format!(
                "ok activation {} {}",
                activation.kind,
                activation.path.display()
            ));
        }
    }

    for line in proxy.completion_diagnostics() {
        let _ = ctx.write_stdout(&format!("ok {line}"));
    }
}
