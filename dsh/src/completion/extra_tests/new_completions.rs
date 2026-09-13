use crate::completion::command::{
    Argument, ArgumentType, CommandCompletion, CommandOption, SubCommand,
};
use crate::completion::json_loader::JsonCompletionLoader;

// A generic "does this command's completion load and does `command` match the
// file name" sweep over every embedded completion file already lives in
// dsh/src/completion/json_loader/tests.rs
// (test_all_embedded_completion_files_load_correctly,
// embedded_completion_definitions_are_valid), so this file keeps only the
// tests that check something that sweep cannot: that a *specific* argument or
// option on a *specific* command wires to the *right* dynamic provider or CLI
// contract, not merely a known one.

#[test]
fn arch_command_options_match_current_cli_contracts() {
    let loader = JsonCompletionLoader::new();

    let pacman_key = loader
        .load_command_completion("pacman-key")
        .unwrap()
        .expect("pacman-key completion");
    let export = pacman_key
        .global_options
        .iter()
        .find(|option| option.short.as_deref() == Some("-e"))
        .expect("pacman-key -e");
    assert_eq!(export.long.as_deref(), Some("--export"));
    assert!(
        pacman_key
            .global_options
            .iter()
            .any(|option| option.short.is_none() && option.long.as_deref() == Some("--edit-key"))
    );
    assert!(
        !pacman_key
            .global_options
            .iter()
            .any(|option| option.short.as_deref() == Some("-x"))
    );

    let mkarchroot = loader
        .load_command_completion("mkarchroot")
        .unwrap()
        .expect("mkarchroot completion");
    let copy = mkarchroot
        .global_options
        .iter()
        .find(|option| option.short.as_deref() == Some("-f"))
        .expect("mkarchroot -f");
    assert!(copy.expects_value());
    assert!(matches!(copy.value_type(), Some(ArgumentType::File { .. })));
    assert!(
        mkarchroot
            .global_options
            .iter()
            .all(|option| option.long.is_none())
    );

    let makechrootpkg = loader
        .load_command_completion("makechrootpkg")
        .unwrap()
        .expect("makechrootpkg completion");
    let namcap = makechrootpkg
        .global_options
        .iter()
        .find(|option| option.short.as_deref() == Some("-n"))
        .expect("makechrootpkg -n");
    assert!(
        namcap
            .description
            .as_deref()
            .is_some_and(|description| description.contains("namcap"))
    );
    assert!(
        !makechrootpkg
            .global_options
            .iter()
            .any(|option| option.short.as_deref() == Some("-s"))
    );

    let arch_nspawn = loader
        .load_command_completion("arch-nspawn")
        .unwrap()
        .expect("arch-nspawn completion");
    for short in ["-C", "-M", "-c", "-f", "-s", "-h"] {
        assert!(
            arch_nspawn
                .global_options
                .iter()
                .any(|option| option.short.as_deref() == Some(short)),
            "arch-nspawn should expose {short}"
        );
    }
    assert!(arch_nspawn.global_options.iter().all(|option| {
        !matches!(option.short.as_deref(), Some("-b" | "-q")) && option.long.is_none()
    }));
}

/// Table-driven: every `(command, provider)` pair the JSON completion for
/// `command` must wire *somewhere* (option, argument, or subcommand) to
/// `provider`. Consolidates what used to be five near-identical per-batch
/// test functions differing only in which commands they covered.
#[test]
fn json_completions_wire_expected_dynamic_providers() {
    let loader = JsonCompletionLoader::new();
    let expected = [
        // new_command_batch
        ("bacon", "bacon.job"),
        ("ghq", "ghq.repository"),
        ("golangci-lint", "golangci_lint.linter"),
        ("jj", "jj.bookmark"),
        ("jj", "jj.revision"),
        ("jj", "jj.workspace"),
        ("meson", "meson.target"),
        ("mkinitcpio", "mkinitcpio.preset"),
        ("pacman-conf", "pacman.repository"),
        ("pdm", "pdm.script"),
        ("pipenv", "pipenv.script"),
        ("snapper", "snapper.config"),
        ("snapper", "snapper.snapshot"),
        ("yay", "pacman.package"),
        ("paru", "pacman.package"),
        ("cargo-nextest", "cargo.package"),
        ("dlv", "system.process_pid"),
        // linux_operations
        ("ip", "ip.netns"),
        ("ip", "ip.route_table"),
        ("journalctl", "journalctl.identifier"),
        ("machinectl", "machinectl.machine"),
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
        ("semanage", "selinux.boolean"),
        ("setsebool", "selinux.boolean"),
        ("getsebool", "selinux.boolean"),
        ("systemctl", "systemctl.unit_file"),
        ("systemd-run", "systemctl.unit"),
        ("systemd-run", "machinectl.machine"),
        ("ufw", "ufw.application"),
        ("iw", "wireless.device"),
        ("udevadm", "udev.subsystem"),
        ("chsh", "login.shell"),
        ("useradd", "login.shell"),
        ("usermod", "login.shell"),
        ("modinfo", "kernel.module"),
        ("rmmod", "kernel.module"),
        ("modprobe", "kernel.module"),
        ("lsof", "system.process_pid"),
        ("lsof", "system.process_name"),
        ("htop", "system.process_pid"),
        ("apropos", "man.page"),
        ("whatis", "man.page"),
        ("ping", "ssh.host"),
        ("traceroute", "ssh.host"),
        ("dig", "ssh.host"),
        // remote_cli
        ("gh", "gh.repository"),
        ("glab", "glab.project"),
        ("argocd", "argocd.application"),
        ("flux", "kubectl.resource_name"),
        ("aws", "aws.eks_cluster"),
        ("gcloud", "gcloud.compute_instance"),
        ("az", "az.resource_group"),
        ("terraform", "terraform.resource"),
        ("tofu", "terraform.resource"),
        ("vault", "vault.policy"),
        ("nomad", "nomad.job"),
        ("rclone", "rclone.remote"),
        ("restic", "restic.snapshot"),
        ("flatpak", "flatpak.application"),
        ("snap", "snap.package"),
        // developer_toolchain
        ("rustup", "rustup.component"),
        ("rustup", "rustup.target"),
        ("rustup", "rustup.toolchain"),
        ("cargo", "cargo.installed_binary"),
        ("cargo", "cargo.test"),
        ("cargo", "cargo.bench"),
        ("bat", "bat.theme"),
        ("bat", "bat.language"),
        ("rg", "rg.file_type"),
        ("ffmpeg", "ffmpeg.encoder"),
        ("ffmpeg", "ffmpeg.decoder"),
        ("ffmpeg", "ffmpeg.format"),
        ("ffprobe", "ffmpeg.format"),
        ("go", "go.env_key"),
        ("pipx", "pipx.installed_package"),
        ("asdf", "asdf.plugin"),
        ("mise", "mise.tool"),
        ("code", "code.extension"),
        ("nox", "nox.session"),
        ("tox", "tox.environment"),
        ("hatch", "hatch.environment"),
        ("pre-commit", "pre_commit.hook_id"),
        ("just", "project.task"),
        ("make", "project.task"),
        ("git", "git.alias"),
    ];

    for (command, provider) in expected {
        let completion = loader
            .load_command_completion(command)
            .unwrap()
            .unwrap_or_else(|| panic!("{command} completion"));
        assert!(
            completion_uses_dynamic_provider(&completion, provider),
            "{command} should use {provider}"
        );
    }

    let cargo = loader
        .load_command_completion("cargo")
        .unwrap()
        .expect("cargo completion");
    for command in ["nextest", "watch"] {
        assert!(
            cargo
                .subcommands
                .iter()
                .any(|subcommand| subcommand.name == command),
            "cargo should expose {command}"
        );
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
