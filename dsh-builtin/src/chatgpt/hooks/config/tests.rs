use super::*;

/// A subject standing in for one `execute`-shaped tool call.
fn tool_input(tool: &str) -> MatchInput<'_> {
    MatchInput::new(HookSubject::tool(tool, "{}"), Path::new("/repo"))
}

/// One call with real arguments, resolved against `/repo`.
fn call_input<'a>(tool: &'a str, arguments: &'a str) -> MatchInput<'a> {
    MatchInput::new(HookSubject::tool(tool, arguments), Path::new("/repo"))
}

/// An event that carries no tool at all (`user-prompt-submit`).
fn no_tool_input() -> MatchInput<'static> {
    MatchInput::new(HookSubject::none(), Path::new("/repo"))
}

fn one(command: &str) -> String {
    format!(
        r#"{{"version":1,"hooks":[{{"id":"audit","events":["pre-tool-use"],"command":{command}}}]}}"#
    )
}

#[test]
fn parses_a_minimal_hook_definition() {
    let hooks = parse(&one(r#"["/opt/dsh-hooks/hook.sh"]"#)).unwrap();
    let matched = hooks.matching(HookEvent::PreToolUse, &tool_input("execute"));
    assert_eq!(matched.len(), 1);
    assert_eq!(matched[0].id, "audit");
    assert_eq!(matched[0].timeout_ms(), 5_000);
}

/// A field name that does nothing is a hook the author believes is running.
#[test]
fn unknown_field_is_a_parse_error() {
    let err = parse(
            r#"{"version":1,"hooks":[{"id":"a","event":["pre-tool-use"],"command":["/opt/dsh-hooks/hook.sh"]}]}"#,
        )
        .expect_err("a typo must not load quietly");
    assert!(err.contains("event"), "{err}");
}

#[test]
fn string_command_is_rejected_with_an_array_hint() {
    let err = parse(&one(r#""python3 hook.py""#)).expect_err("a string is not an argv");
    assert!(err.contains("array of strings"), "{err}");
    assert!(err.contains("not through a shell"), "{err}");
}

#[test]
fn empty_command_array_is_rejected() {
    let err = parse(&one("[]")).expect_err("nothing to run");
    assert!(err.contains("empty command"), "{err}");
}

#[test]
fn duplicate_hook_id_is_rejected() {
    let err = parse(
        r#"{"version":1,"hooks":[
                {"id":"a","events":["pre-tool-use"],"command":["/opt/dsh-hooks/hook.sh"]},
                {"id":"a","events":["post-tool-use"],"command":["/opt/dsh-hooks/hook.sh"]}]}"#,
    )
    .expect_err("ids name hooks in approvals");
    assert!(err.contains("used twice"), "{err}");
}

#[test]
fn unknown_event_name_is_a_parse_error() {
    let err = parse(
            r#"{"version":1,"hooks":[{"id":"a","events":["PreToolUse"],"command":["/opt/dsh-hooks/hook.sh"]}]}"#,
        )
        .expect_err("event names are kebab-case");
    assert!(
        err.contains("PreToolUse") || err.contains("unknown variant"),
        "{err}"
    );
}

#[test]
fn version_other_than_one_is_rejected() {
    let err = parse(r#"{"version":2,"hooks":[]}"#).expect_err("unknown schema");
    assert!(err.contains("version 1"), "{err}");
}

#[test]
fn timeout_is_clamped_to_the_supported_range() {
    let hooks = parse(
            r#"{"version":1,"hooks":[
                {"id":"slow","events":["post-tool-use"],"command":["/opt/dsh-hooks/hook.sh"],"timeout_ms":9999999},
                {"id":"fast","events":["post-tool-use"],"command":["/opt/dsh-hooks/hook.sh"],"timeout_ms":1}]}"#,
        )
        .unwrap();
    let all = hooks.all();
    assert_eq!(all[0].timeout_ms(), MAX_TIMEOUT_MS);
    assert_eq!(all[1].timeout_ms(), MIN_TIMEOUT_MS);
}

#[test]
fn more_than_eight_hooks_for_one_event_is_rejected() {
    let entries = (0..9)
        .map(|i| {
            format!(
                r#"{{"id":"h{i}","events":["pre-tool-use"],"command":["/opt/dsh-hooks/hook.sh"]}}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let err = parse(&format!(r#"{{"version":1,"hooks":[{entries}]}}"#))
        .expect_err("one tool call must not wait for nine processes");
    assert!(err.contains("at most 8"), "{err}");
}

#[test]
fn a_disabled_file_loads_nothing() {
    let hooks = parse(
            r#"{"version":1,"enabled":false,"hooks":[{"id":"a","events":["pre-tool-use"],"command":["/opt/dsh-hooks/hook.sh"]}]}"#,
        )
        .unwrap();
    assert!(hooks.is_empty());
}

#[test]
fn a_disabled_hook_never_matches() {
    let hooks = parse(
            r#"{"version":1,"hooks":[{"id":"a","events":["pre-tool-use"],"command":["/opt/dsh-hooks/hook.sh"],"enabled":false}]}"#,
        )
        .unwrap();
    assert!(
        hooks
            .matching(HookEvent::PreToolUse, &tool_input("execute"))
            .is_empty()
    );
}

#[test]
fn tool_match_glob_matches_an_mcp_prefix() {
    let hooks = parse(
            r#"{"version":1,"hooks":[{"id":"a","events":["pre-tool-use"],"match":{"tools":["mcp__*","edit"]},"command":["/opt/dsh-hooks/hook.sh"]}]}"#,
        )
        .unwrap();

    assert_eq!(
        hooks
            .matching(HookEvent::PreToolUse, &tool_input("mcp__x__y"))
            .len(),
        1
    );
    assert_eq!(
        hooks
            .matching(HookEvent::PreToolUse, &tool_input("edit"))
            .len(),
        1
    );
    assert!(
        hooks
            .matching(HookEvent::PreToolUse, &tool_input("execute"))
            .is_empty()
    );
    // No tool at all cannot satisfy a tool matcher.
    assert!(
        hooks
            .matching(HookEvent::PreToolUse, &no_tool_input())
            .is_empty()
    );
}

/// Only a trailing `*` is supported. `mcp**` used to pass validation and
/// then match nothing, which is the silent no-op this module exists to
/// prevent.
#[test]
fn a_star_anywhere_but_the_end_is_rejected_rather_than_taken_literally() {
    fn config(pattern: &str) -> String {
        format!(
            r#"{{"version":1,"hooks":[{{"id":"a","events":["pre-tool-use"],"match":{{"tools":["{pattern}"]}},"command":["/opt/dsh-hooks/hook.sh"]}}]}}"#
        )
    }

    for pattern in ["mc*p", "mcp**", "*mcp"] {
        let err = parse(&config(pattern))
            .err()
            .unwrap_or_else(|| panic!("`{pattern}` must be refused"));
        assert!(err.contains("last character"), "{pattern}: {err}");
    }

    // The one supported shape still loads and still matches.
    let hooks = parse(&config("mcp__*")).expect("a trailing star is the supported form");
    assert_eq!(
        hooks
            .matching(HookEvent::PreToolUse, &tool_input("mcp__server__tool"))
            .len(),
        1
    );
}

/// `{"tools":["execute"]}` is every command; these are the narrowings.
fn matcher(extra: &str) -> String {
    format!(
        r#"{{"version":1,"hooks":[{{"id":"audit","events":["pre-tool-use"],
               "match":{{{extra}}},"command":["/opt/dsh-hooks/hook.sh"]}}]}}"#
    )
}

fn fires(config: &str, tool: &str, arguments: &str) -> bool {
    let hooks = parse(config).expect(config);
    !hooks
        .matching(HookEvent::PreToolUse, &call_input(tool, arguments))
        .is_empty()
}

fn command_fires(config: &str, command: &str) -> bool {
    let arguments = serde_json::json!({ "command": command }).to_string();
    fires(config, "execute", &arguments)
}

#[test]
fn programs_matches_through_a_wrapper_and_every_stage() {
    let config = matcher(r#""tools":["execute"],"programs":["rm"]"#);
    for command in [
        "rm -rf /tmp/x",
        "sudo rm -rf /tmp/x",
        "timeout 5 rm /tmp/x",
        "echo hi | rm -rf /tmp/x",
        "cd /tmp && rm -rf x",
    ] {
        assert!(command_fires(&config, command), "{command}");
    }
    for command in ["ls -la", "cargo test", "echo rm"] {
        assert!(!command_fires(&config, command), "{command}");
    }
}

#[test]
fn programs_is_a_word_prefix_not_a_substring() {
    let config = matcher(r#""tools":["execute"],"programs":["git push"]"#);
    assert!(command_fires(&config, "git push --force"));
    assert!(!command_fires(&config, "git pushx"));
    assert!(!command_fires(&config, "git status"));
}

/// Dead configuration that looks like a filter is the failure mode here.
#[test]
fn programs_without_an_execute_tool_entry_is_a_load_error() {
    let err = parse(&matcher(r#""tools":["edit"],"programs":["rm"]"#)).unwrap_err();
    assert!(err.contains("`programs` needs"), "{err}");

    // A trailing-star pattern that covers `execute` is fine.
    assert!(parse(&matcher(r#""tools":["exec*"],"programs":["rm"]"#)).is_ok());
}

#[test]
fn an_untokenizable_programs_entry_is_a_load_error() {
    let err = parse(&matcher(r#""tools":["execute"],"programs":["'unclosed"]"#)).unwrap_err();
    assert!(err.contains("is not a command"), "{err}");
}

/// `{"tools": []}` has always meant "every call". Refusing it would refuse
/// the whole chat for a configuration that worked yesterday.
#[test]
fn an_empty_match_object_loads_and_runs_on_every_call() {
    for extra in ["", r#""tools":[]"#] {
        let config = matcher(extra);
        assert!(command_fires(&config, "anything at all"), "{extra}");
        let hooks = parse(&config).expect(extra);
        assert!(hooks.all()[0].matcher_narrows_nothing(), "{extra}");
    }

    // A matcher that does narrow is not reported as narrowing nothing.
    let narrow = parse(&matcher(r#""tools":["execute"]"#)).unwrap();
    assert!(!narrow.all()[0].matcher_narrows_nothing());
}

#[test]
fn paths_glob_matches_a_path_argument() {
    let config = matcher(r#""tools":["edit"],"paths":["/etc/**"]"#);
    assert!(fires(&config, "edit", r#"{"path":"/etc/hosts"}"#));
    assert!(!fires(&config, "edit", r#"{"path":"/tmp/hosts"}"#));
}

/// A relative argument has to be judged where it will land, not as written.
#[test]
fn paths_matches_the_lexically_absolute_form() {
    let config = matcher(r#""tools":["read_file"],"paths":["/repo/src/**"]"#);
    assert!(fires(&config, "read_file", r#"{"path":"src/a.rs"}"#));
}

#[test]
fn paths_does_not_let_dot_dot_dodge_the_glob() {
    let config = matcher(r#""tools":["read_file"],"paths":["/etc/**"]"#);
    assert!(fires(
        &config,
        "read_file",
        r#"{"path":"sub/../../etc/shadow"}"#
    ));
}

/// Every token, not just the program: `bash <path>/run.sh` names a path.
#[test]
fn paths_matches_a_token_of_an_execute_command() {
    let config = matcher(r#""tools":["execute"],"paths":["/etc/**"]"#);
    assert!(command_fires(&config, "cat /etc/shadow"));
    assert!(command_fires(&config, "bash /etc/init.d/thing"));
    assert!(!command_fires(&config, "cat /tmp/shadow"));
}

/// Inside a command line a bare word could be a program or a file, and the
/// two are not distinguishable. Every ambiguity in this module resolves
/// toward firing, because a needless hook run costs a hook run while a
/// missed one costs the check.
#[test]
fn a_bare_filename_argument_still_reaches_a_paths_matcher() {
    let config = matcher(r#""tools":["execute"],"paths":["**/*.env"]"#);
    assert!(command_fires(&config, "rm .env"));
    assert!(command_fires(&config, "rm ./.env"));
    assert!(command_fires(&config, "cat /repo/.env"));
    assert!(!command_fires(&config, "rm notes.md"));

    // The cost of that choice: a broad pattern is true of a program name
    // too. Specific patterns are the useful ones.
    let broad = matcher(r#""tools":["execute"],"paths":["/repo/**"]"#);
    assert!(command_fires(&broad, "cat"));

    // An option is still not a path; a real one is written `./-foo`.
    let dashes = matcher(r#""tools":["execute"],"paths":["/repo/-p"]"#);
    assert!(!command_fires(&dashes, "cargo test -p"));
}

/// `execute` runs the command in its `cwd` argument, so that is where a
/// relative token lands - the hole `touches_skill_file` closed for skill
/// scripts, in the matcher this time.
#[test]
fn paths_resolve_against_the_calls_own_cwd() {
    let config = matcher(r#""tools":["execute"],"paths":["/etc/sub/**"]"#);
    assert!(fires(
        &config,
        "execute",
        r#"{"command":"cat sub/secret","cwd":"/etc"}"#
    ));
    // Without the `cwd` argument the shell's directory is the base.
    assert!(!fires(
        &config,
        "execute",
        r#"{"command":"cat sub/secret"}"#
    ));
}

/// A provider that omits `arguments` for a no-argument tool sends `""`.
/// Treating that as unreadable made every argument matcher fire on it.
#[test]
fn an_absent_arguments_string_is_missing_not_unreadable() {
    let config = matcher(r#""tools":["execute"],"programs":["rm"]"#);
    assert!(!fires(&config, "execute", ""));
    assert!(!fires(&config, "execute", "   "));
    // Text that is actually present and unparseable still fires.
    assert!(fires(&config, "execute", "{not json"));
}

/// An MCP tool is as likely to carry paths in a list or a nested object.
#[test]
fn paths_are_inferred_from_nested_and_repeated_fields() {
    let config = matcher(r#""tools":["mcp__*"],"paths":["/etc/**"]"#);
    for arguments in [
        r#"{"paths":["/tmp/a","/etc/myservice.conf"]}"#,
        r#"{"target":{"path":"/etc/myservice.conf"}}"#,
        r#"{"jobs":[{"src":"/etc/myservice.conf","dst":"/tmp/x"}]}"#,
    ] {
        assert!(fires(&config, "mcp__fs__write", arguments), "{arguments}");
    }
    assert!(!fires(
        &config,
        "mcp__fs__write",
        r#"{"target":{"path":"/tmp/x"}}"#
    ));
}

/// `skill_manage`'s `file` is relative to the skill directory, so joining
/// it with the chat's cwd tests a path the call never touches.
#[test]
fn skill_manage_offers_no_path_candidates() {
    let config = matcher(r#""tools":["skill_manage"],"paths":["/repo/**"]"#);
    assert!(!fires(
        &config,
        "skill_manage",
        r#"{"action":"write_file","name":"deploy","scope":"project","file":"scripts/run.sh"}"#
    ));
}

#[test]
fn paths_matches_the_execute_cwd_argument() {
    let config = matcher(r#""tools":["execute"],"paths":["/etc/**"]"#);
    assert!(fires(
        &config,
        "execute",
        r#"{"command":"ls","cwd":"/etc/apache2"}"#
    ));
}

/// No table for an MCP server's arguments; path *shape* stands in.
#[test]
fn paths_infers_a_path_shaped_field_of_an_unknown_tool() {
    let config = matcher(r#""tools":["mcp__*"],"paths":["/etc/**"]"#);
    assert!(fires(
        &config,
        "mcp__fs__write",
        r#"{"target":"/etc/myservice.conf","mode":"append"}"#
    ));
    assert!(!fires(
        &config,
        "mcp__fs__write",
        r#"{"target":"notapath","mode":"append"}"#
    ));
}

#[test]
fn paths_with_an_invalid_glob_is_a_load_error() {
    let err = parse(&matcher(r#""tools":["edit"],"paths":["/etc/[bad"]"#)).unwrap_err();
    assert!(err.contains("is not a glob"), "{err}");
}

#[test]
fn arguments_match_is_exact_equality() {
    let config = matcher(r#""tools":["search"],"arguments":{"type":"content"}"#);
    assert!(fires(
        &config,
        "search",
        r#"{"query":"x","type":"content"}"#
    ));
    assert!(!fires(
        &config,
        "search",
        r#"{"query":"x","type":"filename"}"#
    ));
    assert!(!fires(&config, "search", r#"{"query":"x"}"#));

    let numeric = matcher(r#""tools":["read_file"],"arguments":{"offset":1}"#);
    assert!(fires(&numeric, "read_file", r#"{"path":"a","offset":1}"#));
    assert!(!fires(&numeric, "read_file", r#"{"path":"a","offset":2}"#));
}

#[test]
fn an_arguments_entry_that_is_not_a_scalar_is_a_load_error() {
    let err = parse(&matcher(r#""tools":["edit"],"arguments":{"path":["a"]}"#)).unwrap_err();
    assert!(err.contains("must be a string, number or boolean"), "{err}");
}

/// Kinds are ANDed; each one alone is not enough.
#[test]
fn matcher_kinds_are_anded() {
    let config = matcher(r#""tools":["execute"],"programs":["rm"],"paths":["/etc/**"]"#);
    assert!(!command_fires(&config, "rm -rf /tmp/x"));
    assert!(!command_fires(&config, "cat /etc/shadow"));
    assert!(command_fires(&config, "rm -rf /etc/thing"));
}

/// The masked form of this command is `mkdir -p ***`, which no `/etc/**`
/// matcher can see. Matching therefore reads the unmasked arguments; only
/// the payload the hook receives is masked.
#[test]
fn matching_uses_the_unmasked_arguments() {
    let config = matcher(r#""tools":["execute"],"paths":["/etc/**"]"#);
    let command = "mkdir -p /etc/myapp";
    assert!(
        crate::safety_policy::redact_sensitive_text(command).contains("***"),
        "this test is only meaningful while `-p` is masked"
    );
    assert!(command_fires(&config, command));
}

/// A mismatched quote must not be a way to skip a check.
#[test]
fn unreadable_arguments_make_an_argument_matcher_fire() {
    for extra in [
        r#""tools":["execute"],"programs":["rm"]"#,
        r#""tools":["execute"],"paths":["/etc/**"]"#,
    ] {
        let config = matcher(extra);
        // A command line that does not tokenise.
        assert!(command_fires(&config, "echo 'unclosed"), "{extra}");
        // Arguments that are not JSON at all.
        assert!(fires(&config, "execute", "{not json"), "{extra}");
    }
    let args = matcher(r#""tools":["search"],"arguments":{"type":"content"}"#);
    assert!(fires(&args, "search", "{not json"));
}

/// Structural absence is the other direction: nothing to be true of.
#[test]
fn an_argument_matcher_cannot_be_satisfied_without_arguments() {
    for extra in [
        r#""tools":["execute"],"programs":["rm"]"#,
        r#""tools":["execute"],"paths":["/etc/**"]"#,
        r#""tools":["execute"],"arguments":{"command":"rm"}"#,
    ] {
        let hooks = parse(&matcher(extra)).expect(extra);
        assert!(
            hooks
                .matching(HookEvent::PreToolUse, &no_tool_input())
                .is_empty(),
            "{extra}"
        );
    }
}

/// The shape every existing configuration has.
#[test]
fn a_tools_only_config_still_loads() {
    let config = matcher(r#""tools":["execute"]"#);
    assert!(command_fires(&config, "anything at all"));
}

/// The per-event cap has to see every event, including the next one added.
#[test]
fn every_event_is_covered_by_the_hook_cap() {
    for event in HookEvent::ALL {
        let name = event.as_str();
        let hooks: Vec<String> = (0..=MAX_HOOKS_PER_EVENT)
            .map(|i| {
                format!(
                    r#"{{"id":"h{i}","events":["{name}"],"command":["/opt/dsh-hooks/hook.sh"]}}"#
                )
            })
            .collect();
        let config = format!(r#"{{"version":1,"hooks":[{}]}}"#, hooks.join(","));
        let err = parse(&config).unwrap_err();
        assert!(err.contains(name), "{name}: {err}");
    }
}

/// Gate-ness and context-carrying are properties of every event, so assert
/// them over `ALL` rather than over a list that drifts.
#[test]
fn every_events_gate_and_context_answers_are_pinned() {
    for event in HookEvent::ALL {
        let expect_gate = matches!(event, HookEvent::UserPromptSubmit | HookEvent::PreToolUse);
        assert_eq!(event.is_gate(), expect_gate, "{}", event.as_str());

        let expect_context = matches!(
            event,
            HookEvent::UserPromptSubmit | HookEvent::PreToolUse | HookEvent::PostToolUse
        );
        assert_eq!(event.uses_context(), expect_context, "{}", event.as_str());
    }
}

/// Compaction cannot be refused: the request would just be too large.
#[test]
fn pre_compact_neither_gates_nor_takes_context() {
    assert!(!HookEvent::PreCompact.is_gate());
    assert!(!HookEvent::PreCompact.uses_context());
    let hooks = parse(
        r#"{"version":1,"hooks":[{"id":"c","events":["pre-compact"],
               "command":["/opt/dsh-hooks/hook.sh"]}]}"#,
    )
    .unwrap();
    assert_eq!(
        hooks
            .matching(HookEvent::PreCompact, &no_tool_input())
            .len(),
        1
    );
}

/// 664 is what `umask 002` produces, and on those systems the group is the
/// user's own. Refusing it meant creating the file the ordinary way stopped
/// `!` from working at all.
#[test]
fn a_group_writable_config_loads_but_a_world_writable_one_does_not() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = dir.path().join("ai-hooks.json");
    std::fs::write(&path, r#"{"version":1,"hooks":[]}"#).unwrap();

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o664)).unwrap();
    assert!(read(&path).is_ok(), "umask 002 must not brick the chat");

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
    let err = read(&path).expect_err("a world-writable command list is a way in");
    assert!(err.contains("world-writable"), "{err}");

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(read(&path).is_ok());
}

/// The file's own mode is no protection when anyone can replace it.
#[test]
fn a_world_writable_directory_is_refused() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("conf");
    std::fs::create_dir(&nested).unwrap();
    let path = nested.join("ai-hooks.json");
    std::fs::write(&path, r#"{"version":1,"hooks":[]}"#).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o777)).unwrap();

    let err = read(&path).expect_err("anyone could swap the file");
    assert!(err.contains("world-writable"), "{err}");

    std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(read(&path).is_ok());
}

/// A relative program is resolved after the runner chdirs, so `./hook.sh`
/// meant "whatever sits in the repository the user cd'd into".
#[test]
fn a_relative_hook_command_is_refused_at_load_time() {
    for program in ["./hook.sh", "../hook.sh", "hooks/audit.sh"] {
        let err = parse(&one(&format!(r#"["{program}"]"#)))
            .err()
            .unwrap_or_else(|| panic!("`{program}` must be refused"));
        assert!(err.contains("relative path"), "{program}: {err}");
    }
}

#[test]
fn a_bare_command_name_is_pinned_to_its_path_entry() {
    use dsh_types::process_runtime::CommandRuntimeSnapshot;
    use std::collections::HashMap;

    // A `true` binary on the logical PATH pins to its absolute path; the
    // process-global PATH is never consulted.
    let dir = tempfile::tempdir().unwrap();
    let pinned = dir.path().join("true");
    std::fs::write(&pinned, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&pinned).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&pinned, perms).unwrap();
    }
    let snapshot = CommandRuntimeSnapshot::new(
        vec![dir.path().to_path_buf()],
        HashMap::new(),
        dir.path().to_path_buf(),
    );
    assert_eq!(
        super::pin_program(&snapshot, "true").unwrap(),
        pinned.to_string_lossy().into_owned()
    );

    // Left alone when not on the logical PATH — never left as something a
    // later chdir could reinterpret.
    let empty = CommandRuntimeSnapshot::new(Vec::new(), HashMap::new(), dir.path().to_path_buf());
    assert_eq!(super::pin_program(&empty, "true").unwrap(), "true");
}

#[test]
fn an_empty_file_is_not_an_error() {
    assert!(parse("   \n").unwrap().is_empty());
}

/// The file is read once per turn at most, so an edit has to invalidate the
/// cache rather than wait for a new shell.
#[test]
fn cache_reloads_when_the_file_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ai-hooks.json");

    std::fs::write(&path, r#"{"version":1,"hooks":[]}"#).unwrap();
    clear_cache();
    assert!(read(&path).unwrap().is_empty());

    std::fs::write(
            &path,
            r#"{"version":1,"hooks":[{"id":"a","events":["post-tool-use"],"command":["/opt/dsh-hooks/hook.sh"]}]}"#,
        )
        .unwrap();
    let reloaded = read(&path).unwrap();
    assert_eq!(reloaded.all().len(), 1);
}
