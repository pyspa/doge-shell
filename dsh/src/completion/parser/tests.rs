use super::*;

#[test]
fn test_tokenize_simple() {
    let parser = CommandLineParser::new();
    let tokens = parser.tokenize("git add file.txt");
    assert_eq!(tokens, vec!["git", "add", "file.txt"]);
}

#[test]
fn test_tokenize_with_quotes() {
    let parser = CommandLineParser::new();
    let tokens = parser.tokenize("git commit -m \"test message\"");
    assert_eq!(tokens, vec!["git", "commit", "-m", "\"test message\""]);
}

#[test]
fn test_parse_command_only() {
    let parser = CommandLineParser::new();
    let result = parser.parse("git", 3);
    assert_eq!(result.command, "git");
    assert_eq!(result.completion_context, CompletionContext::Command);
}

#[test]
fn test_parse_command_with_space() {
    let parser = CommandLineParser::new();
    let result = parser.parse("git ", 4);
    assert_eq!(result.command, "git");
    assert_eq!(result.completion_context, CompletionContext::SubCommand);
}

#[test]
fn test_parse_command_without_space() {
    let parser = CommandLineParser::new();
    let result = parser.parse("git", 3);
    assert_eq!(result.command, "git");
    assert_eq!(result.completion_context, CompletionContext::Command);
}

#[test]
fn test_parse_subcommand_with_space() {
    let parser = CommandLineParser::new();
    let result = parser.parse("git add", 7);
    assert_eq!(result.command, "git");
    assert_eq!(result.subcommand_path, vec!["add"]);
    assert_eq!(result.completion_context, CompletionContext::SubCommand);
}

#[test]
fn test_has_space_after_command() {
    let parser = CommandLineParser::new();
    assert!(parser.has_space_after_command("git ", "git"));
    assert!(parser.has_space_after_command("git add", "git"));
    assert!(!parser.has_space_after_command("git", "git"));
    assert!(!parser.has_space_after_command("gitadd", "git"));
}

#[test]
fn test_parse_nested_subcommand() {
    let parser = CommandLineParser::new();
    let result = parser.parse("git remote add", 14);

    assert_eq!(result.command, "git");
    assert_eq!(result.subcommand_path, vec!["remote", "add"]);
    assert_eq!(result.completion_context, CompletionContext::SubCommand);
}

#[test]
fn test_parse_consonant_only_subcommand() {
    let parser = CommandLineParser::new();
    let input = "git rm ";
    let result = parser.parse(input, input.len());

    assert_eq!(result.command, "git");
    assert_eq!(result.subcommand_path, vec!["rm"]);
    assert_eq!(
        result.completion_context,
        CompletionContext::Argument {
            arg_index: 0,
            arg_type: None
        }
    );
}

#[test]
fn test_parse_long_subcommand_name() {
    let parser = CommandLineParser::new();
    let input = "foo upgrade-interactive ";
    let result = parser.parse(input, input.len());

    assert_eq!(result.command, "foo");
    assert_eq!(result.subcommand_path, vec!["upgrade-interactive"]);
    assert_eq!(
        result.completion_context,
        CompletionContext::Argument {
            arg_index: 0,
            arg_type: None
        }
    );
}

#[test]
fn test_cursor_token_index_handles_multibyte_tokens() {
    let parser = CommandLineParser::new();
    let input = "cmd あい うえ";
    // Cursor is inside the third token ("うえ") after "う" (3 bytes).
    // "cmd " (4) + "あい " (7) + "う" (3) = 14
    let result = parser.parse(input, 14);
    assert_eq!(result.current_token, "う");
}

#[test]
fn test_cursor_inside_multibyte_char() {
    let parser = CommandLineParser::new();
    let input = "cmd あ";
    // "あ" is 3 bytes (e.g., e3 81 82).
    // Cursor at 5 (inside "あ", which is 4..7).
    // 5 - 4 = 1. Inside first byte or second byte.
    // Should truncate to ""
    let result = parser.parse(input, 5);
    assert_eq!(result.current_token, "");
}

#[test]
fn test_parse_more_than_two_subcommands() {
    let parser = CommandLineParser::new();
    // Use tokens that satisfy looks_like_subcommand heuristic.
    let input = "foo alpha delta omega";
    let result = parser.parse(input, input.len());
    assert_eq!(result.subcommand_path, vec!["alpha", "delta", "omega"]);
}

#[test]
fn test_parse_long_option() {
    let parser = CommandLineParser::new();
    let result = parser.parse("git commit --message", 20);

    assert_eq!(result.command, "git");
    assert_eq!(result.subcommand_path, vec!["commit"]);
    assert_eq!(result.completion_context, CompletionContext::LongOption);
    assert!(result.specified_options.contains(&"--message".to_string()));
}

#[test]
fn test_parse_short_option() {
    let parser = CommandLineParser::new();
    let result = parser.parse("git commit -m", 13);

    assert_eq!(result.command, "git");
    assert_eq!(result.subcommand_path, vec!["commit"]);
    assert_eq!(result.completion_context, CompletionContext::ShortOption);
}

#[test]
fn test_parse_single_dash_long_option() {
    let parser = CommandLineParser::new();
    let result = parser.parse("git commit -message", 19);

    assert_eq!(result.command, "git");
    assert_eq!(result.subcommand_path, vec!["commit"]);
    assert_eq!(result.completion_context, CompletionContext::LongOption);
    assert!(result.specified_options.contains(&"-message".to_string()));
}

#[test]
fn test_parse_option_value() {
    let parser = CommandLineParser::new();
    let result = parser.parse("git commit -m \"test", 19);

    assert_eq!(result.command, "git");
    assert_eq!(result.subcommand_path, vec!["commit"]);
    if let CompletionContext::OptionValue { option_name, .. } = result.completion_context {
        assert_eq!(option_name, "-m");
    } else {
        panic!("Expected OptionValue context");
    }
}

#[test]
fn test_parse_inline_long_option_value() {
    let parser = CommandLineParser::new();
    let result = parser.parse("kubectl --context=de", "kubectl --context=de".len());

    assert_eq!(result.command, "kubectl");
    assert_eq!(result.raw_args, vec!["--context=de".to_string()]);
    assert_eq!(result.specified_options, vec!["--context".to_string()]);
    assert_eq!(result.current_token, "de");
    assert!(matches!(
        result.completion_context,
        CompletionContext::OptionValue { option_name, .. } if option_name == "--context"
    ));
}

#[test]
fn test_parse_inline_long_option_empty_value() {
    let parser = CommandLineParser::new();
    let result = parser.parse("kubectl --context=", "kubectl --context=".len());

    assert_eq!(result.raw_args, vec!["--context=".to_string()]);
    assert_eq!(result.specified_options, vec!["--context".to_string()]);
    assert_eq!(result.current_token, "");
    assert!(matches!(
        result.completion_context,
        CompletionContext::OptionValue { option_name, .. } if option_name == "--context"
    ));
}

#[test]
fn test_parse_invalid_inline_long_option_stays_option_like() {
    let parser = CommandLineParser::new();
    let result = parser.parse("cmd --=x", "cmd --=x".len());

    assert_eq!(result.raw_args, vec!["--=x".to_string()]);
    assert_eq!(result.specified_options, vec!["--=x".to_string()]);
    assert_eq!(result.current_token, "--=x");
    assert_eq!(result.completion_context, CompletionContext::LongOption);
}

#[test]
fn test_parse_short_attached_value_is_out_of_scope() {
    let parser = CommandLineParser::new();
    let result = parser.parse("cmd -x=y", "cmd -x=y".len());

    assert_eq!(result.raw_args, vec!["-x=y".to_string()]);
    assert_eq!(result.specified_options, vec!["-x=y".to_string()]);
    assert_eq!(result.current_token, "-x=y");
    assert_eq!(result.completion_context, CompletionContext::LongOption);
}

#[test]
fn test_parse_argument() {
    let parser = CommandLineParser::new();
    let result = parser.parse("git add file", 12);

    assert_eq!(result.command, "git");
    assert_eq!(result.subcommand_path, vec!["add"]);
    if let CompletionContext::Argument { arg_index, .. } = result.completion_context {
        assert_eq!(arg_index, 0);
    } else {
        panic!(
            "Expected Argument context, got: {:?}",
            result.completion_context
        );
    }
}

#[test]
fn test_parse_double_dash_option() {
    let parser = CommandLineParser::new();
    let result = parser.parse("git add --", 10);

    assert_eq!(result.command, "git");
    assert_eq!(result.subcommand_path, vec!["add"]);
    assert_eq!(result.current_token, "--");
    assert_eq!(result.completion_context, CompletionContext::LongOption);
}

#[test]
fn test_parse_after_double_dash_as_argument() {
    let parser = CommandLineParser::new();
    let result = parser.parse("git add -- -file", "git add -- -file".len());

    assert_eq!(result.command, "git");
    assert_eq!(result.subcommand_path, vec!["add"]);
    assert!(result.specified_options.is_empty());
    assert_eq!(result.specified_arguments, vec!["-file".to_string()]);
    assert_eq!(result.current_token, "-file");
    assert_eq!(
        result.completion_context,
        CompletionContext::Argument {
            arg_index: 0,
            arg_type: None
        }
    );
}

#[test]
fn test_parse_trailing_gap_after_double_dash_as_argument() {
    let parser = CommandLineParser::new();
    let result = parser.parse("git add -- ", "git add -- ".len());

    assert_eq!(result.raw_args, vec!["--".to_string(), "".to_string()]);
    assert_eq!(result.specified_arguments, vec!["".to_string()]);
    assert_eq!(
        result.completion_context,
        CompletionContext::Argument {
            arg_index: 0,
            arg_type: None
        }
    );
}

#[test]
fn test_space_detection_edge_cases() {
    let parser = CommandLineParser::new();

    // Test with tab character
    let result = parser.parse("git\t", 4);
    assert_eq!(result.completion_context, CompletionContext::SubCommand);

    // Test with multiple spaces
    let result = parser.parse("git   ", 6);
    assert_eq!(result.completion_context, CompletionContext::SubCommand);

    // Test cursor at different positions
    let result = parser.parse("git ", 3); // cursor at end of command
    assert_eq!(result.completion_context, CompletionContext::Command);

    let result = parser.parse("git ", 4); // cursor at space
    assert_eq!(result.completion_context, CompletionContext::SubCommand);
}

#[test]
fn test_subcommand_completion_requires_space() {
    let parser = CommandLineParser::new();

    // Without space - should be command completion
    let result = parser.parse("git", 3);
    assert_eq!(result.completion_context, CompletionContext::Command);

    // With space - should be subcommand completion
    let result = parser.parse("git ", 4);
    assert_eq!(result.completion_context, CompletionContext::SubCommand);

    // Partial subcommand without space after command - should be command completion
    let result = parser.parse("gita", 4);
    assert_eq!(result.completion_context, CompletionContext::Command);

    // Partial subcommand with space after command - should be subcommand completion
    let result = parser.parse("git a", 5);
    assert_eq!(result.completion_context, CompletionContext::SubCommand);
}

#[test]
fn test_redirect_triggers_argument_completion() {
    let parser = CommandLineParser::new();
    let input = "cat > ";
    let result = parser.parse(input, input.len());

    if let CompletionContext::Argument { arg_index, .. } = result.completion_context {
        assert_eq!(arg_index, 0);
    } else {
        panic!(
            "Expected Argument context after redirect, got {:?}",
            result.completion_context
        );
    }

    assert_eq!(result.current_token, "");
    assert!(result.specified_arguments.is_empty());
}

#[test]
fn test_redirect_target_is_not_counted_as_argument() {
    let parser = CommandLineParser::new();
    let input = "cat file > out";
    let result = parser.parse(input, input.len());

    assert_eq!(result.specified_arguments, vec!["file".to_string()]);
    assert_eq!(result.current_token, "out");

    if let CompletionContext::Argument { arg_index, .. } = result.completion_context {
        assert_eq!(arg_index, 1);
    } else {
        panic!(
            "Expected Argument context for redirect target, got {:?}",
            result.completion_context
        );
    }
}

// --- Group 1: Tokenizer Edge Cases ---

#[test]
fn test_tokenize_empty_input() {
    let parser = CommandLineParser::new();
    let tokens = parser.tokenize("");
    assert!(tokens.is_empty());
}

#[test]
fn test_tokenize_whitespace_only() {
    let parser = CommandLineParser::new();
    let tokens = parser.tokenize("   ");
    assert!(tokens.is_empty());
}

#[test]
fn test_tokenize_single_quotes() {
    let parser = CommandLineParser::new();
    let tokens = parser.tokenize("echo 'hello world'");
    assert_eq!(tokens, vec!["echo", "'hello world'"]);
}

#[test]
fn test_tokenize_unclosed_quote() {
    let parser = CommandLineParser::new();
    // Unclosed quote should be tokenized as is, from start of quote to end of string
    let tokens = parser.tokenize("echo \"hello");
    assert_eq!(tokens, vec!["echo", "\"hello"]);
}

#[test]
fn test_tokenize_mixed_quotes() {
    let parser = CommandLineParser::new();
    let tokens = parser.tokenize("cmd 'a b' \"c d\" e");
    assert_eq!(tokens, vec!["cmd", "'a b'", "\"c d\"", "e"]);
}

#[test]
fn test_tokenize_backslash_escaped_space() {
    let parser = CommandLineParser::new();
    let tokens = parser.tokenize(r#"cat dir\ with\ space/fo"#);
    assert_eq!(tokens, vec!["cat", r#"dir\ with\ space/fo"#]);
}

#[test]
fn test_parse_backslash_escaped_path_argument() {
    let parser = CommandLineParser::new();
    let input = r#"cat dir\ with\ space/fo"#;
    let result = parser.parse(input, input.len());

    assert_eq!(result.current_token, r#"dir\ with\ space/fo"#);
    assert_eq!(
        result.completion_context,
        CompletionContext::Argument {
            arg_index: 0,
            arg_type: None
        }
    );
}

#[test]
fn test_tokenize_escaped_quote_does_not_enter_quote_state() {
    let parser = CommandLineParser::new();
    let tokens = parser.tokenize(r#"echo \"quoted tail"#);
    assert_eq!(tokens, vec!["echo", r#"\"quoted"#, "tail"]);
}

// --- Group 2: looks_like_subcommand Heuristics ---

#[test]
fn test_looks_like_subcommand_option() {
    let parser = CommandLineParser::new();
    assert!(!parser.looks_like_subcommand("-v"));
    assert!(!parser.looks_like_subcommand("--help"));
}

#[test]
fn test_looks_like_subcommand_file_path() {
    let parser = CommandLineParser::new();
    assert!(!parser.looks_like_subcommand("./foo"));
    assert!(!parser.looks_like_subcommand("a/b"));
    // Backslash on windows/generally
    assert!(!parser.looks_like_subcommand("a\\b"));
}

#[test]
fn test_looks_like_subcommand_file_ext() {
    let parser = CommandLineParser::new();
    assert!(!parser.looks_like_subcommand("foo.txt"));
    assert!(!parser.looks_like_subcommand("script.sh"));
    // But "v1.2" might be a version number often used as subcommand or arg?
    // The heuristic says if it contains '.', it's not a subcommand.
    assert!(!parser.looks_like_subcommand("v1.2"));
}

#[test]
fn test_looks_like_subcommand_too_short() {
    let parser = CommandLineParser::new();
    assert!(!parser.looks_like_subcommand("a"));
    assert!(parser.looks_like_subcommand("up")); // 2 chars ok
}

#[test]
fn test_looks_like_subcommand_too_long() {
    let parser = CommandLineParser::new();
    let long_cmd = "a".repeat(33);
    assert!(!parser.looks_like_subcommand(&long_cmd));
}

#[test]
fn test_looks_like_subcommand_valid() {
    let parser = CommandLineParser::new();
    assert!(parser.looks_like_subcommand("add"));
    assert!(parser.looks_like_subcommand("commit"));
    assert!(parser.looks_like_subcommand("self-update"));
    assert!(parser.looks_like_subcommand("install"));
}

// --- Group 3: option_takes_value ---

#[test]
fn test_option_takes_value_known() {
    let parser = CommandLineParser::new();
    assert!(parser.option_takes_value("-m"));
    assert!(parser.option_takes_value("--message"));
    assert!(parser.option_takes_value("-f"));
    assert!(parser.option_takes_value("--file"));
    assert!(parser.option_takes_value("-n"));
    assert!(parser.option_takes_value("--namespace"));
    assert!(parser.option_takes_value("--context"));
}

#[test]
fn test_option_takes_value_unknown() {
    let parser = CommandLineParser::new();
    assert!(!parser.option_takes_value("-v"));
    assert!(!parser.option_takes_value("--verbose"));
    assert!(!parser.option_takes_value("--help"));
}

#[test]
fn test_option_takes_value_edge_cases() {
    let parser = CommandLineParser::new();
    assert!(!parser.option_takes_value("--"));
    assert!(!parser.option_takes_value("-"));
}

// --- Group 4: is_redirect_operator ---

#[test]
fn test_redirect_basic_operators() {
    assert!(CommandLineParser::is_redirect_operator(">"));
    assert!(CommandLineParser::is_redirect_operator(">>"));
    assert!(CommandLineParser::is_redirect_operator("<"));
    assert!(CommandLineParser::is_redirect_operator("&>"));
    assert!(CommandLineParser::is_redirect_operator("&>>"));
}

#[test]
fn test_redirect_numbered_fd() {
    assert!(CommandLineParser::is_redirect_operator("2>"));
    assert!(CommandLineParser::is_redirect_operator("1>"));
    assert!(CommandLineParser::is_redirect_operator("2>>"));
}

#[test]
fn test_redirect_non_operators() {
    assert!(!CommandLineParser::is_redirect_operator("cat"));
    assert!(!CommandLineParser::is_redirect_operator("->"));
    assert!(!CommandLineParser::is_redirect_operator("="));
    assert!(!CommandLineParser::is_redirect_operator(">>>")); // Not supported generally
}

// --- Group 5: find_cursor_token_index Edge Cases ---

#[test]
fn test_cursor_at_very_start() {
    let parser = CommandLineParser::new();
    let spans = parser.tokenize_with_positions("git add");
    // cursor at 0
    let (idx, inside) = parser.find_cursor_token_index(&spans, 0);
    // Using "git add", spans[0] is "git" (0..3).
    // cursor 0 is <= end(3) and >= start(0). So inside=true.
    assert_eq!(idx, 0);
    assert!(inside);
}

#[test]
fn test_cursor_at_end_of_input() {
    let parser = CommandLineParser::new();
    let input = "git add";
    let spans = parser.tokenize_with_positions(input);
    // cursor at 7 (len).
    // spans[1] is "add" (4..7).
    // cursor 7 is <= end(7) and >= start(4). So inside=true.
    let (idx, inside) = parser.find_cursor_token_index(&spans, 7);
    assert_eq!(idx, 1);
    assert!(inside);
}

#[test]
fn test_cursor_between_multiple_spaces() {
    let parser = CommandLineParser::new();
    let input = "a    b";
    let spans = parser.tokenize_with_positions(input);
    // "a" is 0..1. "b" is 5..6.
    // cursor at 3 (middle of spaces).
    // Not inside "a" (<=1). Not inside "b" (>=5).
    // Should return index 1 (before "b"), inside=false.
    let (idx, inside) = parser.find_cursor_token_index(&spans, 3);
    assert_eq!(idx, 1);
    assert!(!inside);
}

// --- Group 6: parse Integration Edge Cases ---

#[test]
fn test_parse_empty_input() {
    let parser = CommandLineParser::new();
    let result = parser.parse("", 0);
    assert_eq!(result.command, "");
    assert_eq!(result.completion_context, CompletionContext::Command);
}

#[test]
fn test_parse_whitespace_only_input() {
    let parser = CommandLineParser::new();
    let result = parser.parse("   ", 3);
    assert_eq!(result.command, "");
    assert_eq!(result.completion_context, CompletionContext::Command);
}

#[test]
fn test_parse_option_after_argument() {
    let parser = CommandLineParser::new();
    // "git add file --force"
    // command: git, subcommand: add, args: [file], options: [--force]
    let result = parser.parse("git add file --force", 20);
    assert_eq!(result.command, "git");
    assert_eq!(result.subcommand_path, vec!["add"]);
    assert_eq!(result.specified_arguments, vec!["file"]);
    assert_eq!(result.specified_options, vec!["--force"]);
    assert_eq!(result.completion_context, CompletionContext::LongOption);
}

#[test]
fn test_parse_multiple_redirects() {
    let parser = CommandLineParser::new();
    let input = "cmd < in > out 2>> err";
    let result = parser.parse(input, input.len());
    // redirects and their targets should NOT be in arguments
    // < in, > out, 2>> err
    // "in", "out", "err" are redirect targets.
    // current logic in `analyze_tokens`:
    // It iterates. If is_redirect_operator, set skip_next.
    // So "in", "out", "err" should be skipped.
    assert!(result.specified_arguments.is_empty());
    assert!(result.specified_options.is_empty());
}

#[test]
fn test_parse_option_value_chain() {
    let parser = CommandLineParser::new();
    // "git commit -m msg --file f.txt arg1"
    // -m takes value -> msg skipped
    // --file takes value -> f.txt skipped
    // arg1 -> kept
    let result = parser.parse("git commit -m msg --file f.txt arg1", 35);
    assert_eq!(result.specified_arguments, vec!["arg1"]);
    assert!(result.specified_options.contains(&"-m".to_string()));
    assert!(result.specified_options.contains(&"--file".to_string()));
}
