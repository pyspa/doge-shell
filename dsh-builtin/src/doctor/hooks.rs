//! `doctor hooks`: AI chat hook configuration, without running any hook.
use crate::ShellProxy;
use dsh_types::Context;
use std::path::Path;

pub(super) fn check_hooks(ctx: &Context, proxy: &mut dyn ShellProxy) {
    use crate::chatgpt::hooks::config;

    if config::nested_in_a_hook() {
        let _ = ctx.write_stdout("skip depth this shell runs inside a hook, so hooks are off");
        return;
    }
    if !config::enabled(proxy) {
        // Say so before listing anything: a report that shows configured hooks
        // while none of them can fire reads as "these are running".
        let _ = ctx.write_stdout(&format!(
            "skip enabled {}=off, so no hook runs in this shell",
            config::HOOKS_ENABLED_KEY
        ));
        return;
    }

    let Some(path) = config::config_path(proxy) else {
        let _ = ctx.write_stdout(&format!(
            "skip config none at {}",
            crate::config_paths::config_home()
                .join(config::HOOKS_CONFIG_FILE)
                .display()
        ));
        return;
    };
    if !path.is_file() {
        let _ = ctx.write_stdout(&format!("skip config none at {}", path.display()));
        return;
    }

    let hooks = match config::read(&path) {
        Ok(hooks) => hooks,
        Err(err) => {
            let _ = ctx.write_stdout(&format!("error config {}: {err}", path.display()));
            return;
        }
    };

    let _ = ctx.write_stdout(&format!("ok config {}", path.display()));
    if hooks.is_empty() {
        let _ = ctx.write_stdout("ok hooks 0");
        return;
    }

    // One immutable runtime for every existence check below: the same
    // logical PATH the hook runner pins programs against at load time.
    let snapshot = proxy.command_runtime_snapshot().ok();
    for hook in hooks.all() {
        let events = hook
            .events
            .iter()
            .map(|event| event.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let state = if hook.enabled { "ok" } else { "skip" };
        let _ = ctx.write_stdout(&format!(
            "{state} hook {} events={events} {} timeout={}ms",
            hook.id,
            describe_matcher(hook),
            hook.timeout_ms()
        ));

        let runnable = snapshot
            .as_ref()
            .is_some_and(|snapshot| program_is_runnable(snapshot, &hook.command[0]));
        if !runnable {
            let _ = ctx.write_stdout(&format!(
                "warn hook {} command not found: {}",
                hook.id, hook.command[0]
            ));
        }

        // Said rather than refused. Listing `session-start` next to
        // `pre-tool-use` is reasonable to write, and rejecting it would break
        // configurations that work; a hook silently never firing on one of its
        // events - or never narrowing at all - is what the person needs told.
        if hook.matcher_narrows_nothing() {
            let _ = ctx.write_stdout(&format!(
                "warn hook {} match narrows nothing; it runs on every call",
                hook.id
            ));
        }
        let kinds = hook.argument_matcher_kinds();
        if !kinds.is_empty() {
            for event in hook.events.iter().filter(|event| !event.carries_a_tool()) {
                let _ = ctx.write_stdout(&format!(
                    "warn hook {} match ({}) cannot be satisfied on {}",
                    hook.id,
                    kinds.join(","),
                    event.as_str()
                ));
            }
        }
    }

    // Scope, not configuration: hooks fire on the `!` chat / agent-task loop
    // only. The command palette, ghost text, and other shell-side AI requests
    // bypass them, so a `pre-tool-use` gate here does not cover those paths.
    let _ = ctx.write_stdout(
        "info scope hooks apply to `!` chat and agent tasks only; shell-side AI (command palette, ghost text) bypasses them",
    );
}

/// The `match` clause as one field per kind, so `doctor` shows what narrowed a
/// hook and not only that something did.
pub(super) fn describe_matcher(hook: &crate::chatgpt::hooks::config::HookDefinition) -> String {
    let Some(matcher) = hook.matcher.as_ref() else {
        return "tools=*".to_string();
    };
    let mut parts = Vec::new();
    let mut push = |name: &str, values: &[String]| {
        if !values.is_empty() {
            parts.push(format!("{name}={}", values.join(",")));
        }
    };
    push("tools", &matcher.tools);
    push("programs", &matcher.programs);
    push("paths", &matcher.paths);
    if !matcher.arguments.is_empty() {
        parts.push(format!(
            "args={}",
            matcher
                .arguments
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    if parts.is_empty() {
        parts.push("tools=*".to_string());
    }
    parts.join(" ")
}

/// Can this program be started at all? Existence only - never execution.
///
/// Bare names resolve through the logical runtime snapshot (the same
/// authority the hook runner's load-time pinning uses); explicit pathnames
/// are checked as files, exactly as the runner would spawn them.
pub(super) fn program_is_runnable(
    snapshot: &dsh_types::process_runtime::CommandRuntimeSnapshot,
    program: &str,
) -> bool {
    let path = Path::new(program);
    if path.is_absolute() || program.contains('/') {
        return path.is_file();
    }
    snapshot.resolve_bare_program(program).is_some()
}
