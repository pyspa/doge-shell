use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::str::FromStr;

pub const DYNAMIC_COMPLETION_PROVIDERS: &[&str] = &[
    "ansible.inventory_host",
    "apk.installed_package",
    "apt.installed_package",
    "archive.entry",
    "argocd.application",
    "argocd.cluster",
    "argocd.context",
    "argocd.project",
    "argocd.repository",
    "asdf.plugin",
    "audit.rule_key",
    "aws.eks_cluster",
    "aws.profile",
    "aws.region",
    "aws.s3_bucket",
    "az.aks_cluster",
    "az.resource_group",
    "az.subscription",
    "az.vm",
    "bat.language",
    "bat.theme",
    "block.device",
    "block.label",
    "block.uuid",
    "brew.installed",
    "btrfs.subvolume",
    "cargo.bench",
    "cargo.bin",
    "cargo.example",
    "cargo.feature",
    "cargo.installed_binary",
    "cargo.package",
    "cargo.test",
    "code.extension",
    "dbus.service",
    "dmsetup.device",
    "dnf.installed_package",
    "docker.compose_service",
    "docker.container",
    "docker.image",
    "docker.network",
    "docker.volume",
    "ffmpeg.decoder",
    "ffmpeg.encoder",
    "ffmpeg.format",
    "filesystem.type",
    "firewalld.icmp_type",
    "firewalld.service",
    "firewalld.zone",
    "flatpak.application",
    "fstab.mountpoint",
    "gcloud.account",
    "gcloud.compute_instance",
    "gcloud.configuration",
    "gcloud.gke_cluster",
    "gcloud.project",
    "gh.issue",
    "gh.pull_request",
    "gh.repository",
    "gh.run",
    "gh.workflow",
    "git.alias",
    "git.branch",
    "git.changed_path",
    "git.checkout_target",
    "git.config_key",
    "git.push_branch",
    "git.remote",
    "git.remote_branch",
    "git.revision",
    "git.stash",
    "git.tag",
    "git.worktree",
    "glab.issue",
    "glab.merge_request",
    "glab.pipeline",
    "glab.project",
    "go.env_key",
    "go.package",
    "hatch.environment",
    "helm.release",
    "ip.netns",
    "ip.route_table",
    "ipset.set",
    "journalctl.boot",
    "journalctl.identifier",
    "js.dependency",
    "k3d.cluster",
    "kernel.module",
    "kind.cluster",
    "kubectl.context",
    "kubectl.namespace",
    "kubectl.resource_name",
    "kubectl.resource_type",
    "localectl.keymap",
    "localectl.locale",
    "login.shell",
    "loginctl.seat",
    "loginctl.session",
    "loop.device",
    "lvm.logical_volume",
    "lvm.physical_volume",
    "lvm.volume_group",
    "machinectl.machine",
    "man.page",
    "maven.module",
    "maven.profile",
    "mdadm.array",
    "minikube.profile",
    "mise.tool",
    "mount.mountpoint",
    "networkctl.link",
    "nft.chain",
    "nft.table",
    "nmcli.connection",
    "nmcli.device",
    "node.bin",
    "node.workspace",
    "nomad.allocation",
    "nomad.job",
    "nomad.namespace",
    "nomad.node",
    "nomad.volume",
    "nox.session",
    "ollama.model",
    "pacman.package",
    "pip.installed_package",
    "pipx.installed_package",
    "podman.container",
    "podman.image",
    "podman.network",
    "podman.volume",
    "pre_commit.hook_id",
    "project.task",
    "python.module",
    "python.project_dependency",
    "rclone.remote",
    "restic.snapshot",
    "rg.file_type",
    "rpm.installed_package",
    "rustup.component",
    "rustup.target",
    "rustup.toolchain",
    "screen.session",
    "selinux.boolean",
    "selinux.module",
    "shell.abbr",
    "shell.alias",
    "shell.env_var",
    "shell.job",
    "snap.package",
    "ssh.host",
    "swap.device",
    "sysctl.key",
    "system.owner_group",
    "system.process_name",
    "system.process_pid",
    "systemctl.unit",
    "systemctl.unit_file",
    "terraform.resource",
    "terraform.workspace",
    "timedatectl.timezone",
    "tmux.session",
    "tox.environment",
    "udev.subsystem",
    "ufw.application",
    "vault.auth_method",
    "vault.policy",
    "vault.secrets_engine",
    "wireguard.config",
    "wireguard.interface",
    "wireless.device",
    "zfs.dataset",
    "zpool.pool",
    "zypper.installed_package",
];

/// A validated dynamic completion provider identifier.
///
/// The wire representation intentionally remains the existing provider string
/// so completion JSON stays backwards compatible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DynamicProviderId(&'static str);

impl DynamicProviderId {
    pub fn parse(provider: &str) -> Option<Self> {
        let index = DYNAMIC_COMPLETION_PROVIDERS.binary_search(&provider).ok()?;
        Some(Self(DYNAMIC_COMPLETION_PROVIDERS[index]))
    }

    pub const fn as_str(self) -> &'static str {
        self.0
    }

    pub fn all() -> impl ExactSizeIterator<Item = Self> {
        DYNAMIC_COMPLETION_PROVIDERS.iter().copied().map(Self)
    }
}

impl fmt::Display for DynamicProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl FromStr for DynamicProviderId {
    type Err = String;

    fn from_str(provider: &str) -> Result<Self, Self::Err> {
        Self::parse(provider)
            .ok_or_else(|| format!("unknown dynamic completion provider: {provider}"))
    }
}

impl TryFrom<String> for DynamicProviderId {
    type Error = String;

    fn try_from(provider: String) -> Result<Self, Self::Error> {
        provider.parse()
    }
}

impl From<DynamicProviderId> for String {
    fn from(provider: DynamicProviderId) -> Self {
        provider.0.to_string()
    }
}

impl Serialize for DynamicProviderId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.0)
    }
}

impl<'de> Deserialize<'de> for DynamicProviderId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let provider = String::deserialize(deserializer)?;
        Self::parse(&provider)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown provider: {provider}")))
    }
}

pub fn is_known_dynamic_completion_provider(provider: &str) -> bool {
    DynamicProviderId::parse(provider).is_some()
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

    #[test]
    fn every_provider_round_trips_through_typed_id_and_json() {
        let ids = DynamicProviderId::all().collect::<Vec<_>>();
        assert_eq!(ids.len(), DYNAMIC_COMPLETION_PROVIDERS.len());

        for id in ids {
            assert_eq!(DynamicProviderId::parse(id.as_str()), Some(id));
            let json = serde_json::to_string(&id).unwrap();
            assert_eq!(json, format!("\"{}\"", id.as_str()));
            assert_eq!(
                serde_json::from_str::<DynamicProviderId>(&json).unwrap(),
                id
            );
        }
    }

    #[test]
    fn unknown_provider_is_rejected() {
        assert!("git.not-a-provider".parse::<DynamicProviderId>().is_err());
        assert!(serde_json::from_str::<DynamicProviderId>("\"git.not-a-provider\"").is_err());
    }
}
