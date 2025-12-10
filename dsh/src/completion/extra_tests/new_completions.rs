use crate::completion::json_loader::JsonCompletionLoader;

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
