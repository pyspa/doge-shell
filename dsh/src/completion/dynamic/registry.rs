use dsh_types::completion::DynamicProviderId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderFamily {
    Git,
    Container,
    Kubernetes,
    Linux,
    Development,
    Project,
    External,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProviderRegistration {
    pub id: DynamicProviderId,
    pub family: ProviderFamily,
}

pub(crate) fn registration(provider: &str) -> Option<ProviderRegistration> {
    let id = DynamicProviderId::parse(provider)?;
    Some(ProviderRegistration {
        id,
        family: family_for(id.as_str()),
    })
}

#[cfg(test)]
pub(crate) fn registrations() -> impl Iterator<Item = ProviderRegistration> {
    DynamicProviderId::all().map(|id| ProviderRegistration {
        id,
        family: family_for(id.as_str()),
    })
}

fn family_for(provider: &str) -> ProviderFamily {
    if provider.starts_with("git.") {
        ProviderFamily::Git
    } else if provider.starts_with("docker.") || provider.starts_with("podman.") {
        ProviderFamily::Container
    } else if provider.starts_with("kubectl.")
        || provider.starts_with("helm.")
        || matches!(
            provider,
            "kind.cluster" | "k3d.cluster" | "minikube.profile"
        )
    {
        ProviderFamily::Kubernetes
    } else if matches!(
        provider.split_once('.').map(|(prefix, _)| prefix),
        Some(
            "block"
                | "dbus"
                | "firewalld"
                | "fstab"
                | "ip"
                | "ipset"
                | "journalctl"
                | "kernel"
                | "localectl"
                | "loginctl"
                | "loop"
                | "lvm"
                | "mount"
                | "networkctl"
                | "nft"
                | "nmcli"
                | "screen"
                | "selinux"
                | "swap"
                | "sysctl"
                | "system"
                | "systemctl"
                | "timedatectl"
                | "tmux"
                | "wireguard"
                | "zfs"
                | "zpool"
        )
    ) {
        ProviderFamily::Linux
    } else if matches!(
        provider.split_once('.').map(|(prefix, _)| prefix),
        Some("cargo" | "go" | "js" | "maven" | "node" | "pip" | "python" | "rustup" | "terraform")
    ) {
        ProviderFamily::Development
    } else if matches!(
        provider,
        "project.task"
            | "archive.entry"
            | "filesystem.type"
            | "man.page"
            | "shell.abbr"
            | "shell.alias"
            | "shell.env_var"
            | "shell.job"
            | "ssh.host"
    ) {
        ProviderFamily::Project
    } else {
        ProviderFamily::External
    }
}
