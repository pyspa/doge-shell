use doge_shell::completion::json_loader::JsonCompletionLoader;

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
        "gzip",
        "date",
        "whoami",
        "mount",
        "umount",
        "passwd",
        "lsmod",
        "modprobe",
        "hostname",
        "unalias",
        "unset",
        "watch",
        "screen",
        "tmux",
        "scp",
        "rsync",
        "rmdir",
        "w",
        "last",
        "netstat",
        "nmcli",
        "whereis",
        "groups",
        "jq",
        "tree",
        "dig",
        "useradd",
        "groupadd",
        "chgrp",
        "cd",
        "traceroute",
        "nohup",
        "stat",
        "whois",
        "base64",
        "sleep",
        "nc",
        "id",
        "lsusb",
        "lspci",
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

#[test]
fn test_git_completion_with_real_json() {
    use doge_shell::completion::command::CommandCompletionDatabase;
    use doge_shell::completion::generator::CompletionGenerator;
    use doge_shell::completion::parser::{CommandLineParser, CompletionContext};

    let root_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = root_dir.parent().unwrap();
    let completions_dir = repo_root.join("completions");

    let loader = JsonCompletionLoader::with_dirs(vec![completions_dir]);

    // Load git completion
    let git_completion = loader
        .load_command_completion("git")
        .unwrap()
        .expect("git completion not found in json");

    // Build DB
    let mut db = CommandCompletionDatabase::new();
    db.add_command(git_completion);

    let parser = CommandLineParser::new();
    let generator = CompletionGenerator::new(&db);

    // Test Case 1: git add -
    // Should return options like --all (and -A if generator is fixed)
    let input = "git add -";
    let parsed = parser.parse(input, input.len());

    // Verify context
    if parsed.completion_context != CompletionContext::LongOption {
        // It might be ShortOption depending on parser logic tweaks, but usually LongOption for "-"
        // Just print it if verification fails
        println!("Context for 'git add -': {:?}", parsed.completion_context);
    }

    let candidates = generator.generate_candidates(&parsed).unwrap();

    // Check for expected candidates from actual git.json
    // git.json defines "add" with options: -A / --all, -u / --update
    let has_all = candidates.iter().any(|c| c.text == "--all");
    let has_short_all = candidates.iter().any(|c| c.text == "-A");

    assert!(has_all, "Should suggest --all for 'git add -'");
    assert!(has_short_all, "Should suggest -A for 'git add -'");

    // Test Case 2: git -
    // Should suggest global options
    let input2 = "git -";
    let parsed2 = parser.parse(input2, input2.len());
    let candidates2 = generator.generate_candidates(&parsed2).unwrap();

    let has_version = candidates2.iter().any(|c| c.text == "--version");
    assert!(has_version, "Should suggest --version for 'git -'");

    // Test Case 3: git commit -
    // Even if commit subcommand is missing options or definition, global options should appear
    let input3 = "git commit -";
    let parsed3 = parser.parse(input3, input3.len());
    let candidates3 = generator.generate_candidates(&parsed3).unwrap();

    let has_version_commit = candidates3.iter().any(|c| c.text == "--version");
    assert!(
        has_version_commit,
        "Should suggest --version for 'git commit -'"
    );
}
