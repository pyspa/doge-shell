use crate::completion::command::ArgumentType;
use crate::completion::json_loader::JsonCompletionLoader;

#[test]
fn test_load_new_json_completions() {
    let loader = JsonCompletionLoader::new();
    let new_commands = vec![
        "which",
        "who",
        "alias",
        "export",
        "bg",
        "fg",
        "jobs",
        "free",
        "uptime",
        "lsblk",
        "file",
        "bzip2",
        "xz",
        "networkctl",
        "ipset",
        "conntrack",
        "iw",
        "iwctl",
        "rfkill",
        "wg",
        "wg-quick",
        "dpkg",
        "rpm",
        "apk",
        "zypper",
        "lsns",
        "lsipc",
        "lslocks",
        "findfs",
        "cryptsetup",
        "smartctl",
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
fn wg_quick_up_uses_wireguard_config_provider() {
    let loader = JsonCompletionLoader::new();
    let completion = loader
        .load_command_completion("wg-quick")
        .expect("wg-quick completion should load")
        .expect("wg-quick completion should exist");
    let up = completion
        .subcommands
        .iter()
        .find(|subcommand| subcommand.name == "up")
        .expect("wg-quick up subcommand should exist");
    let provider = up
        .arguments
        .first()
        .and_then(|argument| argument.arg_type.as_ref())
        .and_then(|arg_type| match arg_type {
            ArgumentType::Dynamic { provider, .. } => Some(provider.as_str()),
            _ => None,
        });

    assert_eq!(provider, Some("wireguard.config"));
}
