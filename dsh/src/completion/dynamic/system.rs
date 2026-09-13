//! Reading the running system for candidates: the per-tool output parsers
//! (screen, pip, blkid, busctl, loginctl, losetup, nmcli, lsblk) and the
//! loaders for filesystem types, fstab mountpoints, sysctl keys, kernel
//! modules, network interfaces, swap devices and WireGuard configs.
//!
//! Only `load_filesystem_types` and `load_sysctl_keys` carry a paired macOS
//! arm. Of the rest: `load_fstab_mountpoints` needs none, because macOS reads
//! `/etc/fstab` at the same path and an absent file just yields nothing. The
//! three kernel/swap loaders (`/proc/swaps`, `/proc/modules`, `/lib/modules`)
//! are Linux-only and currently return an empty list on macOS, which is
//! tracked debt rather than a decision: macOS lists loaded extensions through
//! `kmutil showloaded`/`kextstat`, so a second arm is possible and missing.
//! Anything added here needs that arm from the start -- see
//! `docs/ai/skills/doge-shell-repo/references/platform-support.md`.
use super::*;

pub(super) fn parse_screen_sessions(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .flat_map(|line| line.split_whitespace())
            .filter(|field| {
                field.split_once('.').is_some_and(|(pid, name)| {
                    !name.is_empty() && pid.chars().all(|ch| ch.is_ascii_digit())
                })
            })
            .map(str::to_string)
            .collect(),
    )
}

/// Running process names, for `pkill`/`killall`-style completion.
///
/// Delegates to the process generator for the same reason
/// [`load_network_interfaces`] does: this walked `/proc` itself and so came
/// back empty on macOS while the generator had a working source.
pub(super) fn load_process_names() -> Vec<String> {
    dedup_sorted(crate::completion::generators::process::process_names())
}

/// Running pids. See [`load_process_names`].
pub(super) fn load_process_ids() -> Vec<String> {
    dedup_sorted(crate::completion::generators::process::process_ids())
}

pub(super) fn parse_pip_freeze_packages(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .map(|line| line.split("==").next().unwrap_or(line).to_string())
            .collect(),
    )
}

pub(super) fn parse_package_lines(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect(),
    )
}

pub(super) fn parse_first_fields_excluding(lines: &[String], excluded: &[&str]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| {
                let first = line.split_whitespace().next()?;
                if excluded
                    .iter()
                    .any(|header| first.eq_ignore_ascii_case(header))
                {
                    None
                } else {
                    Some(first.to_string())
                }
            })
            .collect(),
    )
}

pub(super) fn parse_blkid_export_attribute(lines: &[String], attribute: &str) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| {
                let (key, value) = line.split_once('=')?;
                if key == attribute && !value.is_empty() {
                    Some(value.to_string())
                } else {
                    None
                }
            })
            .collect(),
    )
}

pub(super) fn parse_busctl_services(lines: &[String]) -> Vec<String> {
    parse_first_fields_excluding(lines, &["NAME"])
}

pub(super) fn parse_loginctl_sessions(lines: &[String]) -> Vec<String> {
    parse_first_fields_excluding(lines, &["SESSION"])
}

pub(super) fn parse_loginctl_seats(lines: &[String]) -> Vec<String> {
    parse_first_fields_excluding(lines, &["SEAT"])
}

pub(super) fn parse_losetup_devices(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        parse_first_fields_excluding(lines, &["NAME"])
            .into_iter()
            .map(|device| {
                if device.starts_with("/dev/") {
                    device
                } else {
                    format!("/dev/{device}")
                }
            })
            .collect(),
    )
}

pub(super) fn parse_nmcli_first_field(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| {
                let first = line.split(':').next()?;
                if first.is_empty() {
                    None
                } else {
                    Some(first.to_string())
                }
            })
            .collect(),
    )
}

pub(super) fn parse_nmcli_connected_devices(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| {
                let (device, state) = line.split_once(':')?;
                if !device.is_empty() && state == "connected" {
                    Some(device.to_string())
                } else {
                    None
                }
            })
            .collect(),
    )
}

pub(super) fn parse_lsblk_devices(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                let name = fields.next()?;
                let kind = fields.next()?;
                if matches!(kind, "disk" | "part" | "loop") {
                    Some(format!("/dev/{name}"))
                } else {
                    None
                }
            })
            .collect(),
    )
}

/// Filesystem types this kernel can mount, for `mount -t`.
#[cfg(not(target_os = "macos"))]
pub(super) fn load_filesystem_types() -> Vec<String> {
    fs::read_to_string("/proc/filesystems")
        .map(|contents| {
            dedup_sorted(
                contents
                    .lines()
                    .filter_map(|line| line.split_whitespace().last().map(str::to_string))
                    .collect(),
            )
        })
        .unwrap_or_default()
}

/// macOS keeps one bundle per filesystem under `/System/Library/Filesystems`,
/// named `<type>.fs` -- `apfs.fs`, `msdos.fs`, `smbfs.fs` -- which is the same
/// vocabulary `mount -t` accepts. Entries without the suffix are helper
/// directories, not types.
#[cfg(target_os = "macos")]
pub(super) fn load_filesystem_types() -> Vec<String> {
    fs::read_dir("/System/Library/Filesystems")
        .map(|entries| {
            dedup_sorted(
                entries
                    .flatten()
                    .filter_map(|entry| {
                        entry
                            .file_name()
                            .to_str()
                            .and_then(|name| name.strip_suffix(".fs"))
                            .map(str::to_string)
                    })
                    .collect(),
            )
        })
        .unwrap_or_default()
}

pub(super) fn load_fstab_mountpoints() -> Vec<String> {
    fs::read_to_string("/etc/fstab")
        .map(|contents| parse_fstab_mountpoints(&contents))
        .unwrap_or_default()
}

pub(super) fn parse_fstab_mountpoints(contents: &str) -> Vec<String> {
    dedup_sorted(
        contents
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    return None;
                }
                let mountpoint = line.split_whitespace().nth(1)?;
                Some(decode_fstab_field(mountpoint))
            })
            .collect(),
    )
}

pub(super) fn decode_fstab_field(value: &str) -> String {
    value
        .replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
}

pub(super) fn load_package_json_dependencies(package_json: &Path) -> Vec<String> {
    let Ok(contents) = fs::read_to_string(package_json) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&contents) else {
        return Vec::new();
    };
    let mut values = Vec::new();
    for key in [
        "dependencies",
        "devDependencies",
        "optionalDependencies",
        "peerDependencies",
    ] {
        if let Some(object) = value.get(key).and_then(|value| value.as_object()) {
            values.extend(object.keys().cloned());
        }
    }
    dedup_sorted(values)
}

/// Interface names for `tcpdump -i` and friends.
///
/// Delegates to the interface generator, which reads `/sys/class/net` where it
/// exists and `getifaddrs` on macOS. This used to read sysfs itself and so
/// returned nothing on macOS while the generator returned the real list.
pub(super) fn load_network_interfaces() -> Vec<String> {
    dedup_sorted(crate::completion::generators::interface::interface_names())
}

pub(super) fn load_swap_devices() -> Vec<String> {
    fs::read_to_string("/proc/swaps")
        .map(|contents| {
            dedup_sorted(
                contents
                    .lines()
                    .skip(1)
                    .filter_map(|line| line.split_whitespace().next().map(str::to_string))
                    .collect(),
            )
        })
        .unwrap_or_default()
}

/// Every tunable `sysctl` accepts.
///
/// `/proc/sys` is the same namespace with slashes for dots, so walking it
/// avoids spawning anything.
#[cfg(not(target_os = "macos"))]
pub(super) fn load_sysctl_keys() -> Vec<String> {
    let root = Path::new("/proc/sys");
    let mut values = Vec::new();
    collect_sysctl_keys(root, root, &mut values);
    dedup_sorted(values)
}

/// macOS exposes the same namespace only through the tool itself: there is no
/// procfs to walk, and `sysctl -aN` prints the names without their values in a
/// few milliseconds. Failures fall back to an empty list, as everywhere else
/// here.
#[cfg(target_os = "macos")]
pub(super) fn load_sysctl_keys() -> Vec<String> {
    run_command_lines("sysctl", &["-aN"], Path::new("/"))
        .map(dedup_sorted)
        .unwrap_or_default()
}

#[cfg(not(target_os = "macos"))]
pub(super) fn collect_sysctl_keys(root: &Path, dir: &Path, values: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_sysctl_keys(root, &path, values);
            continue;
        }

        if !path.is_file() {
            continue;
        }

        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        let key = relative
            .components()
            .filter_map(|component| component.as_os_str().to_str())
            .collect::<Vec<_>>()
            .join(".");
        if !key.is_empty() {
            values.push(key);
        }
    }
}

/// Reads the currently loaded modules from `/proc/modules`, whose rows start
/// with the module name (`ext4 1052672 1 - Live 0x0000000000000000`).
pub(super) fn load_loaded_kernel_module_names(path: &Path) -> Vec<String> {
    let Ok(contents) = fs::read_to_string(path) else {
        return Vec::new();
    };
    dedup_sorted(
        contents
            .lines()
            .filter_map(|line| line.split_whitespace().next())
            .map(str::to_string)
            .collect(),
    )
}

pub(super) fn load_kernel_module_names() -> Vec<String> {
    let release = run_command_lines("uname", &["-r"], Path::new("/"))
        .ok()
        .and_then(|lines| lines.into_iter().next());
    let root = release
        .map(|release| PathBuf::from("/lib/modules").join(release).join("kernel"))
        .filter(|path| path.exists())
        .unwrap_or_else(|| PathBuf::from("/lib/modules"));
    let mut values = Vec::new();
    collect_kernel_module_names(&root, &mut values);
    dedup_sorted(values)
}

pub(super) fn collect_kernel_module_names(dir: &Path, values: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_kernel_module_names(&path, values);
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let module_name = file_name
            .strip_suffix(".ko")
            .or_else(|| file_name.strip_suffix(".ko.xz"))
            .or_else(|| file_name.strip_suffix(".ko.zst"));
        if let Some(module_name) = module_name {
            values.push(module_name.replace('-', "_"));
        }
    }
}

pub(super) fn collect_wireguard_config_names_from_dirs<'a>(
    dirs: impl IntoIterator<Item = &'a Path>,
) -> Vec<String> {
    let mut values = Vec::new();
    for dir in dirs {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Some(name) = file_name.strip_suffix(".conf") else {
                continue;
            };
            if !name.is_empty() {
                values.push(name.to_string());
            }
        }
    }
    dedup_sorted(values)
}

pub(super) fn format_task_description(source: &str, command: &str) -> String {
    let summary = format!("{source}: {command}");
    truncate_string(&summary, 80)
}

pub(super) fn truncate_string(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }

    let mut out: String = value.chars().take(max_chars.saturating_sub(3)).collect();
    out.push_str("...");
    out
}
