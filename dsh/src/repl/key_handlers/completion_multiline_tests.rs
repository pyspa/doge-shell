use super::*;
use crate::{environment::Environment, lisp::Value, repl::Repl, shell::Shell};
use std::{cell::RefCell, rc::Rc};

fn fixture_shell() -> Shell {
    let environment = Environment::new_synthetic();
    let mut env = environment.write();
    env.variable_state.paths.clear();
    env.set_shell_var("DOGESH_COMPLETION_FISH_FALLBACK".into(), "off".into());
    drop(env);
    Shell::new(environment)
}

#[tokio::test]
async fn tab_completes_options_after_a_multiline_argument_or_line_splice() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("日\n本")).unwrap();
    std::fs::create_dir(dir.path().join("日\\\n本")).unwrap();
    std::fs::create_dir(dir.path().join("日本")).unwrap();
    for (first_line, current_line, completed) in [
        (
            "git -C \"日",
            "本\" --vers§wrong tail",
            "本\" --version tail",
        ),
        ("git -C '日", "本' --vers§wrong tail", "本' --version tail"),
        ("git \\", "--vers§wrong tail", "--version tail"),
        ("git \\", "  --vers§", "  --version"),
        ("gi\\", "t --vers§", "t --version"),
        ("git -C 日\\", "本 --vers§", "本 --version"),
        ("git -C \"日\\", "本\" --vers§", "本\" --version"),
        ("git -C '日\\", "本' --vers§", "本' --version"),
        ("echo &\\", "& git --vers§", "& git --version"),
        ("echo |\\", "| git --vers§", "| git --version"),
        ("DSH_A\\", "B=x git --vers§", "B=x git --version"),
        ("2>\\", "&1 git --vers§", "&1 git --version"),
        ("2>&\\", "1 git --vers§", "1 git --version"),
        ("2\\", ">&1 git --vers§", ">&1 git --version"),
        (
            "ls '日",
            "本' --color=\"alw§wrong\" tail",
            "本' --color=\"always\" tail",
        ),
    ] {
        let mut shell = fixture_shell();
        let mut repl = Repl::new(&mut shell);
        repl.completion_ui
            .integrated_completion
            .initialize_command_completion()
            .unwrap();
        repl.terminal_ui.prompt.write().set_current(dir.path());
        repl.input.reset(first_line.into());
        super::super::execution::handle_execute(&mut repl)
            .await
            .unwrap();
        let prefix = format!("{first_line}\n");
        assert_eq!(repl.state.multiline_buffer, prefix);

        let cursor = current_line[..current_line.find('§').unwrap()]
            .chars()
            .count();
        let before = current_line.replace('§', "");
        repl.input.reset(before.clone());
        repl.input.move_to(cursor);
        handle_trigger_completion(&mut repl).await.unwrap();
        assert_eq!(repl.input.as_str(), completed, "{first_line:?}");
        assert_eq!(repl.state.multiline_buffer, prefix);
        assert!(repl.input.undo());
        assert_eq!(repl.input.as_str(), before);
        assert_eq!(repl.input.cursor(), cursor);
    }
}

#[tokio::test]
async fn tab_uses_logical_invocation_for_legacy_completion() {
    let mut shell = fixture_shell();
    let mut repl = Repl::new(&mut shell);
    let prefix = "multiline_probe '日\n";
    let before = "本' \"界wrong\" tail";
    let expected_argument = completion::replace_space(&format!("{prefix}{before}"));
    {
        let engine = repl.shell.lisp_engine.borrow();
        engine.env.borrow_mut().define(
            "check-multiline-completion-input".into(),
            Value::NativeClosure(Rc::new(RefCell::new(move |_, args| {
                assert_eq!(args, [Value::String(expected_argument.clone())]);
                Ok(Value::List(
                    [Value::String("界 hello".into())].into_iter().collect(),
                ))
            }))),
        );
        let callback = engine
            .run("(lambda (input) (check-multiline-completion-input input))")
            .unwrap();
        engine
            .env
            .borrow()
            .autocompletion
            .borrow_mut()
            .push(completion::AutoComplete {
                target: "multiline_probe".into(),
                cmd: None,
                func: Some(callback),
                candidates: None,
            });
    }
    repl.state.multiline_buffer = prefix.into();
    repl.input.reset(before.into());
    repl.input.move_to("本' \"界".chars().count());
    handle_trigger_completion(&mut repl).await.unwrap();
    assert_eq!(repl.input.as_str(), "本' \"界 hello\" tail");
    assert_eq!(repl.state.multiline_buffer, prefix);
}

#[tokio::test]
async fn tab_leaves_spanning_tokens_and_suppressed_regions_intact() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("界-fixture.txt");
    std::fs::write(&file, "fixture").unwrap();
    let query = file.to_str().unwrap().trim_end_matches(".txt");
    for (prefix, line) in [
        ("bg > \"日\n", format!("{query}\"")),
        ("git --ver\\\n", "s".into()),
        ("bg \\\n", format!("# {query}")),
        ("echo |:\n", format!(" select {query}")),
        ("echo |\\\n", format!(": select {query}")),
        ("echo |\\\n", format!("> {query}")),
        ("echo |\\\n", format!("& {query}")),
        ("echo 日 |\n", format!(" bg 2>&{query}")),
        ("echo 2>\\\n", "&1".into()),
        ("echo 2>&\\\n", "1".into()),
        ("echo 2<\\\n", "&0".into()),
        ("2>\\\n", "&1§ git --vers".into()),
    ] {
        let mut shell = fixture_shell();
        let mut repl = Repl::new(&mut shell);
        repl.state.multiline_buffer = prefix.into();
        let cursor = line[..line.find('§').unwrap_or(line.len())].chars().count();
        let line = line.replace('§', "");
        repl.input.reset(line.clone());
        repl.input.move_to(cursor);
        assert!(
            handle_trigger_completion(&mut repl)
                .await
                .unwrap()
                .is_none(),
            "{prefix:?}{line}"
        );
        assert_eq!(repl.input.as_str(), line);
        assert_eq!(repl.input.cursor(), cursor);
        assert_eq!(repl.state.multiline_buffer, prefix);
    }
}

#[tokio::test]
async fn tab_completes_files_after_spliced_redirect_operators() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("界 fixture.txt");
    std::fs::write(&file, "fixture").unwrap();
    let partial = file.to_str().unwrap().trim_end_matches(".txt");
    for (previous, operator) in [("bg >\\\n", ">"), ("bg &\\\n", ">>"), (">\\\n", ">")] {
        let mut shell = fixture_shell();
        let mut repl = Repl::new(&mut shell);
        repl.state.multiline_buffer = previous.into();
        repl.input
            .reset(format!("{operator}\"{partial}wrong\" tail"));
        repl.input
            .move_to(format!("{operator}\"{partial}").chars().count());
        handle_trigger_completion(&mut repl).await.unwrap();
        assert_eq!(
            repl.input.as_str(),
            format!("{operator}\"{}\" tail", file.display())
        );
        assert_eq!(repl.state.multiline_buffer, previous);
    }
}

#[test]
fn tab_sends_full_logical_source_and_character_cursor_to_provider_and_hook() {
    let _guard = completion::subprocess::external_process_test_guard();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let mut shell = fixture_shell();
        shell.environment.write().set_shell_var(
            "DOGESH_EXTERNAL_COMPLETER".into(),
            concat!(
                "printf '%s\\000' \"$DOGESH_COMPLETION_INPUT\" ",
                "\"$DOGESH_COMPLETION_CURSOR\" \"$DOGESH_COMPLETION_COMMAND\" ",
                "\"$DOGESH_COMPLETION_CURRENT_TOKEN\" > completion-context; ",
                "printf 'multiline-external-candidate\\n'"
            )
            .into(),
        );
        let mut repl = Repl::new(&mut shell);
        repl.terminal_ui.prompt.write().set_current(dir.path());
        let hook_args = Rc::new(RefCell::new(Vec::new()));
        let captured = Rc::clone(&hook_args);
        let hook = Value::NativeClosure(Rc::new(RefCell::new(move |_, args| {
            *captured.borrow_mut() = args;
            Ok(Value::NIL)
        })));
        repl.shell.lisp_engine.borrow().env.borrow_mut().define(
            "*completion-hooks*".into(),
            Value::List([hook].into_iter().collect()),
        );
        let prefix = "multiline-provider '日\n";
        let line = "本' multiline-external-old tail";
        let cursor = "本' multiline-external".chars().count();
        repl.state.multiline_buffer = prefix.into();
        repl.input.reset(line.into());
        repl.input.move_to(cursor);

        let mut refresh = repl
            .completion_ui
            .integrated_completion
            .runtime()
            .subscribe_refresh();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                handle_trigger_completion(&mut repl).await.unwrap();
                if repl.input.as_str() != line {
                    break;
                }
                refresh.changed().await.unwrap();
            }
        })
        .await
        .unwrap();

        assert_eq!(repl.input.as_str(), "本' multiline-external-candidate tail");
        assert_eq!(repl.state.multiline_buffer, prefix);
        let logical_cursor = prefix.chars().count() + cursor;
        assert_eq!(
            *hook_args.borrow(),
            [
                Value::String(format!("{prefix}{line}")),
                Value::Int(logical_cursor as _)
            ],
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("completion-context")).unwrap(),
            format!(
                "{prefix}{line}\0{}\0multiline-provider\0multiline-external\0",
                logical_cursor,
            )
        );
    });
}
