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
