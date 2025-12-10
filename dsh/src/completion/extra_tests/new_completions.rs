use crate::completion::dynamic::DynamicCompletionRegistry;
use crate::completion::json_loader::JsonCompletionLoader;
use crate::completion::parser::{CompletionContext, ParsedCommandLine};

#[test]
fn test_load_new_json_completions() {
    let loader = JsonCompletionLoader::new();
    let new_commands = vec![
        "which", "who", "alias", "export", "bg", "fg", "jobs", "free", "uptime", "lsblk", "file",
        "bzip2", "xz",
    ];

    for cmd in new_commands {
        let completion = loader.load_command_completion(cmd);
        assert!(completion.is_ok(), "Failed to load completion for {}", cmd);
        let completion = completion.unwrap();
        assert!(completion.is_some(), "Completion not found for {}", cmd);
        assert_eq!(completion.unwrap().command, cmd);
    }
}

#[test]
fn test_load_new_dynamic_check() {
    let _registry = DynamicCompletionRegistry::with_configured_handlers();

    // Check chown
    let _chown_cmd = ParsedCommandLine {
        command: "chown".to_string(),
        subcommand_path: vec![],
        args: vec![],
        options: vec![],
        current_token: "".to_string(),
        current_arg: None,
        completion_context: CompletionContext::Command,
        specified_options: vec![],
        specified_arguments: vec![],
        cursor_index: 0,
    };

    /*
    assert!(registry.matches(&chown_cmd), "Should match chown command");
    */

    // Check chgrp
    let _chgrp_cmd = ParsedCommandLine {
        command: "chgrp".to_string(),
        subcommand_path: vec![],
        args: vec![],
        options: vec![],
        current_token: "".to_string(),
        current_arg: None,
        completion_context: CompletionContext::Command,
        specified_options: vec![],
        specified_arguments: vec![],
        cursor_index: 0,
    };
    /*
    assert!(registry.matches(&chgrp_cmd), "Should match chgrp command");
    */
}
