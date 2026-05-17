pub const DYNAMIC_COMPLETION_PROVIDERS: &[&str] = &[
    "apt.installed_package",
    "block.device",
    "cargo.bin",
    "cargo.example",
    "cargo.package",
    "dnf.installed_package",
    "docker.compose_service",
    "docker.container",
    "docker.image",
    "fstab.mountpoint",
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
    "js.dependency",
    "kernel.module",
    "kubectl.context",
    "kubectl.namespace",
    "kubectl.resource_name",
    "kubectl.resource_type",
    "mount.mountpoint",
    "nmcli.connection",
    "nmcli.device",
    "pacman.package",
    "pip.installed_package",
    "project.task",
    "rpm.installed_package",
    "rustup.toolchain",
    "screen.session",
    "ssh.host",
    "sysctl.key",
    "system.process_pid",
    "systemctl.unit",
    "systemctl.unit_file",
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
