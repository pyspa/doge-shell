pub const DYNAMIC_COMPLETION_PROVIDERS: &[&str] = &[
    "apt.installed_package",
    "aws.profile",
    "block.device",
    "block.label",
    "block.uuid",
    "cargo.bin",
    "cargo.example",
    "cargo.package",
    "dbus.service",
    "dnf.installed_package",
    "docker.compose_service",
    "docker.container",
    "docker.image",
    "docker.network",
    "docker.volume",
    "filesystem.type",
    "fstab.mountpoint",
    "gcloud.configuration",
    "gcloud.project",
    "git.branch",
    "git.changed_path",
    "git.checkout_target",
    "git.push_branch",
    "git.remote",
    "git.remote_branch",
    "git.revision",
    "git.stash",
    "git.tag",
    "git.worktree",
    "go.package",
    "js.dependency",
    "kernel.module",
    "kubectl.context",
    "kubectl.namespace",
    "kubectl.resource_name",
    "kubectl.resource_type",
    "localectl.keymap",
    "localectl.locale",
    "loginctl.seat",
    "loginctl.session",
    "loop.device",
    "mount.mountpoint",
    "nmcli.connection",
    "nmcli.device",
    "node.bin",
    "node.workspace",
    "pacman.package",
    "pip.installed_package",
    "podman.container",
    "podman.image",
    "podman.network",
    "podman.volume",
    "project.task",
    "python.module",
    "python.project_dependency",
    "rpm.installed_package",
    "rustup.toolchain",
    "screen.session",
    "ssh.host",
    "swap.device",
    "sysctl.key",
    "system.process_name",
    "system.process_pid",
    "systemctl.unit",
    "systemctl.unit_file",
    "terraform.workspace",
    "timedatectl.timezone",
    "tmux.session",
];

pub fn is_known_dynamic_completion_provider(provider: &str) -> bool {
    DYNAMIC_COMPLETION_PROVIDERS
        .binary_search(&provider)
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dynamic_completion_providers_are_sorted_and_unique() {
        for window in DYNAMIC_COMPLETION_PROVIDERS.windows(2) {
            assert!(
                window[0] < window[1],
                "provider list must stay sorted and unique for binary_search: {:?}",
                window
            );
        }
    }
}
