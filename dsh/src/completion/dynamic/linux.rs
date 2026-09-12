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

/// This family's rows for `local::collect` - see `local` for what belongs
/// here. Table only; routing is unaffected by which family's table a
/// provider's row lives in.
pub(super) const LOCAL_SPECS: &[super::local::LocalSpec] = &[
    super::local::LocalSpec {
        provider: "ip.netns",
        command_name: "ip",
        value_kind: "network-namespace",
        scope: super::local::Scope::CurrentDir,
        source: super::local::Source::Lines {
            executable: "ip",
            args: &["netns", "list"],
            parser: parse_first_column_values,
        },
        description: "network namespace",
    },
    super::local::LocalSpec {
        provider: "nft.table",
        command_name: "nft",
        value_kind: "table",
        scope: super::local::Scope::CurrentDir,
        source: super::local::Source::Lines {
            executable: "nft",
            args: &["list", "tables"],
            parser: parse_nft_tables,
        },
        description: "nftables table",
    },
    super::local::LocalSpec {
        provider: "nft.chain",
        command_name: "nft",
        value_kind: "chain",
        scope: super::local::Scope::CurrentDir,
        source: super::local::Source::Lines {
            executable: "nft",
            args: &["-a", "list", "ruleset"],
            parser: parse_nft_chains,
        },
        description: "nftables chain",
    },
    super::local::LocalSpec {
        provider: "lvm.physical_volume",
        command_name: "lvm",
        value_kind: "physical-volume",
        scope: super::local::Scope::FixedCwd("/"),
        source: super::local::Source::Lines {
            executable: "pvs",
            args: &["--noheadings", "-o", "pv_name"],
            parser: parse_first_column_values,
        },
        description: "LVM physical volume",
    },
    super::local::LocalSpec {
        provider: "lvm.volume_group",
        command_name: "lvm",
        value_kind: "volume-group",
        scope: super::local::Scope::FixedCwd("/"),
        source: super::local::Source::Lines {
            executable: "vgs",
            args: &["--noheadings", "-o", "vg_name"],
            parser: parse_first_column_values,
        },
        description: "LVM volume group",
    },
    super::local::LocalSpec {
        provider: "lvm.logical_volume",
        command_name: "lvm",
        value_kind: "logical-volume",
        scope: super::local::Scope::FixedCwd("/"),
        source: super::local::Source::Lines {
            executable: "lvs",
            args: &["--noheadings", "-o", "lv_path,vg_name,lv_name"],
            parser: parse_lvm_logical_volumes,
        },
        description: "LVM logical volume",
    },
    super::local::LocalSpec {
        provider: "zfs.dataset",
        command_name: "zfs",
        value_kind: "dataset",
        scope: super::local::Scope::CurrentDir,
        source: super::local::Source::Lines {
            executable: "zfs",
            args: &["list", "-H", "-o", "name"],
            parser: parse_non_empty_lines,
        },
        description: "ZFS dataset",
    },
    super::local::LocalSpec {
        provider: "zpool.pool",
        command_name: "zpool",
        value_kind: "pool",
        scope: super::local::Scope::CurrentDir,
        source: super::local::Source::Lines {
            executable: "zpool",
            args: &["list", "-H", "-o", "name"],
            parser: parse_non_empty_lines,
        },
        description: "ZFS pool",
    },
    super::local::LocalSpec {
        provider: "journalctl.identifier",
        command_name: "journalctl",
        value_kind: "identifier",
        scope: super::local::Scope::CurrentDir,
        source: super::local::Source::Lines {
            executable: "journalctl",
            args: &["--no-pager", "-F", "SYSLOG_IDENTIFIER"],
            parser: parse_non_empty_lines,
        },
        description: "journal identifier",
    },
    super::local::LocalSpec {
        provider: "machinectl.machine",
        command_name: "machinectl",
        value_kind: "machine",
        scope: super::local::Scope::CurrentDir,
        source: super::local::Source::Lines {
            executable: "machinectl",
            args: &["list", "--no-legend", "--no-pager"],
            parser: parse_first_column_values,
        },
        description: "systemd machine",
    },
    super::local::LocalSpec {
        provider: "ufw.application",
        command_name: "ufw",
        value_kind: "application",
        scope: super::local::Scope::CurrentDir,
        source: super::local::Source::Lines {
            executable: "ufw",
            args: &["app", "list"],
            parser: parse_ufw_applications,
        },
        description: "UFW application profile",
    },
    super::local::LocalSpec {
        provider: "wireless.device",
        command_name: "iw",
        value_kind: "wireless-device",
        scope: super::local::Scope::CurrentDir,
        source: super::local::Source::Lines {
            executable: "iw",
            args: &["dev"],
            parser: parse_iw_devices,
        },
        description: "wireless device",
    },
    super::local::LocalSpec {
        provider: "selinux.boolean",
        command_name: "getsebool",
        value_kind: "boolean",
        scope: super::local::Scope::FixedCwd("/"),
        source: super::local::Source::Lines {
            executable: "getsebool",
            args: &["-a"],
            parser: parse_selinux_booleans,
        },
        description: "SELinux boolean",
    },
    super::local::LocalSpec {
        provider: "pacman.repository",
        command_name: "pacman-conf",
        value_kind: "repository",
        scope: super::local::Scope::Fixed("/etc/pacman.conf"),
        source: super::local::Source::Lines {
            executable: "pacman-conf",
            args: &["--repo-list"],
            parser: parse_non_empty_lines,
        },
        description: "pacman repository",
    },
    super::local::LocalSpec {
        provider: "ip.route_table",
        command_name: "ip",
        value_kind: "route-table",
        scope: super::local::Scope::Fixed("/etc/iproute2/rt_tables"),
        source: super::local::Source::ScopePath {
            loader: load_ip_route_tables,
        },
        description: "IP route table",
    },
    super::local::LocalSpec {
        provider: "mdadm.array",
        command_name: "mdadm",
        value_kind: "array",
        scope: super::local::Scope::Fixed("/proc/mdstat"),
        source: super::local::Source::ScopePath {
            loader: load_mdadm_arrays,
        },
        description: "mdraid array",
    },
    super::local::LocalSpec {
        provider: "audit.rule_key",
        command_name: "audit",
        value_kind: "rule-key",
        scope: super::local::Scope::Fixed("/etc/audit/rules.d"),
        source: super::local::Source::ScopePath {
            loader: load_audit_rule_keys,
        },
        description: "audit rule key",
    },
    super::local::LocalSpec {
        provider: "login.shell",
        command_name: "shells",
        value_kind: "login-shell",
        scope: super::local::Scope::Fixed("/etc"),
        source: super::local::Source::Path {
            path: "/etc/shells",
            loader: load_login_shells,
        },
        description: "login shell",
    },
    super::local::LocalSpec {
        provider: "udev.subsystem",
        command_name: "udevadm",
        value_kind: "subsystem",
        scope: super::local::Scope::Fixed("/sys/class"),
        source: super::local::Source::ScopePath {
            loader: load_udev_subsystems,
        },
        description: "device subsystem",
    },
    super::local::LocalSpec {
        provider: "snapper.config",
        command_name: "snapper",
        value_kind: "config",
        scope: super::local::Scope::Fixed("/etc/snapper/configs"),
        source: super::local::Source::ScopePath {
            loader: load_file_names,
        },
        description: "snapper configuration",
    },
];

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

    fn collect_mkinitcpio_preset_candidates(
        &self,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let preset_dir = PathBuf::from("/etc/mkinitcpio.d");
        let scope = preset_dir.clone();
        self.collect_cached_value_candidates(
            "mkinitcpio",
            "preset",
            scope,
            current_token,
            "mkinitcpio preset",
            cached_only,
            move || Ok(load_file_stems(&preset_dir, ".preset")),
        )
    }

    fn collect_snapper_snapshot_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let config = selected_snapper_config(parsed_command_line)
            .unwrap_or("root")
            .to_string();
        let command_path = self.resolve_command_path("snapper");
        let current_dir = current_dir.to_path_buf();
        let scope = PathBuf::from("/etc/snapper/configs").join(&config);
        let (range_prefix, filter_token) = snapper_snapshot_filter(current_token);
        let mut candidates = self.collect_cached_value_candidates(
            "snapper",
            &format!("snapshot:{config}"),
            scope,
            &filter_token,
            "snapper snapshot",
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                let output = run_command_stdout(
                    &command_path,
                    &[
                        "--jsonout",
                        "--config",
                        config.as_str(),
                        "list",
                        "--columns",
                        "number,description",
                    ],
                    &current_dir,
                )?;
                Ok(parse_snapper_snapshot_json(&output))
            },
        );
        if let Some(prefix) = range_prefix {
            for candidate in &mut candidates {
                candidate.text.insert_str(0, &prefix);
            }
        }
        candidates
    }
}

fn selected_snapper_config(parsed_command_line: &ParsedCommandLine) -> Option<&str> {
    let words = completion_words(parsed_command_line);
    for (index, word) in words.iter().enumerate() {
        if matches!(*word, "-c" | "--config")
            && let Some(value) = words
                .get(index + 1)
                .copied()
                .filter(|value| !value.is_empty())
        {
            return Some(value);
        }
        if let Some(value) = word
            .strip_prefix("--config=")
            .filter(|value| !value.is_empty())
        {
            return Some(value);
        }
        if let Some(value) = word.strip_prefix("-c").filter(|value| !value.is_empty()) {
            return Some(value);
        }
    }
    None
}

fn snapper_snapshot_filter(current_token: &str) -> (Option<String>, String) {
    if let Some((left, right)) = current_token.split_once("..")
        && !left.is_empty()
        && left.chars().all(|character| character.is_ascii_digit())
        && right.chars().all(|character| character.is_ascii_digit())
    {
        return (Some(format!("{left}..")), right.to_string());
    }

    if let Some((left, right)) = current_token.split_once('-')
        && !left.is_empty()
        && left.chars().all(|character| character.is_ascii_digit())
        && right.chars().all(|character| character.is_ascii_digit())
    {
        return (Some(format!("{left}-")), right.to_string());
    }

    (None, current_token.to_string())
}

fn load_file_names(dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    dedup_sorted(
        entries
            .flatten()
            .filter(|entry| entry.path().is_file())
            .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
            .collect(),
    )
}

fn load_file_stems(dir: &Path, suffix: &str) -> Vec<String> {
    dedup_sorted(
        load_file_names(dir)
            .into_iter()
            .filter_map(|name| name.strip_suffix(suffix).map(str::to_string))
            .filter(|name| !name.is_empty())
            .collect(),
    )
}

fn parse_snapper_snapshot_json(output: &str) -> Vec<String> {
    fn collect_numbers(value: &serde_json::Value, values: &mut Vec<String>) {
        match value {
            serde_json::Value::Array(items) => {
                for item in items {
                    collect_numbers(item, values);
                }
            }
            serde_json::Value::Object(object) => {
                if let Some(number) = object.get("number") {
                    match number {
                        serde_json::Value::Number(number) => values.push(number.to_string()),
                        serde_json::Value::String(number) if !number.is_empty() => {
                            values.push(number.clone())
                        }
                        _ => {}
                    }
                }
                for value in object.values() {
                    if !matches!(
                        value,
                        serde_json::Value::Number(_) | serde_json::Value::String(_)
                    ) {
                        collect_numbers(value, values);
                    }
                }
            }
            _ => {}
        }
    }

    let Ok(value) = serde_json::from_str::<serde_json::Value>(output) else {
        return Vec::new();
    };
    let mut values = Vec::new();
    collect_numbers(&value, &mut values);
    dedup_sorted(values)
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
