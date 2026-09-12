use super::*;
use crate::completion::command::{ArgumentType, CommandCompletion, CommandOption, SubCommand};

#[test]
fn test_correct_flag_like_subcommand() {
    let mut db = CommandCompletionDatabase::new();
    let completion = CommandCompletion {
        command: "pacman".to_string(),
        description: None,
        global_options: vec![],
        subcommands: vec![SubCommand {
            name: "-S".to_string(),
            description: None,
            aliases: vec![],
            options: vec![],
            arguments: vec![],
            subcommands: vec![],
        }],
        arguments: vec![],
    };
    db.add_command(completion);

    let corrector = ContextCorrector::new(&db);

    // Case: "pacman -S" where parser initially thought -S was an option
    let parsed = ParsedCommandLine {
        command: "pacman".to_string(),
        subcommand_path: vec![],
        // Raw args might just be ["-S"] if "pacman" was consumed as command
        raw_args: vec!["-S".to_string()],
        args: vec![],
        options: vec![],
        current_token: "-S".to_string(),
        current_arg: None,
        completion_context: CompletionContext::ShortOption,
        specified_options: vec!["-S".to_string()],
        specified_arguments: vec![],
        cursor_index: 0,
    };

    let corrected = corrector.correct_parsed_command_line(&parsed);

    // This assertion is expected to FAIL before the fix
    assert!(
        matches!(corrected.completion_context, CompletionContext::SubCommand),
        "Expected SubCommand context, got {:?}",
        corrected.completion_context
    );
}

#[test]
fn test_correct_alias_subcommand() {
    let mut db = CommandCompletionDatabase::new();
    let completion = CommandCompletion {
        command: "pacman".to_string(),
        description: None,
        global_options: vec![],
        subcommands: vec![SubCommand {
            name: "-S".to_string(),
            description: None,
            aliases: vec!["--sync".to_string()],
            options: vec![],
            arguments: vec![],
            subcommands: vec![],
        }],
        arguments: vec![],
    };
    db.add_command(completion);

    let corrector = ContextCorrector::new(&db);

    // Case: "pacman --sync"
    let parsed = ParsedCommandLine {
        command: "pacman".to_string(),
        subcommand_path: vec![],
        raw_args: vec!["--sync".to_string()],
        args: vec![],
        options: vec![],
        current_token: "--sync".to_string(), // Current token is the alias
        current_arg: None,
        completion_context: CompletionContext::LongOption, // Parser thinks it's a long option
        specified_options: vec!["--sync".to_string()],
        specified_arguments: vec![],
        cursor_index: 0,
    };

    let corrected = corrector.correct_parsed_command_line(&parsed);

    // Should be corrected to SubCommand context because --sync is alias of -S
    assert!(
        matches!(corrected.completion_context, CompletionContext::SubCommand),
        "Expected SubCommand context for alias, got {:?}",
        corrected.completion_context
    );
}

#[test]
fn alias_subcommand_is_canonicalized_before_argument_completion() {
    let mut db = CommandCompletionDatabase::new();
    db.add_command(CommandCompletion {
        command: "pacman".to_string(),
        description: None,
        global_options: vec![],
        subcommands: vec![SubCommand {
            name: "-S".to_string(),
            description: None,
            aliases: vec!["--sync".to_string()],
            options: vec![],
            arguments: vec![],
            subcommands: vec![],
        }],
        arguments: vec![],
    });

    let parsed = CommandLineParser::new().parse("pacman --sync ", "pacman --sync ".len());
    let corrected = ContextCorrector::new(&db).correct_parsed_command_line(&parsed);

    assert_eq!(corrected.subcommand_path, vec!["-S".to_string()]);
    assert!(matches!(
        corrected.completion_context,
        CompletionContext::Argument { arg_index: 0, .. }
    ));
}

#[test]
fn option_value_context_uses_completion_definition() {
    let mut db = CommandCompletionDatabase::new();
    db.add_command(CommandCompletion {
        command: "cargo".to_string(),
        description: None,
        global_options: vec![],
        subcommands: vec![SubCommand {
            name: "test".to_string(),
            description: None,
            aliases: vec![],
            options: vec![CommandOption {
                short: Some("-p".to_string()),
                long: Some("--package".to_string()),
                description: None,
                takes_value: true,
                value_type: Some(ArgumentType::Choice(vec!["doge-shell".to_string()])),
                argument: None,
            }],
            arguments: vec![],
            subcommands: vec![],
        }],
        arguments: vec![],
    });

    let parsed = CommandLineParser::new().parse("cargo test -p do", "cargo test -p do".len());
    let corrected = ContextCorrector::new(&db).correct_parsed_command_line(&parsed);

    assert!(matches!(
        corrected.completion_context,
        CompletionContext::OptionValue {
            option_name,
            value_type: Some(ArgumentType::Choice(_))
        } if option_name == "-p"
    ));
    assert!(corrected.specified_arguments.is_empty());
    assert!(corrected.args.is_empty());
}

#[test]
fn global_option_before_subcommand_preserves_argument_context() {
    let mut db = CommandCompletionDatabase::new();
    db.add_command(CommandCompletion {
        command: "snapper".to_string(),
        description: None,
        global_options: vec![CommandOption {
            short: Some("-c".to_string()),
            long: Some("--config".to_string()),
            description: None,
            takes_value: true,
            value_type: Some(ArgumentType::Choice(vec!["root".to_string()])),
            argument: None,
        }],
        subcommands: vec![SubCommand {
            name: "delete".to_string(),
            description: None,
            aliases: vec![],
            options: vec![],
            arguments: vec![crate::completion::command::Argument {
                name: "snapshot".to_string(),
                description: None,
                multiple: true,
                arg_type: Some(ArgumentType::Dynamic {
                    provider: "snapper.snapshot".to_string(),
                    scope: None,
                }),
            }],
            subcommands: vec![],
        }],
        arguments: vec![],
    });

    let input = "snapper --config root delete 4";
    let parsed = CommandLineParser::new().parse(input, input.len());
    let corrected = ContextCorrector::new(&db).correct_parsed_command_line(&parsed);

    assert_eq!(corrected.subcommand_path, vec!["delete".to_string()]);
    assert!(matches!(
        corrected.completion_context,
        CompletionContext::Argument { arg_index: 0, .. }
    ));
}

#[test]
fn recovered_subcommand_counts_only_preceding_positional_arguments() {
    let mut db = CommandCompletionDatabase::new();
    db.add_command(CommandCompletion {
        command: "helm".to_string(),
        description: None,
        global_options: vec![CommandOption {
            short: None,
            long: Some("--kube-context".to_string()),
            description: None,
            takes_value: true,
            value_type: Some(ArgumentType::String),
            argument: None,
        }],
        subcommands: vec![SubCommand {
            name: "install".to_string(),
            description: None,
            aliases: vec![],
            options: vec![],
            arguments: vec![
                crate::completion::command::Argument {
                    name: "release".to_string(),
                    description: None,
                    multiple: false,
                    arg_type: Some(ArgumentType::String),
                },
                crate::completion::command::Argument {
                    name: "chart".to_string(),
                    description: None,
                    multiple: false,
                    arg_type: Some(ArgumentType::File { extensions: None }),
                },
            ],
            subcommands: vec![],
        }],
        arguments: vec![],
    });

    let input = "helm --kube-context dev install release ./cha";
    let parsed = CommandLineParser::new().parse(input, input.len());
    let corrected = ContextCorrector::new(&db).correct_parsed_command_line(&parsed);

    assert_eq!(corrected.subcommand_path, vec!["install".to_string()]);
    assert_eq!(
        corrected.specified_arguments,
        vec!["release".to_string(), "./cha".to_string()]
    );
    assert!(matches!(
        corrected.completion_context,
        CompletionContext::Argument { arg_index: 1, .. }
    ));
}

#[test]
fn known_option_value_is_removed_before_positional_argument_index() {
    let mut db = CommandCompletionDatabase::new();
    db.add_command(CommandCompletion {
        command: "pytest".to_string(),
        description: None,
        global_options: vec![CommandOption {
            short: Some("-k".to_string()),
            long: None,
            description: None,
            takes_value: true,
            value_type: Some(ArgumentType::String),
            argument: None,
        }],
        subcommands: vec![],
        arguments: vec![crate::completion::command::Argument {
            name: "path".to_string(),
            description: None,
            multiple: true,
            arg_type: None,
        }],
    });

    let parsed =
        CommandLineParser::new().parse("pytest -k expr tests", "pytest -k expr tests".len());
    let corrected = ContextCorrector::new(&db).correct_parsed_command_line(&parsed);

    assert_eq!(corrected.specified_arguments, vec!["tests".to_string()]);
    assert_eq!(corrected.args, vec!["tests".to_string()]);
    assert!(matches!(
        corrected.completion_context,
        CompletionContext::Argument { arg_index: 0, .. }
    ));
}

#[test]
fn known_option_value_removal_preserves_same_text_positional_argument() {
    let mut db = CommandCompletionDatabase::new();
    db.add_command(CommandCompletion {
        command: "cmd".to_string(),
        description: None,
        global_options: vec![CommandOption {
            short: Some("-k".to_string()),
            long: None,
            description: None,
            takes_value: true,
            value_type: Some(ArgumentType::String),
            argument: None,
        }],
        subcommands: vec![],
        arguments: vec![crate::completion::command::Argument {
            name: "path".to_string(),
            description: None,
            multiple: true,
            arg_type: None,
        }],
    });

    let parsed = CommandLineParser::new().parse("cmd foo -k foo bar", "cmd foo -k foo bar".len());
    let corrected = ContextCorrector::new(&db).correct_parsed_command_line(&parsed);

    assert_eq!(
        corrected.specified_arguments,
        vec!["foo".to_string(), "bar".to_string()]
    );
    assert!(matches!(
        corrected.completion_context,
        CompletionContext::Argument { arg_index: 1, .. }
    ));
}

#[test]
fn known_option_value_is_removed_before_empty_positional_argument() {
    let mut db = CommandCompletionDatabase::new();
    db.add_command(CommandCompletion {
        command: "pytest".to_string(),
        description: None,
        global_options: vec![CommandOption {
            short: Some("-k".to_string()),
            long: None,
            description: None,
            takes_value: true,
            value_type: Some(ArgumentType::String),
            argument: None,
        }],
        subcommands: vec![],
        arguments: vec![crate::completion::command::Argument {
            name: "path".to_string(),
            description: None,
            multiple: true,
            arg_type: None,
        }],
    });

    let parsed = CommandLineParser::new().parse("pytest -k expr ", "pytest -k expr ".len());
    let corrected = ContextCorrector::new(&db).correct_parsed_command_line(&parsed);

    assert_eq!(corrected.specified_arguments, vec!["".to_string()]);
    assert!(matches!(
        corrected.completion_context,
        CompletionContext::Argument { arg_index: 0, .. }
    ));
}

#[test]
fn inline_long_option_value_uses_completion_definition() {
    let mut db = CommandCompletionDatabase::new();
    db.add_command(CommandCompletion {
        command: "kubectl".to_string(),
        description: None,
        global_options: vec![CommandOption {
            short: None,
            long: Some("--context".to_string()),
            description: None,
            takes_value: true,
            value_type: Some(ArgumentType::Choice(vec!["dev-cluster".to_string()])),
            argument: None,
        }],
        subcommands: vec![],
        arguments: vec![],
    });

    let parsed =
        CommandLineParser::new().parse("kubectl --context=de", "kubectl --context=de".len());
    let corrected = ContextCorrector::new(&db).correct_parsed_command_line(&parsed);

    assert_eq!(corrected.current_token, "de");
    assert!(matches!(
        corrected.completion_context,
        CompletionContext::OptionValue {
            option_name,
            value_type: Some(ArgumentType::Choice(_))
        } if option_name == "--context"
    ));
    assert_eq!(corrected.raw_args, vec!["--context=de".to_string()]);
    assert_eq!(corrected.specified_options, vec!["--context".to_string()]);
}

#[test]
fn short_attached_option_value_uses_completion_definition() {
    let mut db = CommandCompletionDatabase::new();
    db.add_command(CommandCompletion {
        command: "kubectl".to_string(),
        description: None,
        global_options: vec![CommandOption {
            short: Some("-n".to_string()),
            long: Some("--namespace".to_string()),
            description: None,
            takes_value: true,
            value_type: Some(ArgumentType::Choice(vec!["dev-namespace".to_string()])),
            argument: None,
        }],
        subcommands: vec![],
        arguments: vec![],
    });

    let parsed = CommandLineParser::new().parse("kubectl -nde", "kubectl -nde".len());
    let corrected = ContextCorrector::new(&db).correct_parsed_command_line(&parsed);

    assert_eq!(corrected.current_token, "de");
    assert!(matches!(
        corrected.completion_context,
        CompletionContext::OptionValue {
            option_name,
            value_type: Some(ArgumentType::Choice(_))
        } if option_name == "-n"
    ));
    assert_eq!(corrected.raw_args, vec!["-nde".to_string()]);
    assert_eq!(corrected.specified_options, vec!["-n".to_string()]);
    assert_eq!(corrected.options, vec!["-n".to_string()]);
}

#[test]
fn short_attached_option_without_value_definition_does_not_use_value_provider() {
    let mut db = CommandCompletionDatabase::new();
    db.add_command(CommandCompletion {
        command: "cmd".to_string(),
        description: None,
        global_options: vec![CommandOption {
            short: Some("-v".to_string()),
            long: Some("--verbose".to_string()),
            description: None,
            takes_value: false,
            value_type: None,
            argument: None,
        }],
        subcommands: vec![],
        arguments: vec![],
    });

    let parsed = CommandLineParser::new().parse("cmd -vfoo", "cmd -vfoo".len());
    let corrected = ContextCorrector::new(&db).correct_parsed_command_line(&parsed);

    assert_eq!(corrected.current_token, "-vfoo");
    assert_eq!(corrected.completion_context, CompletionContext::LongOption);
    assert_eq!(corrected.specified_options, vec!["-vfoo".to_string()]);
}

#[test]
fn short_equals_option_value_stays_out_of_scope() {
    let mut db = CommandCompletionDatabase::new();
    db.add_command(CommandCompletion {
        command: "cmd".to_string(),
        description: None,
        global_options: vec![CommandOption {
            short: Some("-x".to_string()),
            long: Some("--example".to_string()),
            description: None,
            takes_value: true,
            value_type: Some(ArgumentType::Choice(vec!["value".to_string()])),
            argument: None,
        }],
        subcommands: vec![],
        arguments: vec![],
    });

    let parsed = CommandLineParser::new().parse("cmd -x=y", "cmd -x=y".len());
    let corrected = ContextCorrector::new(&db).correct_parsed_command_line(&parsed);

    assert_eq!(corrected.current_token, "-x=y");
    assert_eq!(corrected.completion_context, CompletionContext::LongOption);
    assert_eq!(corrected.specified_options, vec!["-x=y".to_string()]);
}

#[test]
fn separate_option_empty_value_uses_completion_definition() {
    let mut db = CommandCompletionDatabase::new();
    db.add_command(CommandCompletion {
        command: "cargo".to_string(),
        description: None,
        global_options: vec![],
        subcommands: vec![SubCommand {
            name: "test".to_string(),
            description: None,
            aliases: vec![],
            options: vec![CommandOption {
                short: Some("-p".to_string()),
                long: Some("--package".to_string()),
                description: None,
                takes_value: true,
                value_type: Some(ArgumentType::Choice(vec!["doge-shell".to_string()])),
                argument: None,
            }],
            arguments: vec![],
            subcommands: vec![],
        }],
        arguments: vec![],
    });

    let parsed = CommandLineParser::new().parse("cargo test -p ", "cargo test -p ".len());
    let corrected = ContextCorrector::new(&db).correct_parsed_command_line(&parsed);

    assert_eq!(corrected.current_token, "");
    assert!(matches!(
        corrected.completion_context,
        CompletionContext::OptionValue {
            option_name,
            value_type: Some(ArgumentType::Choice(_))
        } if option_name == "-p"
    ));
}

#[test]
fn inline_long_option_without_value_definition_does_not_use_value_provider() {
    let mut db = CommandCompletionDatabase::new();
    db.add_command(CommandCompletion {
        command: "cmd".to_string(),
        description: None,
        global_options: vec![CommandOption {
            short: None,
            long: Some("--verbose".to_string()),
            description: None,
            takes_value: false,
            value_type: None,
            argument: None,
        }],
        subcommands: vec![],
        arguments: vec![],
    });

    let parsed = CommandLineParser::new().parse("cmd --verbose=x", "cmd --verbose=x".len());
    let corrected = ContextCorrector::new(&db).correct_parsed_command_line(&parsed);

    assert_eq!(corrected.current_token, "--verbose=x");
    assert_eq!(corrected.completion_context, CompletionContext::LongOption);
}
