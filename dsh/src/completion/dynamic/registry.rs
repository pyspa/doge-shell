use super::{
    CachePolicy, DynamicCompletionProvider, container, dev, external, git, kubernetes, linux,
    local, project,
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
        // Fixed-shape providers (a fixed executable + fixed args + a parser,
        // or a fixed path + a loader) are answered from the `local` spec
        // tables before the family collector runs, so adding one is a data
        // row rather than a wrapper method plus a `match` arm. `family_for`
        // still classifies every id - the row lives in a family module's
        // table - it just does not have to route these.
        if let Some(candidates) = local::collect(provider, request) {
            return Some(candidates);
        }
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

/// Which family collector owns a provider that is *not* answered from a
/// `local::LocalSpec` row.
///
/// Only the providers that still need a family collector appear here. A
/// provider with a table row never reaches `collector_for`
/// (`ProviderRegistration::collect` consults `local` first), so classifying it
/// would be dead weight that also implies an ownership the family module does
/// not have - `a_table_driven_provider_never_also_has_a_family_arm` in
/// `local.rs` enforces that the two routes stay disjoint. A misclassification
/// here is not silent either: the unconditional `External` fallthrough sends
/// the provider to `platform::collect`, which returns `None` for a local id,
/// and `every_registered_provider_has_a_dispatch_arm` fails.
fn family_for(provider: &str) -> ProviderFamily {
    if provider.starts_with("git.") {
        ProviderFamily::Git
    } else if provider.starts_with("docker.") || provider.starts_with("podman.") {
        ProviderFamily::Container
    } else if provider.starts_with("kubectl.") || provider.starts_with("helm.") {
        ProviderFamily::Kubernetes
    } else if matches!(
        provider.split_once('.').map(|(prefix, _)| prefix),
        Some(
            "block"
                | "kernel"
                | "mkinitcpio"
                | "mount"
                | "nmcli"
                | "selinux"
                | "snapper"
                | "sysctl"
                | "system"
                | "systemctl"
                | "wireguard"
        )
    ) {
        ProviderFamily::Linux
    } else if matches!(
        provider.split_once('.').map(|(prefix, _)| prefix),
        Some(
            "bacon"
                | "cargo"
                | "ghq"
                | "go"
                | "hatch"
                | "jj"
                | "js"
                | "maven"
                | "meson"
                | "node"
                | "nox"
                | "pdm"
                | "pip"
                | "pipenv"
                | "pre_commit"
                | "python"
                | "terraform"
                | "tox"
        )
    ) {
        ProviderFamily::Development
    } else if matches!(
        provider,
        "project.task"
            | "archive.entry"
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
