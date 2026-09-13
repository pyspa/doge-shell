//! Completion for Linux system administration: systemd, netfilter, SELinux,
//! audit, pacman and snapper.
//!
//! This module and the Linux loaders in `dynamic.rs` are the only places
//! allowed to read a source that exists on one platform only, and
//! `scripts/portability-allowlist.txt` pins every such literal. Nothing here is
//! compiled out on macOS: both dispatch routes below run unconditionally, and
//! each one finds its file or command missing and returns no candidates. That
//! silence is the intended behaviour -- these are completions for tools macOS
//! does not have -- and it is the one place in the tree where "empty on the
//! other platform" is correct rather than a porting gap.
//!
//! Two routes reach this file, and a provider uses exactly one of them:
//!
//! - `LOCAL_SPECS` below, for the fixed-shape providers. `local::collect` reads
//!   these rows straight from `registry::ProviderRegistration::collect`, so they
//!   never reach `collector_for`/`family_for` at all -- their `ProviderFamily`
//!   classification is irrelevant, which is why four rows here
//!   (`machinectl.machine`, `ufw.application`, `audit.rule_key`, `mdadm.array`)
//!   are classified `External` yet live in this table: a row belongs to the file
//!   that owns the parser or loader it names, not to its family.
//! - `collect` below, reached via `registry::collector_for(ProviderFamily::Linux)`,
//!   for the providers that need runtime-built arguments or multi-source merges.
//!
//! See docs/ai/skills/doge-shell-repo/references/platform-support.md, and
//! `local.rs` for what makes a provider "fixed shape".

use super::{
    CachePolicy, CompletionContext, DynamicCompletionProvider, ParsedCommandLine, SystemdUnitQuery,
    completion_words, dedup_sorted, parse_non_empty_lines, run_command_lines, run_command_stdout,
    selected_systemd_manager_scope, systemctl_unit_kind_for_subcommand,
};
use crate::completion::integrated::EnhancedCandidate;
use std::fs;
use std::path::{Path, PathBuf};

mod collectors;
mod parsers;
mod specs;
use parsers::*;
pub(super) use specs::LOCAL_SPECS;

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
        "wireguard.config" => {
            collector.collect_wireguard_config_candidates(current_dir, current_token)
        }
        "sysctl.key" => collector.collect_sysctl_key_candidates(current_token, cached_only),
        "system.process_name" => collector.collect_process_name_candidates(
            parsed_command_line,
            "system",
            request.cache_policy,
        ),
        "system.process_pid" => {
            collector.collect_process_pid_candidates(parsed_command_line, cached_only)
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
        "mkinitcpio.preset" => {
            collector.collect_mkinitcpio_preset_candidates(current_token, cached_only)
        }
        "selinux.module" => collector.collect_selinux_module_candidates(current_token, cached_only),
        "system.owner_group" => {
            collector.collect_owner_group_candidates(current_token, cached_only)
        }
        "snapper.snapshot" => collector.collect_snapper_snapshot_candidates(
            parsed_command_line,
            current_dir,
            current_token,
            cached_only,
        ),
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

    #[test]
    fn arch_and_snapper_inventory_parsers_use_local_sources() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("linux.preset"), "").unwrap();
        fs::write(dir.path().join("fallback.preset"), "").unwrap();
        fs::write(dir.path().join("README"), "").unwrap();

        assert_eq!(
            load_file_stems(dir.path(), ".preset"),
            vec!["fallback".to_string(), "linux".to_string()]
        );
        assert_eq!(
            load_file_names(dir.path()),
            vec![
                "README".to_string(),
                "fallback.preset".to_string(),
                "linux.preset".to_string(),
            ]
        );
        assert_eq!(
            parse_snapper_snapshot_json(
                r#"{"configs":[{"snapshots":[{"number":1},{"number":"42"}]}]}"#
            ),
            vec!["1".to_string(), "42".to_string()]
        );
        assert!(parse_snapper_snapshot_json("not-json").is_empty());
    }

    #[test]
    fn snapper_config_parser_supports_separate_and_inline_options() {
        use crate::completion::parser::CommandLineParser;

        for (input, expected) in [
            ("snapper --config home delete ", "home"),
            ("snapper --config=home delete ", "home"),
            ("snapper -chome delete ", "home"),
        ] {
            let parsed = CommandLineParser::new().parse(input, input.len());
            assert_eq!(selected_snapper_config(&parsed), Some(expected), "{input}");
        }

        assert_eq!(
            snapper_snapshot_filter("1..4"),
            (Some("1..".to_string()), "4".to_string())
        );
        assert_eq!(
            snapper_snapshot_filter("1-"),
            (Some("1-".to_string()), String::new())
        );
        assert_eq!(snapper_snapshot_filter("42"), (None, "42".to_string()));
    }
}
