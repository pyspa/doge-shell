pub(in super::super) fn collect(
    collector: &super::super::DynamicCompletionProvider,
    request: &super::super::registry::DynamicProviderRequest<'_>,
) -> Option<Vec<super::super::EnhancedCandidate>> {
    use super::super::*;

    let provider = request.provider.as_str();
    let parsed_command_line = request.parsed_command_line;
    let current_dir = request.current_dir;
    let cached_only = request.cache_policy.is_cached_only();
    let current_token = parsed_command_line.current_token.as_str();

    Some(match provider {
        "brew.installed" => collector.collect_brew_installed_candidates(current_token, cached_only),
        "ollama.model" => collector.collect_ollama_model_candidates(current_token, cached_only),
        "apt.installed_package" => collector.collect_apt_installed_package_candidates(
            current_dir,
            current_token,
            parsed_command_line.command.as_str(),
            cached_only,
        ),
        "apk.installed_package" => collector.collect_apk_installed_package_candidates(
            current_dir,
            current_token,
            cached_only,
        ),
        "dnf.installed_package" => collector.collect_rpm_installed_package_candidates(
            current_dir,
            current_token,
            "dnf",
            cached_only,
        ),
        "rpm.installed_package" => collector.collect_rpm_installed_package_candidates(
            current_dir,
            current_token,
            "rpm",
            cached_only,
        ),
        "zypper.installed_package" => collector.collect_zypper_installed_package_candidates(
            current_dir,
            current_token,
            cached_only,
        ),
        "machinectl.machine" => {
            collector.collect_machinectl_machine_candidates(current_dir, current_token, cached_only)
        }
        "ufw.application" => {
            collector.collect_ufw_application_candidates(current_dir, current_token, cached_only)
        }
        "pacman.package" => collector.collect_pacman_package_candidates(
            current_dir,
            current_token,
            matches!(
                parsed_command_line
                    .subcommand_path
                    .first()
                    .map(String::as_str)
                    .or_else(|| parsed_command_line.raw_args.first().map(String::as_str)),
                Some("-S")
            ),
            cached_only,
        ),
        "audit.rule_key" => collector.collect_audit_rule_key_candidates(current_token, cached_only),
        "ansible.inventory_host" => collector.collect_ansible_inventory_host_candidates(
            parsed_command_line,
            current_dir,
            current_token,
            cached_only,
        ),
        "aws.profile" => collector.collect_aws_profile_candidates(current_token, cached_only),
        "az.subscription" => {
            collector.collect_az_subscription_candidates(current_token, cached_only)
        }
        "gcloud.configuration" => {
            collector.collect_gcloud_configuration_candidates(current_token, cached_only)
        }
        "gcloud.project" => collector.collect_gcloud_project_candidates(current_token, cached_only),
        "btrfs.subvolume" => {
            collector.collect_btrfs_subvolume_candidates(current_dir, current_token, cached_only)
        }
        "dmsetup.device" => collector.collect_dmsetup_device_candidates(current_token, cached_only),
        "mdadm.array" => collector.collect_mdadm_array_candidates(current_token, cached_only),
        _ => {
            return platform::collect(
                collector,
                provider,
                parsed_command_line,
                current_dir,
                cached_only,
            );
        }
    })
}
