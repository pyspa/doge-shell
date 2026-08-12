pub(in super::super) fn collect(
    collector: &super::super::DynamicCompletionProvider,
    request: &super::super::registry::DynamicProviderRequest<'_>,
) -> Option<Vec<super::super::EnhancedCandidate>> {
    use super::super::*;

    let provider = request.provider.as_str();
    let scope = request.scope;
    let parsed_command_line = request.parsed_command_line;
    let current_dir = request.current_dir;
    let cached_only = request.cache_policy.is_cached_only();
    let current_token = parsed_command_line.current_token.as_str();

    Some(match provider {
        "block.device" => {
            collector.collect_block_device_candidates(current_dir, current_token, cached_only)
        }
        "block.label" => collector.collect_blkid_attribute_candidates(
            current_dir,
            current_token,
            "LABEL",
            "block label",
            cached_only,
        ),
        "block.uuid" => collector.collect_blkid_attribute_candidates(
            current_dir,
            current_token,
            "UUID",
            "block uuid",
            cached_only,
        ),
        "dbus.service" => {
            collector.collect_dbus_service_candidates(current_dir, current_token, cached_only)
        }
        "systemctl.unit" => {
            let kind = systemctl_unit_kind_for_context(parsed_command_line);
            collector.collect_systemd_unit_candidates(
                current_dir,
                current_token,
                SystemdUnitQuery::new(
                    kind,
                    selected_systemd_manager_scope(parsed_command_line),
                    systemd_unit_type_filter(scope),
                ),
                "systemd unit",
                cached_only,
            )
        }
        "systemctl.unit_file" => collector.collect_systemd_unit_candidates(
            current_dir,
            current_token,
            SystemdUnitQuery::new(
                SystemdUnitListKind::UnitFiles,
                selected_systemd_manager_scope(parsed_command_line),
                systemd_unit_type_filter(scope),
            ),
            "systemd unit file",
            cached_only,
        ),
        "journalctl.boot" => {
            collector.collect_journalctl_boot_candidates(current_dir, current_token, cached_only)
        }
        "journalctl.identifier" => collector.collect_journalctl_identifier_candidates(
            current_dir,
            current_token,
            cached_only,
        ),
        "firewalld.zone" => {
            collector.collect_firewalld_zone_candidates(current_dir, current_token, cached_only)
        }
        "firewalld.service" => {
            collector.collect_firewalld_service_candidates(current_dir, current_token, cached_only)
        }
        "firewalld.icmp_type" => collector.collect_firewalld_icmp_type_candidates(
            current_dir,
            current_token,
            cached_only,
        ),
        "networkctl.link" => {
            collector.collect_networkctl_link_candidates(current_dir, current_token, cached_only)
        }
        "ipset.set" => {
            collector.collect_ipset_set_candidates(current_dir, current_token, cached_only)
        }
        "wireguard.interface" => collector.collect_wireguard_interface_candidates(
            current_dir,
            current_token,
            cached_only,
        ),
        "wireguard.config" => {
            collector.collect_wireguard_config_candidates(current_dir, current_token)
        }
        "fstab.mountpoint" => {
            collector.collect_fstab_mountpoint_candidates(current_token, cached_only)
        }
        "localectl.keymap" => {
            collector.collect_localectl_keymap_candidates(current_dir, current_token, cached_only)
        }
        "localectl.locale" => {
            collector.collect_localectl_locale_candidates(current_dir, current_token, cached_only)
        }
        "loginctl.seat" => {
            collector.collect_loginctl_seat_candidates(current_dir, current_token, cached_only)
        }
        "loginctl.session" => {
            collector.collect_loginctl_session_candidates(current_dir, current_token, cached_only)
        }
        "loop.device" => {
            collector.collect_loop_device_candidates(current_dir, current_token, cached_only)
        }
        "sysctl.key" => collector.collect_sysctl_key_candidates(current_token, cached_only),
        "swap.device" => collector.collect_swap_device_candidates(current_token, cached_only),
        "system.process_name" => collector.collect_process_name_candidates(
            parsed_command_line,
            "system",
            request.cache_policy,
        ),
        "system.process_pid" => {
            collector.collect_process_pid_candidates(parsed_command_line, cached_only)
        }
        "timedatectl.timezone" => collector.collect_timedatectl_timezone_candidates(
            current_dir,
            current_token,
            cached_only,
        ),
        "tmux.session" => {
            collector.collect_tmux_session_candidates(current_dir, current_token, cached_only)
        }
        "screen.session" => {
            collector.collect_screen_session_candidates(current_dir, current_token, cached_only)
        }
        "nmcli.connection" => collector.collect_nmcli_value_candidates(
            current_dir,
            current_token,
            NmcliCompletionSpec {
                kind: "connection",
                args: &["-t", "-f", "NAME", "connection", "show"],
                description: "NetworkManager connection",
                parser: parse_nmcli_first_field,
            },
            cached_only,
        ),
        "nmcli.device" => collector.collect_nmcli_value_candidates(
            current_dir,
            current_token,
            NmcliCompletionSpec {
                kind: "device",
                args: &["-t", "-f", "DEVICE", "device"],
                description: "NetworkManager device",
                parser: parse_nmcli_first_field,
            },
            cached_only,
        ),
        "mount.mountpoint" => {
            collector.collect_mountpoint_candidates(current_dir, current_token, cached_only)
        }
        "kernel.module" => {
            collector.collect_kernel_module_candidates(scope, current_token, cached_only)
        }
        "ip.netns" => {
            collector.collect_ip_netns_candidates(current_dir, current_token, cached_only)
        }
        "ip.route_table" => collector.collect_ip_route_table_candidates(current_token, cached_only),
        "lvm.logical_volume" => {
            collector.collect_lvm_logical_volume_candidates(current_token, cached_only)
        }
        "lvm.physical_volume" => {
            collector.collect_lvm_physical_volume_candidates(current_token, cached_only)
        }
        "lvm.volume_group" => {
            collector.collect_lvm_volume_group_candidates(current_token, cached_only)
        }
        "nft.chain" => {
            collector.collect_nft_chain_candidates(current_dir, current_token, cached_only)
        }
        "nft.table" => {
            collector.collect_nft_table_candidates(current_dir, current_token, cached_only)
        }
        "selinux.module" => collector.collect_selinux_module_candidates(current_token, cached_only),
        "system.owner_group" => {
            collector.collect_owner_group_candidates(current_token, cached_only)
        }
        "wireless.device" => {
            collector.collect_wireless_device_candidates(current_dir, current_token, cached_only)
        }
        "login.shell" => collector.collect_login_shell_candidates(current_token, cached_only),
        "udev.subsystem" => collector.collect_udev_subsystem_candidates(current_token, cached_only),
        "selinux.boolean" => {
            collector.collect_selinux_boolean_candidates(current_token, cached_only)
        }
        "zfs.dataset" => {
            collector.collect_zfs_dataset_candidates(current_dir, current_token, cached_only)
        }
        "zpool.pool" => {
            collector.collect_zpool_pool_candidates(current_dir, current_token, cached_only)
        }
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
