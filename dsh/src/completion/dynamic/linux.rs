use super::{
    CachePolicy, CompletionContext, DynamicCompletionProvider, ParsedCommandLine, SystemdUnitQuery,
    dedup_sorted, parse_non_empty_lines, run_command_lines, selected_systemd_manager_scope,
    systemctl_unit_kind_for_subcommand,
};
use crate::completion::integrated::EnhancedCandidate;
use std::fs;
use std::path::{Path, PathBuf};

pub(super) fn collect(
    collector: &super::DynamicCompletionProvider,
    request: &super::registry::DynamicProviderRequest<'_>,
) -> Option<Vec<EnhancedCandidate>> {
    use super::*;

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

impl DynamicCompletionProvider {
    pub(crate) fn collect_systemctl_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        cache_policy: CachePolicy,
    ) -> Vec<EnhancedCandidate> {
        let cached_only = cache_policy.is_cached_only();
        if !matches!(
            parsed_command_line.completion_context,
            CompletionContext::SubCommand | CompletionContext::Argument { .. }
        ) {
            return Vec::new();
        }

        let Some(subcommand) = parsed_command_line
            .subcommand_path
            .first()
            .map(String::as_str)
        else {
            return Vec::new();
        };
        let kind = match systemctl_unit_kind_for_subcommand(subcommand) {
            Some(kind) => kind,
            _ => return Vec::new(),
        };

        self.collect_systemd_unit_candidates(
            current_dir,
            parsed_command_line.current_token.as_str(),
            SystemdUnitQuery::new(
                kind,
                selected_systemd_manager_scope(parsed_command_line),
                None,
            ),
            "systemd unit",
            cached_only,
        )
    }

    pub(crate) fn collect_ip_netns_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("ip");
        let current_dir = current_dir.to_path_buf();
        self.collect_cached_value_candidates(
            "ip",
            "network-namespace",
            current_dir.clone(),
            current_token,
            "network namespace",
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                Ok(parse_first_column_values(&run_command_lines(
                    &command_path,
                    &["netns", "list"],
                    &current_dir,
                )?))
            },
        )
    }

    pub(crate) fn collect_ip_route_table_candidates(
        &self,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let path = PathBuf::from("/etc/iproute2/rt_tables");
        self.collect_cached_value_candidates(
            "ip",
            "route-table",
            path.clone(),
            current_token,
            "IP route table",
            cached_only,
            move || Ok(load_ip_route_tables(&path)),
        )
    }

    pub(crate) fn collect_nft_table_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("nft");
        let current_dir = current_dir.to_path_buf();
        self.collect_cached_value_candidates(
            "nft",
            "table",
            current_dir.clone(),
            current_token,
            "nftables table",
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                Ok(parse_nft_tables(&run_command_lines(
                    &command_path,
                    &["list", "tables"],
                    &current_dir,
                )?))
            },
        )
    }

    pub(crate) fn collect_nft_chain_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("nft");
        let current_dir = current_dir.to_path_buf();
        self.collect_cached_value_candidates(
            "nft",
            "chain",
            current_dir.clone(),
            current_token,
            "nftables chain",
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                Ok(parse_nft_chains(&run_command_lines(
                    &command_path,
                    &["-a", "list", "ruleset"],
                    &current_dir,
                )?))
            },
        )
    }

    pub(crate) fn collect_lvm_physical_volume_candidates(
        &self,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("pvs");
        self.collect_cached_value_candidates(
            "lvm",
            "physical-volume",
            PathBuf::from("/"),
            current_token,
            "LVM physical volume",
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                Ok(parse_first_column_values(&run_command_lines(
                    &command_path,
                    &["--noheadings", "-o", "pv_name"],
                    Path::new("/"),
                )?))
            },
        )
    }

    pub(crate) fn collect_lvm_volume_group_candidates(
        &self,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("vgs");
        self.collect_cached_value_candidates(
            "lvm",
            "volume-group",
            PathBuf::from("/"),
            current_token,
            "LVM volume group",
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                Ok(parse_first_column_values(&run_command_lines(
                    &command_path,
                    &["--noheadings", "-o", "vg_name"],
                    Path::new("/"),
                )?))
            },
        )
    }

    pub(crate) fn collect_lvm_logical_volume_candidates(
        &self,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("lvs");
        self.collect_cached_value_candidates(
            "lvm",
            "logical-volume",
            PathBuf::from("/"),
            current_token,
            "LVM logical volume",
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                Ok(parse_lvm_logical_volumes(&run_command_lines(
                    &command_path,
                    &["--noheadings", "-o", "lv_path,vg_name,lv_name"],
                    Path::new("/"),
                )?))
            },
        )
    }

    pub(crate) fn collect_zfs_dataset_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("zfs");
        let current_dir = current_dir.to_path_buf();
        self.collect_cached_value_candidates(
            "zfs",
            "dataset",
            current_dir.clone(),
            current_token,
            "ZFS dataset",
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                Ok(parse_non_empty_lines(&run_command_lines(
                    &command_path,
                    &["list", "-H", "-o", "name"],
                    &current_dir,
                )?))
            },
        )
    }

    pub(crate) fn collect_zpool_pool_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("zpool");
        let current_dir = current_dir.to_path_buf();
        self.collect_cached_value_candidates(
            "zpool",
            "pool",
            current_dir.clone(),
            current_token,
            "ZFS pool",
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                Ok(parse_non_empty_lines(&run_command_lines(
                    &command_path,
                    &["list", "-H", "-o", "name"],
                    &current_dir,
                )?))
            },
        )
    }

    pub(crate) fn collect_btrfs_subvolume_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("btrfs");
        let current_dir = current_dir.to_path_buf();
        self.collect_cached_value_candidates(
            "btrfs",
            "subvolume",
            current_dir.clone(),
            current_token,
            "Btrfs subvolume",
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                let root = current_dir.to_str().unwrap_or(".");
                Ok(parse_btrfs_subvolumes(&run_command_lines(
                    &command_path,
                    &["subvolume", "list", "-o", root],
                    &current_dir,
                )?))
            },
        )
    }

    pub(crate) fn collect_mdadm_array_candidates(
        &self,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        self.collect_cached_value_candidates(
            "mdadm",
            "array",
            PathBuf::from("/proc/mdstat"),
            current_token,
            "mdraid array",
            cached_only,
            move || Ok(load_mdadm_arrays(Path::new("/proc/mdstat"))),
        )
    }

    pub(crate) fn collect_dmsetup_device_candidates(
        &self,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("dmsetup");
        self.collect_cached_value_candidates(
            "dmsetup",
            "device",
            PathBuf::from("/dev/mapper"),
            current_token,
            "device mapper name",
            cached_only,
            move || {
                let mut values = load_dev_mapper_devices(Path::new("/dev/mapper"));
                if let Some(command_path) = command_path
                    && let Ok(lines) = run_command_lines(&command_path, &["ls"], Path::new("/"))
                {
                    values.extend(parse_first_column_values(&lines));
                }
                Ok(dedup_sorted(values))
            },
        )
    }

    pub(crate) fn collect_audit_rule_key_candidates(
        &self,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        self.collect_cached_value_candidates(
            "audit",
            "rule-key",
            PathBuf::from("/etc/audit/rules.d"),
            current_token,
            "audit rule key",
            cached_only,
            move || Ok(load_audit_rule_keys(Path::new("/etc/audit/rules.d"))),
        )
    }

    pub(crate) fn collect_selinux_module_candidates(
        &self,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("semodule");
        self.collect_cached_value_candidates(
            "selinux",
            "module",
            PathBuf::from("/etc/selinux"),
            current_token,
            "SELinux module",
            cached_only,
            move || {
                let mut values = load_selinux_module_files(Path::new("/etc/selinux"));
                if let Some(command_path) = command_path
                    && let Ok(lines) = run_command_lines(&command_path, &["-l"], Path::new("/"))
                {
                    values.extend(parse_first_column_values(&lines));
                }
                Ok(dedup_sorted(values))
            },
        )
    }

    pub(crate) fn collect_journalctl_identifier_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("journalctl");
        let current_dir = current_dir.to_path_buf();
        self.collect_cached_value_candidates(
            "journalctl",
            "identifier",
            current_dir.clone(),
            current_token,
            "journal identifier",
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                Ok(parse_non_empty_lines(&run_command_lines(
                    &command_path,
                    &["--no-pager", "-F", "SYSLOG_IDENTIFIER"],
                    &current_dir,
                )?))
            },
        )
    }

    pub(crate) fn collect_machinectl_machine_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("machinectl");
        let current_dir = current_dir.to_path_buf();
        self.collect_cached_value_candidates(
            "machinectl",
            "machine",
            current_dir.clone(),
            current_token,
            "systemd machine",
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                Ok(parse_first_column_values(&run_command_lines(
                    &command_path,
                    &["list", "--no-legend", "--no-pager"],
                    &current_dir,
                )?))
            },
        )
    }

    pub(crate) fn collect_ufw_application_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("ufw");
        let current_dir = current_dir.to_path_buf();
        self.collect_cached_value_candidates(
            "ufw",
            "application",
            current_dir.clone(),
            current_token,
            "UFW application profile",
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                Ok(parse_ufw_applications(&run_command_lines(
                    &command_path,
                    &["app", "list"],
                    &current_dir,
                )?))
            },
        )
    }

    pub(crate) fn collect_wireless_device_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("iw");
        let current_dir = current_dir.to_path_buf();
        self.collect_cached_value_candidates(
            "iw",
            "wireless-device",
            current_dir.clone(),
            current_token,
            "wireless device",
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                Ok(parse_iw_devices(&run_command_lines(
                    &command_path,
                    &["dev"],
                    &current_dir,
                )?))
            },
        )
    }

    pub(crate) fn collect_login_shell_candidates(
        &self,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        self.collect_cached_value_candidates(
            "shells",
            "login-shell",
            PathBuf::from("/etc"),
            current_token,
            "login shell",
            cached_only,
            move || Ok(load_login_shells(Path::new("/etc/shells"))),
        )
    }

    pub(crate) fn collect_udev_subsystem_candidates(
        &self,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        self.collect_cached_value_candidates(
            "udevadm",
            "subsystem",
            PathBuf::from("/sys/class"),
            current_token,
            "device subsystem",
            cached_only,
            move || Ok(load_udev_subsystems(Path::new("/sys/class"))),
        )
    }

    pub(crate) fn collect_selinux_boolean_candidates(
        &self,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("getsebool");
        self.collect_cached_value_candidates(
            "getsebool",
            "boolean",
            PathBuf::from("/"),
            current_token,
            "SELinux boolean",
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                Ok(parse_selinux_booleans(&run_command_lines(
                    &command_path,
                    &["-a"],
                    Path::new("/"),
                )?))
            },
        )
    }
}

/// Extracts interface names from `iw dev`, whose device rows are indented
/// under each `phy#N` block as `Interface wlan0`.
fn parse_iw_devices(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| line.trim().strip_prefix("Interface "))
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .collect(),
    )
}

fn load_login_shells(path: &Path) -> Vec<String> {
    let Ok(contents) = fs::read_to_string(path) else {
        return Vec::new();
    };
    dedup_sorted(
        contents
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with('/'))
            .map(str::to_string)
            .collect(),
    )
}

fn load_udev_subsystems(path: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(path) else {
        return Vec::new();
    };
    dedup_sorted(
        entries
            .flatten()
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect(),
    )
}

/// Extracts boolean names from `getsebool -a`, which prints `name --> on`.
fn parse_selinux_booleans(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| line.split("-->").next())
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .collect(),
    )
}

fn load_ip_route_tables(path: &Path) -> Vec<String> {
    let Ok(contents) = fs::read_to_string(path) else {
        return vec![
            "default".to_string(),
            "main".to_string(),
            "local".to_string(),
        ];
    };
    let mut values = vec![
        "default".to_string(),
        "main".to_string(),
        "local".to_string(),
    ];
    for line in contents.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let _id = parts.next();
        if let Some(name) = parts.next()
            && is_simple_completion_value(name)
        {
            values.push(name.to_string());
        }
    }
    dedup_sorted(values)
}

fn parse_nft_tables(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| {
                let mut parts = line.split_whitespace();
                (parts.next()? == "table").then_some(())?;
                let _family = parts.next()?;
                parts.next().map(str::to_string)
            })
            .collect(),
    )
}

fn parse_nft_chains(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| {
                let mut parts = line.split_whitespace();
                (parts.next()? == "chain").then_some(())?;
                parts.next().map(str::to_string)
            })
            .collect(),
    )
}

fn parse_first_column_values(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| line.split_whitespace().next().map(str::to_string))
            .collect(),
    )
}

fn parse_ufw_applications(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty() && !line.ends_with(':'))
            .map(str::to_string)
            .collect(),
    )
}

fn parse_lvm_logical_volumes(lines: &[String]) -> Vec<String> {
    let mut values = Vec::new();
    for line in lines {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if let Some(path) = fields.first()
            && path.starts_with('/')
        {
            values.push((*path).to_string());
        }
        if fields.len() >= 3 {
            values.push(format!("{}/{}", fields[1], fields[2]));
            values.push(fields[2].to_string());
        }
    }
    dedup_sorted(values)
}

fn parse_btrfs_subvolumes(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| {
                line.split_once(" path ")
                    .map(|(_, path)| path.trim().to_string())
            })
            .collect(),
    )
}

fn load_mdadm_arrays(path: &Path) -> Vec<String> {
    let Ok(contents) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut values = Vec::new();
    for line in contents.lines() {
        let Some((name, _rest)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if name.starts_with("md") && is_simple_completion_value(name) {
            values.push(name.to_string());
            values.push(format!("/dev/{name}"));
        }
    }
    dedup_sorted(values)
}

fn load_dev_mapper_devices(dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut values = Vec::new();
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if name == "control" || !is_simple_completion_value(&name) {
            continue;
        }
        values.push(name.clone());
        values.push(format!("/dev/mapper/{name}"));
    }
    dedup_sorted(values)
}

fn load_audit_rule_keys(dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut values = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("rules") {
            continue;
        }
        let Ok(contents) = fs::read_to_string(path) else {
            continue;
        };
        values.extend(parse_audit_rule_keys(&contents));
    }
    dedup_sorted(values)
}

fn parse_audit_rule_keys(contents: &str) -> Vec<String> {
    let mut values = Vec::new();
    for line in contents.lines() {
        let mut parts = line.split_whitespace().peekable();
        while let Some(part) = parts.next() {
            if part == "-k" {
                if let Some(value) = parts.peek()
                    && is_simple_completion_value(value)
                {
                    values.push((*value).to_string());
                }
            } else if let Some(value) = part.strip_prefix("-k") {
                if is_simple_completion_value(value) {
                    values.push(value.to_string());
                }
            } else if let Some(value) = part.strip_prefix("key=")
                && is_simple_completion_value(value)
            {
                values.push(value.to_string());
            }
        }
    }
    dedup_sorted(values)
}

fn load_selinux_module_files(root: &Path) -> Vec<String> {
    let mut values = Vec::new();
    collect_selinux_module_files(root, 0, &mut values);
    dedup_sorted(values)
}

fn collect_selinux_module_files(dir: &Path, depth: usize, values: &mut Vec<String>) {
    if depth > 5 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_selinux_module_files(&path, depth + 1, values);
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("cil" | "pp")
        ) && is_simple_completion_value(stem)
        {
            values.push(stem.to_string());
        }
    }
}

fn is_simple_completion_value(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | ':'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn linux_parsers_read_static_inventory_sources() {
        let dir = tempdir().unwrap();
        let rt_tables = dir.path().join("rt_tables");
        fs::write(&rt_tables, "255 local\n254 main\n100 custom\n# comment\n").unwrap();
        assert_eq!(
            load_ip_route_tables(&rt_tables),
            vec![
                "custom".to_string(),
                "default".to_string(),
                "local".to_string(),
                "main".to_string(),
            ]
        );

        let mdstat = dir.path().join("mdstat");
        fs::write(
            &mdstat,
            "md0 : active raid1 sda1[0] sdb1[1]\nunused devices: <none>\n",
        )
        .unwrap();
        assert_eq!(
            load_mdadm_arrays(&mdstat),
            vec!["/dev/md0".to_string(), "md0".to_string()]
        );
    }

    #[test]
    fn linux_parsers_extract_command_output_values() {
        assert_eq!(
            parse_nft_tables(&["table inet filter".to_string(), "table ip nat".to_string(),]),
            vec!["filter".to_string(), "nat".to_string()]
        );
        assert_eq!(
            parse_nft_chains(&["chain input { # handle 1".to_string()]),
            vec!["input".to_string()]
        );
        assert_eq!(
            parse_lvm_logical_volumes(&["/dev/vg0/root vg0 root".to_string()]),
            vec![
                "/dev/vg0/root".to_string(),
                "root".to_string(),
                "vg0/root".to_string(),
            ]
        );
        assert_eq!(
            parse_btrfs_subvolumes(&["ID 256 gen 12 top level 5 path home".to_string()]),
            vec!["home".to_string()]
        );
        assert_eq!(
            parse_ufw_applications(&[
                "Available applications:".to_string(),
                "  Nginx Full".to_string(),
                "  OpenSSH".to_string(),
            ]),
            vec!["Nginx Full".to_string(), "OpenSSH".to_string()]
        );
    }

    #[test]
    fn wireless_and_host_inventory_parsers_read_local_sources() {
        assert_eq!(
            parse_iw_devices(&[
                "phy#0".to_string(),
                "\tUnnamed/non-netdev interface".to_string(),
                "\t\ttype P2P-device".to_string(),
                "\tInterface wlan0".to_string(),
            ]),
            vec!["wlan0".to_string()]
        );
        assert_eq!(
            parse_selinux_booleans(&[
                "httpd_can_network_connect --> off".to_string(),
                "samba_enable_home_dirs --> on".to_string(),
            ]),
            vec![
                "httpd_can_network_connect".to_string(),
                "samba_enable_home_dirs".to_string()
            ]
        );

        let dir = tempdir().unwrap();
        let shells = dir.path().join("shells");
        fs::write(&shells, "# /etc/shells\n/bin/sh\n/usr/bin/fish\n\n").unwrap();
        assert_eq!(
            load_login_shells(&shells),
            vec!["/bin/sh".to_string(), "/usr/bin/fish".to_string()]
        );

        let class = dir.path().join("class");
        fs::create_dir_all(class.join("net")).unwrap();
        fs::create_dir_all(class.join("block")).unwrap();
        assert_eq!(
            load_udev_subsystems(&class),
            vec!["block".to_string(), "net".to_string()]
        );
    }

    #[test]
    fn audit_and_selinux_parsers_read_local_config() {
        assert_eq!(
            parse_audit_rule_keys("-w /etc/passwd -p wa -k identity\n-a always,exit -F key=exec\n"),
            vec!["exec".to_string(), "identity".to_string()]
        );

        let dir = tempdir().unwrap();
        let modules = dir.path().join("targeted").join("active").join("modules");
        fs::create_dir_all(&modules).unwrap();
        fs::write(modules.join("ssh.cil"), "").unwrap();
        fs::write(modules.join("web.pp"), "").unwrap();
        assert_eq!(
            load_selinux_module_files(dir.path()),
            vec!["ssh".to_string(), "web".to_string()]
        );
    }
}
