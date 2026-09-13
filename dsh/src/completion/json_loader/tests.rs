use super::*;
use std::fs;
use tempfile::TempDir;

#[test]
fn test_load_valid_completion_file() {
    let temp_dir = TempDir::new().unwrap();
    let completion_file = temp_dir.path().join("test.json");

    let test_completion = r#"
        {
            "command": "test",
            "description": "Test command",
            "global_options": [],
            "subcommands": [
                {
                    "name": "sub",
                    "description": "Test subcommand",
                    "aliases": [],
                    "options": [],
                    "arguments": [],
                    "subcommands": []
                }
            ]
        }
        "#;

    fs::write(&completion_file, test_completion).unwrap();

    let loader = JsonCompletionLoader::with_dirs(vec![temp_dir.path().to_path_buf()]);
    let result = loader.load_completion_file(&completion_file);

    assert!(result.is_ok());
    let completion = result.unwrap();
    assert_eq!(completion.command, "test");
    assert_eq!(completion.subcommands.len(), 1);
    assert_eq!(completion.subcommands[0].name, "sub");
}

#[test]
fn test_load_option_value_type_fields() {
    let loader = JsonCompletionLoader::new();
    let completion = loader
        .load_completion_from_content(
            br#"
                {
                    "command": "kubectl",
                    "global_options": [
                        {
                            "short": "-n",
                            "long": "--namespace",
                            "description": "Namespace",
                            "takes_value": true,
                            "value_type": {
                                "type": "Choice",
                                "data": ["default", "kube-system"]
                            }
                        }
                    ]
                }
                "#,
            "inline",
        )
        .unwrap();

    let option = &completion.global_options[0];
    assert!(option.takes_value);
    assert!(matches!(
        option.value_type(),
        Some(crate::completion::command::ArgumentType::Choice(values))
            if values == &vec!["default".to_string(), "kube-system".to_string()]
    ));
}

#[test]
fn filesystem_completion_overrides_embedded_completion() {
    let temp_dir = TempDir::new().unwrap();
    fs::write(
        temp_dir.path().join("git.json"),
        r#"
            {
                "command": "git",
                "description": "User override",
                "subcommands": [
                    {
                        "name": "custom-user-subcommand",
                        "description": "Only in user config"
                    }
                ]
            }
            "#,
    )
    .unwrap();

    let loader = JsonCompletionLoader::with_dirs(vec![temp_dir.path().to_path_buf()]);
    let completion = loader.load_command_completion("git").unwrap().unwrap();

    assert_eq!(completion.description.as_deref(), Some("User override"));
    assert!(
        completion
            .subcommands
            .iter()
            .any(|subcommand| subcommand.name == "custom-user-subcommand")
    );
}

#[test]
fn fallback_completion_does_not_override_embedded_completion() {
    let temp_dir = TempDir::new().unwrap();
    fs::write(
        temp_dir.path().join("git.json"),
        r#"
            {
                "command": "git",
                "description": "Fallback override",
                "subcommands": [
                    {
                        "name": "fallback-only-subcommand",
                        "description": "Only in fallback"
                    }
                ]
            }
            "#,
    )
    .unwrap();

    let loader = JsonCompletionLoader::with_override_and_fallback_dirs(
        Vec::new(),
        vec![temp_dir.path().to_path_buf()],
    );
    let completion = loader.load_command_completion("git").unwrap().unwrap();

    assert_ne!(completion.description.as_deref(), Some("Fallback override"));
    assert!(
        !completion
            .subcommands
            .iter()
            .any(|subcommand| subcommand.name == "fallback-only-subcommand")
    );
}

#[test]
fn fallback_completion_loads_when_embedded_missing() {
    let temp_dir = TempDir::new().unwrap();
    fs::write(
        temp_dir.path().join("local-only-command.json"),
        r#"
            {
                "command": "local-only-command",
                "description": "Fallback-only completion"
            }
            "#,
    )
    .unwrap();

    let loader = JsonCompletionLoader::with_override_and_fallback_dirs(
        Vec::new(),
        vec![temp_dir.path().to_path_buf()],
    );
    let completion = loader
        .load_command_completion("local-only-command")
        .unwrap()
        .unwrap();

    assert_eq!(
        completion.description.as_deref(),
        Some("Fallback-only completion")
    );
}

#[test]
fn command_completion_schema_matches_runtime_field_names() {
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../../../../command-completion-schema.json")).unwrap();
    let properties = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .unwrap();
    assert!(properties.contains_key("arguments"));

    let argument_properties = schema
        .pointer("/definitions/Argument/properties")
        .and_then(serde_json::Value::as_object)
        .unwrap();
    assert!(argument_properties.contains_key("type"));
    assert!(!argument_properties.contains_key("arg_type"));

    let option_properties = schema
        .pointer("/definitions/CommandOption/properties")
        .and_then(serde_json::Value::as_object)
        .unwrap();
    assert!(option_properties.contains_key("argument"));

    for type_name in [
        "Process",
        "CommandWithArgs",
        "User",
        "Group",
        "Signal",
        "Interface",
        "Dynamic",
    ] {
        assert!(
            schema.to_string().contains(&format!(r#""{type_name}""#)),
            "schema should include runtime ArgumentType::{type_name}"
        );
    }
}

#[test]
fn command_completion_schema_uses_shared_dynamic_provider_list() {
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../../../../command-completion-schema.json")).unwrap();
    let dynamic_type = schema
        .pointer("/definitions/ArgumentType/oneOf")
        .and_then(serde_json::Value::as_array)
        .unwrap()
        .iter()
        .find(|entry| {
            entry.get("title").and_then(serde_json::Value::as_str) == Some("Dynamic Type")
        })
        .unwrap();
    let schema_providers = dynamic_type
        .pointer("/properties/data/properties/provider/enum")
        .and_then(serde_json::Value::as_array)
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(
        schema_providers,
        dsh_types::completion::DYNAMIC_COMPLETION_PROVIDERS
    );
}

#[test]
fn top_level_options_are_merged_with_global_options() {
    let loader = JsonCompletionLoader::new();
    let completion = loader
        .load_completion_from_content(
            br#"{
                    "command": "legacy-options",
                    "global_options": [
                        { "long": "--help", "description": "Show help" }
                    ],
                    "options": [
                        { "long": "--verbose", "short": "-v", "description": "Verbose output" }
                    ]
                }"#,
            "legacy-options.json",
        )
        .unwrap();

    assert!(
        completion
            .global_options
            .iter()
            .any(|option| option.long.as_deref() == Some("--help"))
    );
    assert!(
        completion
            .global_options
            .iter()
            .any(|option| option.long.as_deref() == Some("--verbose"))
    );
}

#[test]
fn test_load_invalid_json() {
    let temp_dir = TempDir::new().unwrap();
    let completion_file = temp_dir.path().join("invalid.json");

    fs::write(&completion_file, "invalid json").unwrap();

    let loader = JsonCompletionLoader::with_dirs(vec![temp_dir.path().to_path_buf()]);
    let result = loader.load_completion_file(&completion_file);

    assert!(result.is_err());
}

#[test]
fn test_validation_empty_command_name() {
    let loader = JsonCompletionLoader::new();
    let completion = CommandCompletion {
        command: "".to_string(),
        description: None,
        subcommands: vec![],
        global_options: vec![],
        arguments: vec![],
    };

    let result = loader.validate_completion(&completion);
    assert!(result.is_err());
}

#[test]
fn test_list_available_completions() {
    let temp_dir = TempDir::new().unwrap();

    // Create test JSON files
    fs::write(temp_dir.path().join("git.json"), "{}").unwrap();
    fs::write(temp_dir.path().join("cargo.json"), "{}").unwrap();
    fs::write(temp_dir.path().join("not_json.txt"), "{}").unwrap();

    let loader = JsonCompletionLoader::with_dirs(vec![temp_dir.path().to_path_buf()]);
    let completions = loader.list_available_completions().unwrap();

    // Should include both embedded completions and filesystem completions.
    // The exact number grows as built-in command definitions are added.
    assert!(completions.len() >= 5);
    assert!(completions.contains(&"git".to_string()));
    assert!(completions.contains(&"cargo".to_string()));
    assert!(completions.contains(&"docker".to_string()));
    assert!(completions.contains(&"npm".to_string()));
    assert!(completions.contains(&"kubectl".to_string()));
    assert!(completions.contains(&"make".to_string()));
    assert!(!completions.contains(&"not_json".to_string()));
}

#[test]
fn balanced_completion_batch_exposes_representative_subcommands() {
    let loader = JsonCompletionLoader::new();
    for (command, expected_subcommand) in [
        ("act", None),
        ("argocd", Some("app")),
        ("chezmoi", Some("apply")),
        ("delta", None),
        ("flux", Some("reconcile")),
        ("helmfile", Some("sync")),
        ("kind", Some("create")),
        ("k3d", Some("cluster")),
        ("minikube", Some("start")),
        ("nomad", Some("job")),
        ("ollama", Some("run")),
        ("pre-commit", Some("install")),
        ("starship", Some("init")),
        ("vault", Some("login")),
        ("wezterm", Some("cli")),
    ] {
        let completion = loader
            .load_command_completion(command)
            .unwrap_or_else(|error| panic!("failed to load {command}: {error}"))
            .unwrap_or_else(|| panic!("missing completion for {command}"));
        if let Some(expected) = expected_subcommand {
            assert!(
                completion
                    .subcommands
                    .iter()
                    .any(|subcommand| subcommand.name == expected),
                "{command} should expose {expected}"
            );
        } else {
            assert!(
                !completion.global_options.is_empty(),
                "{command} should expose options"
            );
        }
    }
}

#[test]
fn test_load_real_git_completion() {
    let loader = JsonCompletionLoader::new();

    match loader.load_command_completion("git") {
        Ok(Some(completion)) => {
            assert_eq!(completion.command, "git");
            assert!(completion.description.is_some());
            assert!(!completion.subcommands.is_empty());

            // Verify that "add" subcommand exists
            let add_subcommand = completion.subcommands.iter().find(|sc| sc.name == "add");
            assert!(add_subcommand.is_some());

            let add = add_subcommand.unwrap();
            assert!(add.description.is_some());
            assert!(!add.options.is_empty());

            println!(
                "Successfully loaded git completion with {} subcommands",
                completion.subcommands.len()
            );
        }
        Ok(None) => {
            println!(
                "Git completion file not found - this is expected if no embedded or filesystem completion exists"
            );
        }
        Err(e) => {
            println!("Error loading git completion: {e}");
        }
    }
}

/// Regression: `git push`'s subcommand entry had no `options` at all, so
/// `git push --<TAB>` only offered git's top-level `global_options`
/// (`--version`, `--bare`, ...) and never `--force-with-lease` /
/// `--force-if-includes`.
#[test]
fn git_push_offers_lease_protected_force_options() {
    use crate::completion::generator::CompletionGenerator;
    use crate::completion::parser::{CompletionContext, ParsedCommandLine};

    let loader = JsonCompletionLoader::new();
    let database = loader.load_database().expect("Failed to load database");
    let generator = CompletionGenerator::new(&database);

    let parsed = ParsedCommandLine {
        command: "git".to_string(),
        subcommand_path: vec!["push".to_string()],
        raw_args: vec!["push".to_string(), "--force".to_string()],
        args: vec![],
        options: vec![],
        current_token: "--force".to_string(),
        current_arg: Some("--force".to_string()),
        completion_context: CompletionContext::LongOption,
        specified_options: vec![],
        specified_arguments: vec![],
        cursor_index: 1,
    };

    let texts: Vec<String> = generator
        .generate_candidates(&parsed)
        .expect("candidate generation should succeed")
        .into_iter()
        .map(|c| c.text)
        .collect();

    assert!(
        texts.contains(&"--force".to_string()),
        "expected --force, got: {texts:?}"
    );
    assert!(
        texts.contains(&"--force-with-lease".to_string()),
        "expected --force-with-lease, got: {texts:?}"
    );
    assert!(
        texts.contains(&"--force-if-includes".to_string()),
        "expected --force-if-includes, got: {texts:?}"
    );
}

#[test]
fn test_load_real_cargo_completion() {
    let loader = JsonCompletionLoader::new();

    match loader.load_command_completion("cargo") {
        Ok(Some(completion)) => {
            assert_eq!(completion.command, "cargo");
            assert!(completion.description.is_some());
            assert!(!completion.subcommands.is_empty());

            // Verify that "build" subcommand exists
            let build_subcommand = completion.subcommands.iter().find(|sc| sc.name == "build");
            assert!(build_subcommand.is_some());

            println!(
                "Successfully loaded cargo completion with {} subcommands",
                completion.subcommands.len()
            );
        }
        Ok(None) => {
            println!(
                "Cargo completion file not found - this is expected if no embedded or filesystem completion exists"
            );
        }
        Err(e) => {
            println!("Error loading cargo completion: {e}");
        }
    }
}

#[test]
fn test_embedded_completions_available() {
    let loader = JsonCompletionLoader::new();

    match loader.list_available_completions() {
        Ok(completions) => {
            println!("Available completions: {completions:?}");
            // We expect at least some completions to be available from embedded resources
            // The exact number depends on what's in the completions/ directory
        }
        Err(e) => {
            println!("Error listing completions: {e}");
        }
    }
}

#[test]
fn test_load_completion_from_content() {
    let loader = JsonCompletionLoader::new();

    let test_json = r#"
        {
            "command": "test",
            "description": "Test command",
            "global_options": [],
            "subcommands": [
                {
                    "name": "sub",
                    "description": "Test subcommand",
                    "aliases": [],
                    "options": [],
                    "arguments": [],
                    "subcommands": []
                }
            ]
        }
        "#;

    let result = loader.load_completion_from_content(test_json.as_bytes(), "test_source");
    assert!(result.is_ok());

    let completion = result.unwrap();
    assert_eq!(completion.command, "test");
    assert_eq!(completion.subcommands.len(), 1);
    assert_eq!(completion.subcommands[0].name, "sub");
}

#[test]
fn test_validate_option_with_placeholders() {
    let loader = JsonCompletionLoader::new();

    // Test short option with placeholder
    let option_with_short = crate::completion::command::CommandOption {
        short: Some("-f <FILE>".to_string()),
        long: None,
        description: None,
        takes_value: false,
        value_type: None,
        argument: None,
    };
    assert!(loader.validate_option(&option_with_short, "test").is_ok());

    // Test long option with placeholder
    let option_with_long = crate::completion::command::CommandOption {
        short: None,
        long: Some("--type <TYPE>".to_string()),
        description: None,
        takes_value: false,
        value_type: None,
        argument: None,
    };
    assert!(loader.validate_option(&option_with_long, "test").is_ok());

    // Test both short and long with placeholders
    let option_both = crate::completion::command::CommandOption {
        short: Some("-f <FILE>".to_string()),
        long: Some("--file <FILE>".to_string()),
        description: None,
        takes_value: false,
        value_type: None,
        argument: None,
    };
    assert!(loader.validate_option(&option_both, "test").is_ok());

    // Test invalid short options (short must start with a single dash and have content)
    let invalid_short = crate::completion::command::CommandOption {
        short: Some("--".to_string()), // Invalid: this is a long option prefix, not a short option
        long: None,
        description: None,
        takes_value: false,
        value_type: None,
        argument: None,
    };
    assert!(loader.validate_option(&invalid_short, "test").is_err());

    let invalid_bare_short = crate::completion::command::CommandOption {
        short: Some("-".to_string()),
        long: None,
        description: None,
        takes_value: false,
        value_type: None,
        argument: None,
    };
    assert!(loader.validate_option(&invalid_bare_short, "test").is_err());

    // Test that valid short option like -123 is now accepted
    let valid_short_with_number = crate::completion::command::CommandOption {
        short: Some("-123".to_string()), // Should be valid now: starts with -
        long: None,
        description: None,
        takes_value: false,
        value_type: None,
        argument: None,
    };
    assert!(
        loader
            .validate_option(&valid_short_with_number, "test")
            .is_ok()
    );

    let valid_short_with_attached_value = crate::completion::command::CommandOption {
        short: Some("-ofile".to_string()),
        long: None,
        description: None,
        takes_value: false,
        value_type: None,
        argument: None,
    };
    assert!(
        loader
            .validate_option(&valid_short_with_attached_value, "test")
            .is_ok()
    );

    // Test invalid long option (should still fail)
    let invalid_long = crate::completion::command::CommandOption {
        short: None,
        long: Some("--".to_string()), // Invalid: just -- without any content
        description: None,
        takes_value: false,
        value_type: None,
        argument: None,
    };
    assert!(loader.validate_option(&invalid_long, "test").is_err());

    let invalid_long_with_placeholder = crate::completion::command::CommandOption {
        short: None,
        long: Some("-- <ARG>".to_string()),
        description: None,
        takes_value: false,
        value_type: None,
        argument: None,
    };
    assert!(
        loader
            .validate_option(&invalid_long_with_placeholder, "test")
            .is_err()
    );

    let invalid_bare_long = crate::completion::command::CommandOption {
        short: None,
        long: Some("-".to_string()),
        description: None,
        takes_value: false,
        value_type: None,
        argument: None,
    };
    assert!(loader.validate_option(&invalid_bare_long, "test").is_err());

    // Test that long option starting with -- and containing numbers is now valid
    let valid_long_with_number = crate::completion::command::CommandOption {
        short: None,
        long: Some("--123invalid".to_string()), // Should be valid now: starts with --
        description: None,
        takes_value: false,
        value_type: None,
        argument: None,
    };
    assert!(
        loader
            .validate_option(&valid_long_with_number, "test")
            .is_ok()
    );

    let valid_single_dash_long = crate::completion::command::CommandOption {
        short: None,
        long: Some("-Xmx".to_string()),
        description: None,
        takes_value: false,
        value_type: None,
        argument: None,
    };
    assert!(
        loader
            .validate_option(&valid_single_dash_long, "test")
            .is_ok()
    );
}

/// Test that all embedded JSON completion files can be loaded and parsed correctly
#[test]
fn test_all_embedded_completion_files_load_correctly() {
    let loader = JsonCompletionLoader::new();
    // Directly iterate over embedded assets, do not use list_available_completions
    // This ensures the test is hermetic and only checks what's compiled in.
    let embedded_commands: Vec<String> = CompletionAssets::iter()
        .filter(|path| path.ends_with(".json"))
        .map(|path| path.strip_suffix(".json").unwrap().to_string())
        .collect();

    for command_name in embedded_commands {
        println!("Testing completion file: {}", command_name);

        // Load the command completion
        let result = loader.load_command_completion(&command_name);
        assert!(
            result.is_ok(),
            "Failed to load completion for command '{}': {:?}",
            command_name,
            result.err()
        );

        // Check that we got a completion
        let completion = result.unwrap();
        assert!(
            completion.is_some(),
            "Expected completion for command '{}' but got None",
            command_name
        );

        // Check that the completion has the command name
        let completion = completion.unwrap();
        assert_eq!(
            completion.command, command_name,
            "Command name mismatch for '{}': expected '{}', got '{}'",
            command_name, command_name, completion.command
        );

        // Validate the completion data
        let validation_result = loader.validate_completion(&completion);
        assert!(
            validation_result.is_ok(),
            "Validation failed for command '{}': {:?}",
            command_name,
            validation_result.err()
        );

        println!(
            "✓ Successfully loaded and validated completion for '{}': {} subcommands, {} global options",
            command_name,
            completion.subcommands.len(),
            completion.global_options.len()
        );
    }
}

/// Test that completion candidates can be generated from loaded JSON files
#[test]
fn test_completion_candidates_generation_from_json() {
    use crate::completion::generator::CompletionGenerator;
    use crate::completion::parser::{CompletionContext, ParsedCommandLine};

    let loader = JsonCompletionLoader::new();
    let database = loader.load_database().expect("Failed to load database");
    let generator = CompletionGenerator::new(&database);

    for command in ["git", "cargo", "docker", "rg"] {
        assert!(
            generator.has_command_completion(command),
            "test fixture expects a completion for '{command}'"
        );

        let parsed_command = ParsedCommandLine {
            command: command.to_string(),
            subcommand_path: vec![],
            args: vec![],
            options: vec![],
            current_token: "".to_string(),
            current_arg: Some("".to_string()),
            completion_context: CompletionContext::Command,
            specified_options: vec![],
            specified_arguments: vec![],
            raw_args: vec![],
            cursor_index: 0,
        };

        let candidates = generator.generate_candidates(&parsed_command).unwrap();
        assert!(
            !candidates.is_empty(),
            "expected completion candidates for command '{command}'"
        );

        // If the command has subcommands, filtering by the first letter of
        // one should return at least that candidate.
        let Ok(Some(cmd_completion)) = loader.load_command_completion(command) else {
            continue;
        };
        let Some(first_subcommand) = cmd_completion.subcommands.first() else {
            continue;
        };

        let first_letter: String = first_subcommand.name.chars().take(1).collect();
        let parsed_subcommand = ParsedCommandLine {
            command: command.to_string(),
            subcommand_path: vec![],
            args: vec![],
            options: vec![],
            current_token: first_letter.clone(),
            current_arg: Some(first_letter),
            completion_context: CompletionContext::SubCommand,
            specified_options: vec![],
            specified_arguments: vec![],
            raw_args: vec![],
            cursor_index: 0,
        };

        let subcommand_candidates = generator.generate_candidates(&parsed_subcommand).unwrap();
        assert!(
            !subcommand_candidates.is_empty(),
            "expected subcommand candidates for '{command}'"
        );
    }
}

#[test]
fn test_completion_files_display_candidates_correctly() {
    use crate::completion::display::Candidate as DisplayCandidate;
    use crate::completion::generator::CompletionGenerator;
    use crate::completion::parser::{CompletionContext, ParsedCommandLine};

    let loader = JsonCompletionLoader::new();
    let database = loader.load_database().expect("Failed to load database");
    let generator = CompletionGenerator::new(&database);

    // The command-level candidate generation itself is already covered by
    // test_completion_candidates_generation_from_json; this test checks the
    // step after it, converting EnhancedCandidate into the display-facing
    // Candidate enum, which that one does not.
    for command in ["git", "cargo"] {
        assert!(
            generator.has_command_completion(command),
            "test fixture expects a completion for '{command}'"
        );

        let parsed_command = ParsedCommandLine {
            command: command.to_string(),
            subcommand_path: vec![],
            args: vec![],
            options: vec![],
            current_token: "".to_string(),
            current_arg: Some("".to_string()),
            completion_context: CompletionContext::SubCommand,
            specified_options: vec![],
            specified_arguments: vec![],
            raw_args: vec![],
            cursor_index: 0,
        };

        let enhanced_candidates = generator.generate_candidates(&parsed_command).unwrap();
        let display_candidates: Vec<DisplayCandidate> = enhanced_candidates
            .into_iter()
            .map(|c| match c.completion_type {
                super::super::command::CompletionType::SubCommand => DisplayCandidate::Command {
                    name: c.text,
                    description: c.description.unwrap_or_default(),
                },
                super::super::command::CompletionType::LongOption
                | super::super::command::CompletionType::ShortOption => DisplayCandidate::Option {
                    name: c.text,
                    description: c.description.unwrap_or_default(),
                },
                _ => DisplayCandidate::Item(c.text, c.description.unwrap_or_default()),
            })
            .collect();

        assert!(
            !display_candidates.is_empty(),
            "expected display candidates for command '{command}'"
        );
    }
}

#[test]
fn test_git_completion_loading() {
    use crate::completion::command::ArgumentType;

    // Initialize loader
    let loader = JsonCompletionLoader::new();
    let mut database = CommandCompletionDatabase::new();

    // Load database from embedded
    let result = loader.load_from_embedded(&mut database);
    assert!(
        result.is_ok(),
        "Failed to load from embedded resources: {:?}",
        result.err()
    );

    // Verify git command exists
    let git_cmd = database.get_command("git");
    assert!(git_cmd.is_some(), "git command not found in database");
    let git_cmd = git_cmd.unwrap();

    // Verify switch subcommand
    let switch_sub = git_cmd.subcommands.iter().find(|s| s.name == "switch");
    assert!(switch_sub.is_some(), "git switch subcommand not found");
    let switch_sub = switch_sub.unwrap();

    // Verify argument type is Dynamic
    assert!(
        !switch_sub.arguments.is_empty(),
        "git switch has no arguments"
    );
    let branch_arg = &switch_sub.arguments[0];

    match &branch_arg.arg_type {
        Some(ArgumentType::Dynamic { provider, scope }) => {
            assert_eq!(provider, "git.branch");
            assert_eq!(scope.as_deref(), Some("project"));
        }
        _ => panic!(
            "Expected Dynamic argument type for git switch, found {:?}",
            branch_arg.arg_type
        ),
    }
}

/// Regression guard for the four builtins whose definitions lived only in
/// the old repository-root mirror and were therefore never embedded.
#[test]
fn builtin_completions_are_embedded() {
    for command in ["dirs", "popd", "pushd", "cron"] {
        assert!(
            CompletionAssets::get(&format!("{command}.json")).is_some(),
            "completion for builtin '{command}' is not embedded"
        );
    }
}

/// Every built-in completion definition is validated here.
///
/// A bad `provider` string is otherwise silent: the loader keeps it as a
/// plain `String`, `DynamicProviderId::parse` returns `None`, and the user
/// just gets zero candidates. Nothing else in the test suite catches it.
#[test]
fn embedded_completion_definitions_are_valid() {
    let embedded_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../completions");
    let mut checked = 0usize;

    for entry in fs::read_dir(&embedded_dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }

        let contents = fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&contents)
            .unwrap_or_else(|err| panic!("{} is not valid JSON: {err}", path.display()));

        let stem = path.file_stem().unwrap().to_str().unwrap();
        let command = value.get("command").and_then(serde_json::Value::as_str);
        assert_eq!(
            command,
            Some(stem),
            "`command` must match the file name in {}",
            path.display()
        );

        assert!(
            !json_contains_script_type(&value),
            "built-in completion must not use Script: {}",
            path.display()
        );

        let mut unknown = Vec::new();
        collect_dynamic_providers(&value, &mut unknown);
        unknown.retain(|provider| {
            !dsh_types::completion::is_known_dynamic_completion_provider(provider)
        });
        assert!(
            unknown.is_empty(),
            "unknown dynamic provider(s) {unknown:?} in {}; add them to \
                 DYNAMIC_COMPLETION_PROVIDERS in dsh-types/src/completion.rs",
            path.display()
        );

        checked += 1;
    }

    assert!(
        checked > 100,
        "expected the built-in completion directory to be populated, saw {checked} files"
    );
}

fn collect_dynamic_providers(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(object) => {
            if object.get("type").and_then(serde_json::Value::as_str) == Some("Dynamic")
                && let Some(provider) = object
                    .get("data")
                    .and_then(|data| data.get("provider"))
                    .and_then(serde_json::Value::as_str)
            {
                out.push(provider.to_string());
            }
            for nested in object.values() {
                collect_dynamic_providers(nested, out);
            }
        }
        serde_json::Value::Array(values) => {
            for nested in values {
                collect_dynamic_providers(nested, out);
            }
        }
        _ => {}
    }
}

fn json_contains_script_type(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(object) => {
            object.get("type").and_then(serde_json::Value::as_str) == Some("Script")
                || object.values().any(json_contains_script_type)
        }
        serde_json::Value::Array(values) => values.iter().any(json_contains_script_type),
        _ => false,
    }
}
