use doge_shell::completion::{
    dynamic::DynamicCompletionRegistry, json_loader::JsonCompletionLoader,
};
use std::path::PathBuf;

#[test]
fn test_new_json_completions_load() {
    let root_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // We need to point to the correct completions directory
    // In tests, CARGO_MANIFEST_DIR points to dsh crate root
    // But our completions are in parent root/completions
    let repo_root = root_dir.parent().unwrap();
    let completions_dir = repo_root.join("completions");

    let loader = JsonCompletionLoader::with_dirs(vec![completions_dir]);

    let commands = vec![
        "gzip", "date", "whoami", "mount", "umount", "passwd", "lsmod", "modprobe", "hostname",
        "unalias", "unset", "watch", "screen", "tmux", "scp", "rsync", "rmdir", "w", "last",
        "netstat", "nmcli", "whereis", "groups",
    ];

    for cmd in commands {
        match loader.load_command_completion(cmd) {
            Ok(Some(completion)) => {
                println!("Successfully loaded completion for {}", cmd);
                assert_eq!(completion.command, cmd);
            }
            Ok(None) => panic!("Failed to load completion for {}: returned None", cmd),
            Err(e) => panic!("Failed to load completion for {}: {}", cmd, e),
        }
    }
}

// Note: Testing dynamic completions that run shell commands (like ps, cat /etc/passwd)
// in a CI/Test environment can be flaky or fail due to permissions/missing files.
// We will test that they are loaded correctly into the registry.
#[test]
fn test_new_dynamic_completions_load() {
    let registry = DynamicCompletionRegistry::with_configured_handlers();

    // We can't easily check internal handlers list without public access,
    // but we can check if they match dummy commands.

    // Check renice match
    let renice_match = registry.matches(&doge_shell::completion::parser::ParsedCommandLine {
        command: "renice".to_string(),
        subcommand_path: vec![],
        args: vec!["-p".to_string()],
        options: vec![],
        current_token: "".to_string(),
        current_arg: None,
        completion_context: doge_shell::completion::parser::CompletionContext::Command,
        specified_options: vec![],
        specified_arguments: vec![],
        cursor_index: 0,
    });
    // Note: The renice config requires subcommands=[], args_contains=["-p"]
    // Our dummy parsed command has args=["-p"], so it should match if logic is correct.
    assert!(renice_match, "Registry should match renice command with -p");

    // Check userdel match (StartsWithCommand)
    let userdel_match = registry.matches(&doge_shell::completion::parser::ParsedCommandLine {
        command: "userdel".to_string(),
        subcommand_path: vec![],
        args: vec![],
        options: vec![],
        current_token: "".to_string(),
        current_arg: None,
        completion_context: doge_shell::completion::parser::CompletionContext::Command,
        specified_options: vec![],
        specified_arguments: vec![],
        cursor_index: 0,
    });
    assert!(userdel_match, "Registry should match userdel command");

    // Check pwdx match
    let pwdx_match = registry.matches(&doge_shell::completion::parser::ParsedCommandLine {
        command: "pwdx".to_string(),
        subcommand_path: vec![],
        args: vec!["123".to_string()],
        options: vec![],
        current_token: "".to_string(),
        current_arg: None,
        completion_context: doge_shell::completion::parser::CompletionContext::Command,
        specified_options: vec![],
        specified_arguments: vec![],
        cursor_index: 0,
    });
    assert!(pwdx_match, "Registry should match pwdx command");

    // Check groupmod match
    let groupmod_match = registry.matches(&doge_shell::completion::parser::ParsedCommandLine {
        command: "groupmod".to_string(),
        subcommand_path: vec![],
        args: vec!["groupname".to_string()],
        options: vec![],
        current_token: "".to_string(),
        current_arg: None,
        completion_context: doge_shell::completion::parser::CompletionContext::Command,
        specified_options: vec![],
        specified_arguments: vec![],
        cursor_index: 0,
    });
    assert!(groupmod_match, "Registry should match groupmod command");

    // Check pgrep match
    let pgrep_match = registry.matches(&doge_shell::completion::parser::ParsedCommandLine {
        command: "pgrep".to_string(),
        subcommand_path: vec![],
        args: vec!["bash".to_string()],
        options: vec![],
        current_token: "".to_string(),
        current_arg: None,
        completion_context: doge_shell::completion::parser::CompletionContext::Command,
        specified_options: vec![],
        specified_arguments: vec![],
        cursor_index: 0,
    });
    assert!(pgrep_match, "Registry should match pgrep command");

    // Check pidof match
    let pidof_match = registry.matches(&doge_shell::completion::parser::ParsedCommandLine {
        command: "pidof".to_string(),
        subcommand_path: vec![],
        args: vec!["bash".to_string()],
        options: vec![],
        current_token: "".to_string(),
        current_arg: None,
        completion_context: doge_shell::completion::parser::CompletionContext::Command,
        specified_options: vec![],
        specified_arguments: vec![],
        cursor_index: 0,
    });
    assert!(pidof_match, "Registry should match pidof command");

    // Check umount match
    let umount_match = registry.matches(&doge_shell::completion::parser::ParsedCommandLine {
        command: "umount".to_string(),
        subcommand_path: vec![],
        args: vec!["/mnt".to_string()],
        options: vec![],
        current_token: "".to_string(),
        current_arg: None,
        completion_context: doge_shell::completion::parser::CompletionContext::Command,
        specified_options: vec![],
        specified_arguments: vec![],
        cursor_index: 0,
    });
    assert!(umount_match, "Registry should match umount command");
}
