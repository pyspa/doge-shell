use super::{
    CachePolicy, DynamicCompletionProvider, container, dev, external, git, kubernetes, linux,
    project,
};
use crate::completion::integrated::EnhancedCandidate;
use crate::completion::parser::ParsedCommandLine;
use dsh_types::completion::DynamicProviderId;
use std::path::Path;

pub(crate) struct DynamicProviderRequest<'a> {
    pub(super) provider: DynamicProviderId,
    pub(super) scope: Option<&'a str>,
    pub(super) parsed_command_line: &'a ParsedCommandLine,
    pub(super) current_dir: &'a Path,
    pub(super) cache_policy: CachePolicy,
}

pub(crate) type ProviderCollector = for<'a> fn(
    &DynamicCompletionProvider,
    &DynamicProviderRequest<'a>,
) -> Option<Vec<EnhancedCandidate>>;

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

#[derive(Debug, Clone, Copy)]
pub(crate) struct ProviderRegistration {
    pub id: DynamicProviderId,
    pub family: ProviderFamily,
    pub collector: ProviderCollector,
}

impl ProviderRegistration {
    pub(crate) fn collect(
        self,
        provider: &DynamicCompletionProvider,
        request: &DynamicProviderRequest<'_>,
    ) -> Option<Vec<EnhancedCandidate>> {
        debug_assert!(std::ptr::fn_addr_eq(
            self.collector,
            collector_for(self.family)
        ));
        (self.collector)(provider, request)
    }
}

pub(crate) fn registration(provider: &str) -> Option<ProviderRegistration> {
    let id = DynamicProviderId::parse(provider)?;
    Some(registration_for_id(id))
}

#[cfg(test)]
pub(crate) fn registrations() -> impl Iterator<Item = ProviderRegistration> {
    DynamicProviderId::all().map(registration_for_id)
}

fn registration_for_id(id: DynamicProviderId) -> ProviderRegistration {
    let family = family_for(id.as_str());
    ProviderRegistration {
        id,
        family,
        collector: collector_for(family),
    }
}

fn collector_for(family: ProviderFamily) -> ProviderCollector {
    match family {
        ProviderFamily::Git => git::collect,
        ProviderFamily::Container => container::collect,
        ProviderFamily::Kubernetes => kubernetes::collect,
        ProviderFamily::Linux => linux::collect,
        ProviderFamily::Development => dev::collect,
        ProviderFamily::Project => project::collect,
        ProviderFamily::External => external::collect,
    }
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
                | "login"
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
                | "udev"
                | "wireguard"
                | "wireless"
                | "zfs"
                | "zpool"
        )
    ) {
        ProviderFamily::Linux
    } else if matches!(
        provider.split_once('.').map(|(prefix, _)| prefix),
        Some(
            "asdf"
                | "bat"
                | "cargo"
                | "code"
                | "ffmpeg"
                | "go"
                | "hatch"
                | "js"
                | "maven"
                | "mise"
                | "node"
                | "nox"
                | "pip"
                | "pipx"
                | "pre_commit"
                | "python"
                | "rg"
                | "rustup"
                | "terraform"
                | "tox"
        )
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
