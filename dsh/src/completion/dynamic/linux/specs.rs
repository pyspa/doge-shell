//! This family's `LocalSpec` rows for `local::collect`. Table only: which
//! family's table a provider's row lives in does not affect routing, so a fixed
//! shape belongs here rather than in `collect`'s match.
use super::*;

/// This family's rows for `local::collect` - see `local` for what belongs
/// here. Table only; routing is unaffected by which family's table a
/// provider's row lives in.
pub(crate) const LOCAL_SPECS: &[crate::completion::dynamic::local::LocalSpec] = &[
    crate::completion::dynamic::local::LocalSpec {
        provider: "ip.netns",
        command_name: "ip",
        value_kind: "network-namespace",
        scope: crate::completion::dynamic::local::Scope::CurrentDir,
        source: crate::completion::dynamic::local::Source::Lines {
            executable: "ip",
            args: &["netns", "list"],
            parser: parse_first_column_values,
        },
        description: "network namespace",
    },
    crate::completion::dynamic::local::LocalSpec {
        provider: "nft.table",
        command_name: "nft",
        value_kind: "table",
        scope: crate::completion::dynamic::local::Scope::CurrentDir,
        source: crate::completion::dynamic::local::Source::Lines {
            executable: "nft",
            args: &["list", "tables"],
            parser: parse_nft_tables,
        },
        description: "nftables table",
    },
    crate::completion::dynamic::local::LocalSpec {
        provider: "nft.chain",
        command_name: "nft",
        value_kind: "chain",
        scope: crate::completion::dynamic::local::Scope::CurrentDir,
        source: crate::completion::dynamic::local::Source::Lines {
            executable: "nft",
            args: &["-a", "list", "ruleset"],
            parser: parse_nft_chains,
        },
        description: "nftables chain",
    },
    crate::completion::dynamic::local::LocalSpec {
        provider: "lvm.physical_volume",
        command_name: "lvm",
        value_kind: "physical-volume",
        scope: crate::completion::dynamic::local::Scope::FixedCwd("/"),
        source: crate::completion::dynamic::local::Source::Lines {
            executable: "pvs",
            args: &["--noheadings", "-o", "pv_name"],
            parser: parse_first_column_values,
        },
        description: "LVM physical volume",
    },
    crate::completion::dynamic::local::LocalSpec {
        provider: "lvm.volume_group",
        command_name: "lvm",
        value_kind: "volume-group",
        scope: crate::completion::dynamic::local::Scope::FixedCwd("/"),
        source: crate::completion::dynamic::local::Source::Lines {
            executable: "vgs",
            args: &["--noheadings", "-o", "vg_name"],
            parser: parse_first_column_values,
        },
        description: "LVM volume group",
    },
    crate::completion::dynamic::local::LocalSpec {
        provider: "lvm.logical_volume",
        command_name: "lvm",
        value_kind: "logical-volume",
        scope: crate::completion::dynamic::local::Scope::FixedCwd("/"),
        source: crate::completion::dynamic::local::Source::Lines {
            executable: "lvs",
            args: &["--noheadings", "-o", "lv_path,vg_name,lv_name"],
            parser: parse_lvm_logical_volumes,
        },
        description: "LVM logical volume",
    },
    crate::completion::dynamic::local::LocalSpec {
        provider: "zfs.dataset",
        command_name: "zfs",
        value_kind: "dataset",
        scope: crate::completion::dynamic::local::Scope::CurrentDir,
        source: crate::completion::dynamic::local::Source::Lines {
            executable: "zfs",
            args: &["list", "-H", "-o", "name"],
            parser: parse_non_empty_lines,
        },
        description: "ZFS dataset",
    },
    crate::completion::dynamic::local::LocalSpec {
        provider: "zpool.pool",
        command_name: "zpool",
        value_kind: "pool",
        scope: crate::completion::dynamic::local::Scope::CurrentDir,
        source: crate::completion::dynamic::local::Source::Lines {
            executable: "zpool",
            args: &["list", "-H", "-o", "name"],
            parser: parse_non_empty_lines,
        },
        description: "ZFS pool",
    },
    crate::completion::dynamic::local::LocalSpec {
        provider: "journalctl.identifier",
        command_name: "journalctl",
        value_kind: "identifier",
        scope: crate::completion::dynamic::local::Scope::CurrentDir,
        source: crate::completion::dynamic::local::Source::Lines {
            executable: "journalctl",
            args: &["--no-pager", "-F", "SYSLOG_IDENTIFIER"],
            parser: parse_non_empty_lines,
        },
        description: "journal identifier",
    },
    crate::completion::dynamic::local::LocalSpec {
        provider: "machinectl.machine",
        command_name: "machinectl",
        value_kind: "machine",
        scope: crate::completion::dynamic::local::Scope::CurrentDir,
        source: crate::completion::dynamic::local::Source::Lines {
            executable: "machinectl",
            args: &["list", "--no-legend", "--no-pager"],
            parser: parse_first_column_values,
        },
        description: "systemd machine",
    },
    crate::completion::dynamic::local::LocalSpec {
        provider: "ufw.application",
        command_name: "ufw",
        value_kind: "application",
        scope: crate::completion::dynamic::local::Scope::CurrentDir,
        source: crate::completion::dynamic::local::Source::Lines {
            executable: "ufw",
            args: &["app", "list"],
            parser: parse_ufw_applications,
        },
        description: "UFW application profile",
    },
    crate::completion::dynamic::local::LocalSpec {
        provider: "wireless.device",
        command_name: "iw",
        value_kind: "wireless-device",
        scope: crate::completion::dynamic::local::Scope::CurrentDir,
        source: crate::completion::dynamic::local::Source::Lines {
            executable: "iw",
            args: &["dev"],
            parser: parse_iw_devices,
        },
        description: "wireless device",
    },
    crate::completion::dynamic::local::LocalSpec {
        provider: "selinux.boolean",
        command_name: "getsebool",
        value_kind: "boolean",
        scope: crate::completion::dynamic::local::Scope::FixedCwd("/"),
        source: crate::completion::dynamic::local::Source::Lines {
            executable: "getsebool",
            args: &["-a"],
            parser: parse_selinux_booleans,
        },
        description: "SELinux boolean",
    },
    crate::completion::dynamic::local::LocalSpec {
        provider: "pacman.repository",
        command_name: "pacman-conf",
        value_kind: "repository",
        scope: crate::completion::dynamic::local::Scope::Fixed("/etc/pacman.conf"),
        source: crate::completion::dynamic::local::Source::Lines {
            executable: "pacman-conf",
            args: &["--repo-list"],
            parser: parse_non_empty_lines,
        },
        description: "pacman repository",
    },
    crate::completion::dynamic::local::LocalSpec {
        provider: "ip.route_table",
        command_name: "ip",
        value_kind: "route-table",
        scope: crate::completion::dynamic::local::Scope::Fixed("/etc/iproute2/rt_tables"),
        source: crate::completion::dynamic::local::Source::ScopePath {
            loader: load_ip_route_tables,
        },
        description: "IP route table",
    },
    crate::completion::dynamic::local::LocalSpec {
        provider: "mdadm.array",
        command_name: "mdadm",
        value_kind: "array",
        scope: crate::completion::dynamic::local::Scope::Fixed("/proc/mdstat"),
        source: crate::completion::dynamic::local::Source::ScopePath {
            loader: load_mdadm_arrays,
        },
        description: "mdraid array",
    },
    crate::completion::dynamic::local::LocalSpec {
        provider: "audit.rule_key",
        command_name: "audit",
        value_kind: "rule-key",
        scope: crate::completion::dynamic::local::Scope::Fixed("/etc/audit/rules.d"),
        source: crate::completion::dynamic::local::Source::ScopePath {
            loader: load_audit_rule_keys,
        },
        description: "audit rule key",
    },
    crate::completion::dynamic::local::LocalSpec {
        provider: "login.shell",
        command_name: "shells",
        value_kind: "login-shell",
        scope: crate::completion::dynamic::local::Scope::Fixed("/etc"),
        source: crate::completion::dynamic::local::Source::Path {
            path: "/etc/shells",
            loader: load_login_shells,
        },
        description: "login shell",
    },
    crate::completion::dynamic::local::LocalSpec {
        provider: "udev.subsystem",
        command_name: "udevadm",
        value_kind: "subsystem",
        scope: crate::completion::dynamic::local::Scope::Fixed("/sys/class"),
        source: crate::completion::dynamic::local::Source::ScopePath {
            loader: load_udev_subsystems,
        },
        description: "device subsystem",
    },
    crate::completion::dynamic::local::LocalSpec {
        provider: "snapper.config",
        command_name: "snapper",
        value_kind: "config",
        scope: crate::completion::dynamic::local::Scope::Fixed("/etc/snapper/configs"),
        source: crate::completion::dynamic::local::Source::ScopePath {
            loader: load_file_names,
        },
        description: "snapper configuration",
    },
];
