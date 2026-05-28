use crate::completion::command::{
    Argument, ArgumentType, CommandCompletion, CommandOption, SubCommand,
};
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
        "abbr",
        "bookmark",
        "blocks",
        "doctor",
        "out",
        "pm",
        "project",
        "pj",
        "safe-run",
        "snippet",
        "task",
        "trigger",
        "gpg",
        "age",
        "sops",
        "pass",
        "op",
        "rclone",
        "restic",
        "borg",
        "nix",
        "nix-env",
        "nix-shell",
        "flatpak",
        "snap",
        "mysql",
        "mysqladmin",
        "mongosh",
        "http",
        "xh",
        "hyperfine",
        "tokei",
        "lazygit",
        "gitui",
        "zoxide",
        "doctl",
        "flyctl",
        "vercel",
        "netlify",
        "ansible-lint",
        "ansible-galaxy",
        "ansible-vault",
        "bazel",
        "mvnw",
        "gradlew",
        "basename",
        "dirname",
        "dd",
        "dir",
        "mkfifo",
        "mknod",
        "mktemp",
        "od",
        "paste",
        "printenv",
        "pwd",
        "shred",
        "split",
        "stty",
        "sync",
        "tr",
        "true",
        "false",
        "yes",
        "at",
        "batch",
        "timeout",
        "logger",
        "logrotate",
        "iostat",
        "mpstat",
        "pidstat",
        "sar",
        "vmstat",
        "perf",
        "ldd",
        "ldconfig",
        "arping",
        "ping6",
        "ncat",
        "socat",
        "tshark",
        "sfdisk",
        "cfdisk",
        "partprobe",
        "e2fsck",
        "tune2fs",
        "dumpe2fs",
        "resize2fs",
        "xfs_info",
        "xfs_growfs",
        "xfs_repair",
        "btrfs",
        "zfs",
        "zpool",
        "lvm",
        "pvcreate",
        "pvdisplay",
        "pvs",
        "vgcreate",
        "vgdisplay",
        "vgs",
        "lvcreate",
        "lvdisplay",
        "lvs",
        "mdadm",
        "dmsetup",
        "chage",
        "su",
        "visudo",
        "lastlog",
        "users",
        "getcap",
        "setcap",
        "semanage",
        "restorecon",
        "ausearch",
        "auditctl",
        "sestatus",
        "semodule",
        "gunzip",
        "bunzip2",
        "unxz",
        "locate",
        "updatedb",
        "column",
        "patch",
        "cmp",
        "comm",
        "sha256sum",
        "sha1sum",
        "md5sum",
        "cksum",
        "strings",
        "hexdump",
        "xxd",
        "type",
        "command",
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

#[test]
fn strengthened_json_completions_use_dynamic_providers() {
    let loader = JsonCompletionLoader::new();

    let az = loader
        .load_command_completion("az")
        .unwrap()
        .expect("az completion");
    assert_eq!(
        az.global_options
            .iter()
            .find(|option| option.long.as_deref() == Some("--subscription"))
            .and_then(|option| option.value_type.as_ref())
            .and_then(dynamic_provider),
        Some("az.subscription")
    );

    let gradle = loader
        .load_command_completion("gradle")
        .unwrap()
        .expect("gradle completion");
    assert_eq!(
        gradle
            .arguments
            .first()
            .and_then(|argument| argument.arg_type.as_ref())
            .and_then(dynamic_provider),
        Some("project.task")
    );

    let mvn = loader
        .load_command_completion("mvn")
        .unwrap()
        .expect("mvn completion");
    assert_eq!(
        mvn.global_options
            .iter()
            .find(|option| option.short.as_deref() == Some("-P"))
            .and_then(|option| option.value_type.as_ref())
            .and_then(dynamic_provider),
        Some("maven.profile")
    );

    let helm = loader
        .load_command_completion("helm")
        .unwrap()
        .expect("helm completion");
    let status = helm
        .subcommands
        .iter()
        .find(|subcommand| subcommand.name == "status")
        .expect("helm status");
    assert_eq!(
        status
            .arguments
            .first()
            .and_then(|argument| argument.arg_type.as_ref())
            .and_then(dynamic_provider),
        Some("helm.release")
    );
}

#[test]
fn linux_operations_json_completions_use_dynamic_providers() {
    let loader = JsonCompletionLoader::new();
    let providers = [
        ("ip", "ip.route_table"),
        ("nft", "nft.table"),
        ("nft", "nft.chain"),
        ("lvm", "lvm.volume_group"),
        ("lvdisplay", "lvm.logical_volume"),
        ("pvdisplay", "lvm.physical_volume"),
        ("zfs", "zfs.dataset"),
        ("zpool", "zpool.pool"),
        ("btrfs", "btrfs.subvolume"),
        ("mdadm", "mdadm.array"),
        ("dmsetup", "dmsetup.device"),
        ("auditctl", "audit.rule_key"),
        ("ausearch", "audit.rule_key"),
        ("semodule", "selinux.module"),
        ("semanage", "selinux.module"),
    ];

    for (command, provider) in providers {
        let completion = loader
            .load_command_completion(command)
            .unwrap()
            .unwrap_or_else(|| panic!("{command} completion"));
        assert!(
            completion_uses_dynamic_provider(&completion, provider),
            "{command} should use {provider}"
        );
    }
}

fn dynamic_provider(arg_type: &ArgumentType) -> Option<&str> {
    match arg_type {
        ArgumentType::Dynamic { provider, .. } => Some(provider.as_str()),
        _ => None,
    }
}

fn completion_uses_dynamic_provider(completion: &CommandCompletion, provider: &str) -> bool {
    completion
        .global_options
        .iter()
        .any(|option| option_uses_dynamic_provider(option, provider))
        || completion
            .arguments
            .iter()
            .any(|argument| argument_uses_dynamic_provider(argument, provider))
        || completion
            .subcommands
            .iter()
            .any(|subcommand| subcommand_uses_dynamic_provider(subcommand, provider))
}

fn subcommand_uses_dynamic_provider(subcommand: &SubCommand, provider: &str) -> bool {
    subcommand
        .options
        .iter()
        .any(|option| option_uses_dynamic_provider(option, provider))
        || subcommand
            .arguments
            .iter()
            .any(|argument| argument_uses_dynamic_provider(argument, provider))
        || subcommand
            .subcommands
            .iter()
            .any(|nested| subcommand_uses_dynamic_provider(nested, provider))
}

fn option_uses_dynamic_provider(option: &CommandOption, provider: &str) -> bool {
    option
        .value_type
        .as_ref()
        .and_then(dynamic_provider)
        .is_some_and(|actual| actual == provider)
        || option
            .argument
            .as_ref()
            .is_some_and(|argument| argument_uses_dynamic_provider(argument, provider))
}

fn argument_uses_dynamic_provider(argument: &Argument, provider: &str) -> bool {
    argument
        .arg_type
        .as_ref()
        .and_then(dynamic_provider)
        .is_some_and(|actual| actual == provider)
}
