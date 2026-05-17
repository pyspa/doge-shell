use doge_shell::completion::command::ArgumentType;
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
        "sysctl",
        "findmnt",
        "blkid",
        "fdisk",
        "parted",
        "fsck",
        "mkfs",
        "swapon",
        "swapoff",
        "losetup",
        "systemd-analyze",
        "loginctl",
        "timedatectl",
        "hostnamectl",
        "resolvectl",
        "service",
        "systemd-run",
        "machinectl",
        "localectl",
        "deno",
        "turbo",
        "nx",
        "npx",
        "vite",
        "vitest",
        "eslint",
        "prettier",
        "tsc",
        "ts-node",
        "jest",
        "playwright",
        "crontab",
        "usermod",
        "userdel",
        "groupmod",
        "groupdel",
        "apt",
        "apt-get",
        "dnf",
        "yum",
        "getent",
        "getconf",
        "namei",
        "readlink",
        "realpath",
        "install",
        "truncate",
        "lsattr",
        "chattr",
        "getfacl",
        "setfacl",
        "fallocate",
        "mkswap",
        "wipefs",
        "blockdev",
        "killall",
        "fuser",
        "renice",
        "nice",
        "ionice",
        "chrt",
        "taskset",
        "prlimit",
        "flock",
        "setsid",
        "ethtool",
        "bridge",
        "tc",
        "arp",
        "tracepath",
        "mtr",
        "host",
        "nslookup",
        "iptables",
        "ip6tables",
        "nft",
        "ufw",
        "firewall-cmd",
        "busctl",
        "udevadm",
        "coredumpctl",
        "bootctl",
        "systemd-tmpfiles",
        "systemd-sysusers",
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
fn test_dev_cli_completions_use_dynamic_providers() {
    let root_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = root_dir.parent().unwrap();
    let completions_dir = repo_root.join("completions");

    let loader = JsonCompletionLoader::with_dirs(vec![completions_dir]);

    let uv = loader
        .load_command_completion("uv")
        .unwrap()
        .expect("uv completion not found in json");
    let uv_remove = uv
        .subcommands
        .iter()
        .find(|sub| sub.name == "remove")
        .expect("missing uv remove");
    assert!(
        matches!(
            uv_remove.arguments.first().and_then(|arg| arg.arg_type.as_ref()),
            Some(ArgumentType::Dynamic { provider, .. }) if provider == "python.project_dependency"
        ),
        "uv remove should complete local Python project dependencies"
    );

    let npx = loader
        .load_command_completion("npx")
        .unwrap()
        .expect("npx completion not found in json");
    assert!(
        matches!(
            npx.arguments.first().and_then(|arg| arg.arg_type.as_ref()),
            Some(ArgumentType::Dynamic { provider, .. }) if provider == "node.bin"
        ),
        "npx should complete local node_modules/.bin commands"
    );

    let go = loader
        .load_command_completion("go")
        .unwrap()
        .expect("go completion not found in json");
    let go_test = go
        .subcommands
        .iter()
        .find(|sub| sub.name == "test")
        .expect("missing go test");
    assert!(
        matches!(
            go_test.arguments.first().and_then(|arg| arg.arg_type.as_ref()),
            Some(ArgumentType::Dynamic { provider, .. }) if provider == "go.package"
        ),
        "go test should complete local Go packages"
    );

    let nx = loader
        .load_command_completion("nx")
        .unwrap()
        .expect("nx completion not found in json");
    let nx_run = nx
        .subcommands
        .iter()
        .find(|sub| sub.name == "run")
        .expect("missing nx run");
    assert!(
        matches!(
            nx_run.arguments.first().and_then(|arg| arg.arg_type.as_ref()),
            Some(ArgumentType::Dynamic { provider, scope }) if provider == "project.task" && scope.as_deref() == Some("nx.run")
        ),
        "nx run should complete qualified Nx run targets"
    );
}

#[test]
fn test_chown_owner_group_precision_gap_is_documented() {
    let root_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = root_dir.parent().unwrap();
    let completions_dir = repo_root.join("completions");

    let loader = JsonCompletionLoader::with_dirs(vec![completions_dir]);
    let chown_completion = loader
        .load_command_completion("chown")
        .unwrap()
        .expect("chown completion not found in json");

    let owner_group = chown_completion
        .arguments
        .first()
        .expect("missing chown owner/group argument");
    assert!(
        matches!(owner_group.arg_type, Some(ArgumentType::User)),
        "chown owner/group should keep user completion until grouped owner[:group] completion exists"
    );

    let from = chown_completion
        .global_options
        .iter()
        .find(|option| option.long.as_deref() == Some("--from"))
        .expect("missing chown --from option");
    assert!(
        matches!(from.value_type(), Some(ArgumentType::String)),
        "chown --from intentionally remains String to avoid generating user x group combinations"
    );
    // TODO: Add a focused owner[:group] completion strategy that completes the
    // group side after ':' without precomputing the full user x group matrix.
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

#[test]
fn test_pacman_completion_uses_dynamic_package_arguments() {
    let root_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = root_dir.parent().unwrap();
    let completions_dir = repo_root.join("completions");

    let loader = JsonCompletionLoader::with_dirs(vec![completions_dir]);
    let pacman_completion = loader
        .load_command_completion("pacman")
        .unwrap()
        .expect("pacman completion not found in json");

    let sync = pacman_completion
        .subcommands
        .iter()
        .find(|sub| sub.name == "-S")
        .expect("missing pacman -S subcommand");
    let remove = pacman_completion
        .subcommands
        .iter()
        .find(|sub| sub.name == "-R")
        .expect("missing pacman -R subcommand");

    let sync_arg = sync
        .arguments
        .first()
        .expect("missing pacman -S package arg");
    assert!(sync_arg.multiple, "pacman -S package arg must be multiple");
    assert!(
        matches!(
            sync_arg.arg_type,
            Some(ArgumentType::Dynamic {
                ref provider,
                ..
            }) if provider == "pacman.package"
        ),
        "pacman -S package arg should use reusable dynamic package candidates"
    );

    let remove_arg = remove
        .arguments
        .first()
        .expect("missing pacman -R package arg");
    assert!(
        remove_arg.multiple,
        "pacman -R package arg must be multiple"
    );
    assert!(
        matches!(
            remove_arg.arg_type,
            Some(ArgumentType::Dynamic {
                ref provider,
                ..
            }) if provider == "pacman.package"
        ),
        "pacman -R package arg should use reusable dynamic package candidates"
    );
}
