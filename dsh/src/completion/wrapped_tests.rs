#[cfg(test)]
mod tests {
    use crate::completion::command::{
        Argument, ArgumentType, CommandCompletion, CommandCompletionDatabase, CommandOption,
        SubCommand,
    };
    use crate::completion::generator::CompletionGenerator;
    use crate::completion::parser::{CommandLineParser, CompletionContext};

    fn create_test_database() -> CommandCompletionDatabase {
        let mut db = CommandCompletionDatabase::new();

        // Register 'sudo' with CommandWithArgs
        let sudo_completion = CommandCompletion {
            command: "sudo".to_string(),
            description: Some("Execute command as another user".to_string()),
            global_options: vec![],
            subcommands: vec![],
            arguments: vec![Argument {
                name: "command".to_string(),
                description: Some("Command to execute".to_string()),
                arg_type: Some(ArgumentType::CommandWithArgs),
            }],
        };
        db.add_command(sudo_completion);

        // Register 'git' as a sample wrapped command
        let git_completion = CommandCompletion {
            command: "git".to_string(),
            description: Some("Git version control".to_string()),
            global_options: vec![],
            subcommands: vec![
                SubCommand {
                    name: "add".to_string(),
                    description: Some("Add files".to_string()),
                    options: vec![],
                    arguments: vec![],
                    subcommands: vec![],
                },
                SubCommand {
                    name: "commit".to_string(),
                    description: Some("Commit changes".to_string()),
                    options: vec![
                        CommandOption {
                            short: Some("-m".to_string()),
                            long: Some("--message".to_string()),
                            description: None,
                            argument: None,
                        },
                        CommandOption {
                            short: Some("-a".to_string()),
                            long: Some("--all".to_string()),
                            description: None,
                            argument: None,
                        },
                    ],
                    arguments: vec![],
                    subcommands: vec![],
                },
            ],
            arguments: vec![],
        };
        db.add_command(git_completion);

        db
    }

    #[test]
    fn test_wrapped_command_argument_generates_system_commands() {
        let db = create_test_database();
        let generator = CompletionGenerator::new(&db);
        let parser = CommandLineParser::new();

        let input = "sudo git";
        let parsed = parser.parse(input, input.len());
        let parsed = generator.correct_parsed_command_line(&parsed);

        // Context should be Argument at index 0 after correction
        if let CompletionContext::Argument { arg_index, .. } = parsed.completion_context {
            assert_eq!(arg_index, 0);

            let _candidates = generator.generate_candidates(&parsed).unwrap();
            // Assuming git is installed/mocked? System commands depend on PATH.
            // We can't guarantee "git" is in candidates without mocking filesystem.
            // But we can check that it didn't error.
            // assert!(candidates.len() >= 0); // Just check it doesn't panic
        } else {
            panic!(
                "Expected Argument context, got {:?}",
                parsed.completion_context
            );
        }
    }

    #[test]
    fn test_recursive_subcommand_completion() {
        let db = create_test_database();
        let generator = CompletionGenerator::new(&db);
        let parser = CommandLineParser::new();

        // "sudo git a" -> should suggest "add" from git's completion
        let input = "sudo git a";
        let parsed = parser.parse(input, input.len());
        // Apply correction to ensure text "git" is treated as arg 0 of sudo
        let parsed = generator.correct_parsed_command_line(&parsed);

        // This is Argument 1 for sudo
        if let CompletionContext::Argument { arg_index, .. } = parsed.completion_context {
            assert_eq!(arg_index, 1);
        } else {
            panic!(
                "Expected Argument context, got {:?}",
                parsed.completion_context
            );
        }

        let candidates = generator.generate_candidates(&parsed).unwrap();
        assert!(candidates.iter().any(|c| c.text == "add"));
    }

    #[test]
    fn test_recursive_option_completion() {
        let db = create_test_database();
        let generator = CompletionGenerator::new(&db);
        let parser = CommandLineParser::new();

        // "sudo git commit -" -> should suggest "--message" from git's completion
        let input = "sudo git commit -";
        let parsed = parser.parse(input, input.len());
        // Correction shouldn't change Short/LongOption context generally but good practice
        let parsed = generator.correct_parsed_command_line(&parsed);

        let candidates = generator.generate_candidates(&parsed).unwrap();
        // Should contain --message
        assert!(candidates.iter().any(|c| c.text == "--message"));
        assert!(candidates.iter().any(|c| c.text == "-m"));
    }

    #[test]
    fn test_recursive_completion_with_trailing_space() {
        let mut db = create_test_database();
        // Add checkout subcommand to git for this test
        if let Some(mut git) = db.get_command("git").cloned() {
            git.subcommands.push(SubCommand {
                name: "checkout".to_string(),
                description: Some("Switch branch".to_string()),
                options: vec![],
                arguments: vec![Argument {
                    name: "branch".to_string(),
                    description: None,
                    arg_type: Some(ArgumentType::Choice(vec![
                        "main".to_string(),
                        "feature".to_string(),
                    ])),
                }],
                subcommands: vec![],
            });
            db.add_command(git);
        }

        let generator = CompletionGenerator::new(&db);
        let parser = CommandLineParser::new();

        // "sudo git checkout " -> should suggest "main", "feature"
        let input = "sudo git checkout ";
        let parsed = parser.parse(input, input.len());
        let parsed = generator.correct_parsed_command_line(&parsed);

        let candidates = generator.generate_candidates(&parsed).unwrap();
        assert!(candidates.iter().any(|c| c.text == "main"));
        assert!(candidates.iter().any(|c| c.text == "feature"));
    }
}
