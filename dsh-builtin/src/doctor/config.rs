//! `doctor config`: config.lisp and runtime skills directory presence.
use dsh_types::Context;
use std::fs;

pub(super) fn check_config(ctx: &Context) {
    let config_path = crate::config_paths::config_file("config.lisp");
    if config_path.exists() {
        let _ = ctx.write_stdout(&format!("ok config {}", config_path.display()));
    } else {
        let _ = ctx.write_stdout(&format!("warn missing {}", config_path.display()));
    }

    let skills_dir = crate::config_paths::skills_dir();
    if skills_dir.exists() {
        let count = fs::read_dir(&skills_dir)
            .map(|entries| entries.count())
            .unwrap_or(0);
        let _ = ctx.write_stdout(&format!(
            "ok runtime-skills {} entries={count}",
            skills_dir.display()
        ));
    } else {
        let _ = ctx.write_stdout(&format!("warn missing {}", skills_dir.display()));
    }
}
