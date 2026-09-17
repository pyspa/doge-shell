//! Fixed-shape completion providers that live directly here rather than in a family submodule (`git.*`, `platform.rs`'s remote CLIs, ...): a fixed executable+args+parser, or a fixed path read through a
//! loader. See `local` for what belongs in a table at all.
use super::*;

/// `local`'s share of the fixed-shape providers that live directly in this
/// file rather than in a family submodule (`git.*`, `platform.rs`'s remote
/// CLIs, ...). See `local` for what belongs in a table at all.
pub(super) const CORE_LOCAL_SPECS: &[local::LocalSpec] = &[
    local::LocalSpec {
        provider: "agent.task",
        command_name: "agent",
        value_kind: "task",
        // Not a real path - `Fixed` only serves as this row's cache key, and
        // the store's own location depends on `XDG_STATE_HOME` at runtime, so
        // it cannot be a `&'static str` literal here anyway.
        scope: local::Scope::Fixed("dsh:agent"),
        source: local::Source::Fixed {
            loader: load_agent_task_ids,
        },
        description: "agent task id",
    },
    local::LocalSpec {
        provider: "cron.job",
        command_name: "cron",
        value_kind: "job",
        // Not a real path - `Fixed` only serves as this row's cache key, and
        // the store's own location depends on `XDG_STATE_HOME` at runtime, so
        // it cannot be a `&'static str` literal here anyway.
        scope: local::Scope::Fixed("dsh:cron"),
        source: local::Source::Fixed {
            loader: load_cron_job_names,
        },
        description: "cron job name",
    },
    local::LocalSpec {
        provider: "dbus.service",
        command_name: "busctl",
        value_kind: "service",
        scope: local::Scope::Fixed("/run/dbus"),
        source: local::Source::Lines {
            executable: "busctl",
            args: &["list"],
            parser: parse_busctl_services,
        },
        description: "D-Bus service",
    },
    local::LocalSpec {
        provider: "localectl.locale",
        command_name: "localectl",
        value_kind: "locale",
        scope: local::Scope::Fixed("/usr/lib/locale"),
        source: local::Source::Lines {
            executable: "localectl",
            args: &["list-locales"],
            parser: parse_package_lines,
        },
        description: "locale",
    },
    local::LocalSpec {
        provider: "localectl.keymap",
        command_name: "localectl",
        value_kind: "keymap",
        scope: local::Scope::Fixed("/usr/share/kbd/keymaps"),
        source: local::Source::Lines {
            executable: "localectl",
            args: &["list-keymaps"],
            parser: parse_package_lines,
        },
        description: "keymap",
    },
    local::LocalSpec {
        provider: "loginctl.session",
        command_name: "loginctl",
        value_kind: "session",
        scope: local::Scope::Fixed("/run/systemd/sessions"),
        source: local::Source::Lines {
            executable: "loginctl",
            args: &["list-sessions", "--no-legend"],
            parser: parse_loginctl_sessions,
        },
        description: "login session",
    },
    local::LocalSpec {
        provider: "loginctl.seat",
        command_name: "loginctl",
        value_kind: "seat",
        scope: local::Scope::Fixed("/run/systemd/seats"),
        source: local::Source::Lines {
            executable: "loginctl",
            args: &["list-seats", "--no-legend"],
            parser: parse_loginctl_seats,
        },
        description: "login seat",
    },
    local::LocalSpec {
        provider: "loop.device",
        command_name: "losetup",
        value_kind: "loop-device",
        scope: local::Scope::Fixed("/sys/block"),
        source: local::Source::Lines {
            executable: "losetup",
            args: &["--list", "--noheadings", "--output", "NAME"],
            parser: parse_losetup_devices,
        },
        description: "loop device",
    },
    local::LocalSpec {
        provider: "timedatectl.timezone",
        command_name: "timedatectl",
        value_kind: "timezone",
        scope: local::Scope::Fixed("/usr/share/zoneinfo"),
        source: local::Source::Lines {
            executable: "timedatectl",
            args: &["list-timezones"],
            parser: parse_package_lines,
        },
        description: "time zone",
    },
    local::LocalSpec {
        provider: "apk.installed_package",
        command_name: "apk",
        value_kind: "installed-package",
        scope: local::Scope::Fixed("/lib/apk/db/installed"),
        source: local::Source::Lines {
            executable: "apk",
            args: &["info"],
            parser: parse_package_lines,
        },
        description: "installed apk package",
    },
    local::LocalSpec {
        provider: "zypper.installed_package",
        command_name: "zypper",
        value_kind: "installed-package",
        scope: local::Scope::Fixed("/var/lib/rpm"),
        source: local::Source::Lines {
            executable: "rpm",
            args: &["-qa", "--qf", "%{NAME}\\n"],
            parser: parse_package_lines,
        },
        description: "installed rpm package",
    },
    local::LocalSpec {
        provider: "journalctl.boot",
        command_name: "journalctl",
        value_kind: "boot",
        scope: local::Scope::Fixed("/var/log/journal"),
        source: local::Source::Lines {
            executable: "journalctl",
            args: &["--list-boots", "--no-pager"],
            parser: parse_journalctl_boots,
        },
        description: "journal boot",
    },
    local::LocalSpec {
        provider: "firewalld.zone",
        command_name: "firewall-cmd",
        value_kind: "zone",
        scope: local::Scope::Fixed("/etc/firewalld/zones"),
        source: local::Source::Lines {
            executable: "firewall-cmd",
            args: &["--get-zones"],
            parser: parse_whitespace_values,
        },
        description: "firewalld zone",
    },
    local::LocalSpec {
        provider: "firewalld.service",
        command_name: "firewall-cmd",
        value_kind: "service",
        scope: local::Scope::Fixed("/usr/lib/firewalld/services"),
        source: local::Source::Lines {
            executable: "firewall-cmd",
            args: &["--get-services"],
            parser: parse_whitespace_values,
        },
        description: "firewalld service",
    },
    local::LocalSpec {
        provider: "firewalld.icmp_type",
        command_name: "firewall-cmd",
        value_kind: "icmp-type",
        scope: local::Scope::Fixed("/usr/lib/firewalld/icmptypes"),
        source: local::Source::Lines {
            executable: "firewall-cmd",
            args: &["--get-icmptypes"],
            parser: parse_whitespace_values,
        },
        description: "firewalld ICMP type",
    },
    local::LocalSpec {
        provider: "networkctl.link",
        command_name: "networkctl",
        value_kind: "link",
        scope: local::Scope::Fixed("/sys/class/net"),
        source: local::Source::Lines {
            executable: "networkctl",
            args: &["list", "--all", "--no-legend", "--no-pager"],
            parser: parse_networkctl_links,
        },
        description: "network link",
    },
    local::LocalSpec {
        provider: "ipset.set",
        command_name: "ipset",
        value_kind: "set",
        scope: local::Scope::Fixed("/etc/ipset.conf"),
        source: local::Source::Lines {
            executable: "ipset",
            args: &["list", "-n"],
            parser: parse_package_lines,
        },
        description: "ipset set",
    },
    local::LocalSpec {
        provider: "wireguard.interface",
        command_name: "wg",
        value_kind: "interface",
        scope: local::Scope::Fixed("/etc/wireguard"),
        source: local::Source::Lines {
            executable: "wg",
            args: &["show", "interfaces"],
            parser: parse_whitespace_values,
        },
        description: "WireGuard interface",
    },
    local::LocalSpec {
        provider: "kind.cluster",
        command_name: "kind",
        value_kind: "cluster",
        scope: local::Scope::FixedCwd("/"),
        source: local::Source::Lines {
            executable: "kind",
            args: &["get", "clusters"],
            parser: parse_non_empty_lines,
        },
        description: "kind cluster",
    },
    local::LocalSpec {
        provider: "k3d.cluster",
        command_name: "k3d",
        value_kind: "cluster",
        scope: local::Scope::FixedCwd("/"),
        source: local::Source::Lines {
            executable: "k3d",
            args: &["cluster", "list", "--no-headers"],
            parser: parse_first_column_lines,
        },
        description: "k3d cluster",
    },
    local::LocalSpec {
        provider: "ollama.model",
        command_name: "ollama",
        value_kind: "model",
        scope: local::Scope::FixedCwd("/"),
        source: local::Source::Lines {
            executable: "ollama",
            args: &["list"],
            parser: parse_first_column_lines,
        },
        description: "ollama model",
    },
    local::LocalSpec {
        provider: "dnf.installed_package",
        command_name: "dnf",
        value_kind: "installed-package",
        scope: local::Scope::Fixed("/var/lib/rpm"),
        source: local::Source::Lines {
            executable: "rpm",
            args: &["-qa", "--qf", "%{NAME}\\n"],
            parser: parse_package_lines,
        },
        description: "installed rpm package",
    },
    local::LocalSpec {
        provider: "rpm.installed_package",
        command_name: "rpm",
        value_kind: "installed-package",
        scope: local::Scope::Fixed("/var/lib/rpm"),
        source: local::Source::Lines {
            executable: "rpm",
            args: &["-qa", "--qf", "%{NAME}\\n"],
            parser: parse_package_lines,
        },
        description: "installed rpm package",
    },
    local::LocalSpec {
        provider: "minikube.profile",
        command_name: "minikube",
        value_kind: "profile",
        scope: local::Scope::FixedCwd("/"),
        source: local::Source::Stdout {
            executable: "minikube",
            args: &["profile", "list", "-o", "json"],
            parser: parse_minikube_profiles,
        },
        description: "minikube profile",
    },
    local::LocalSpec {
        provider: "filesystem.type",
        command_name: "filesystem",
        value_kind: "type",
        scope: local::Scope::Fixed("/proc/filesystems"),
        source: local::Source::Fixed {
            loader: load_filesystem_types,
        },
        description: "filesystem type",
    },
    local::LocalSpec {
        provider: "swap.device",
        command_name: "swap",
        value_kind: "device",
        scope: local::Scope::Fixed("/proc/swaps"),
        source: local::Source::Fixed {
            loader: load_swap_devices,
        },
        description: "swap device",
    },
    local::LocalSpec {
        provider: "block.device",
        command_name: "block",
        value_kind: "device",
        scope: local::Scope::Fixed("/sys/block"),
        source: local::Source::Lines {
            executable: "lsblk",
            args: &["-rno", "NAME,TYPE"],
            parser: parse_lsblk_devices,
        },
        description: "block device",
    },
    local::LocalSpec {
        provider: "fstab.mountpoint",
        command_name: "fstab",
        value_kind: "mountpoint",
        scope: local::Scope::Fixed("/etc/fstab"),
        source: local::Source::Fixed {
            loader: load_fstab_mountpoints,
        },
        description: "fstab mount point",
    },
    local::LocalSpec {
        provider: "tmux.session",
        command_name: "tmux",
        value_kind: "session",
        scope: local::Scope::CurrentDirCanonical,
        source: local::Source::Lines {
            executable: "tmux",
            args: &["list-sessions", "-F", "#{session_name}"],
            parser: local::identity_lines,
        },
        description: "tmux session",
    },
    local::LocalSpec {
        provider: "screen.session",
        command_name: "screen",
        value_kind: "session",
        scope: local::Scope::CurrentDirCanonical,
        source: local::Source::Lines {
            executable: "screen",
            args: &["-ls"],
            parser: parse_screen_sessions,
        },
        description: "screen session",
    },
    local::LocalSpec {
        provider: "rustup.toolchain",
        command_name: "rustup",
        value_kind: "toolchain",
        scope: local::Scope::CurrentDirCanonical,
        source: local::Source::Lines {
            executable: "rustup",
            args: &["toolchain", "list"],
            parser: parse_first_fields,
        },
        description: "rustup toolchain",
    },
];

/// Full task IDs for `agent`'s completions (`show`/`logs`/`wait`/`resume`/
/// `retry`/`cancel`/`delete`/`respond`).
///
/// Lists IDs from the store; a missing store or any failure (permissions)
/// yields an empty list rather than an error, matching every other loader
/// in this table. The existence check comes first so a TAB press never
/// creates state as a side effect (`open` would create the directory and
/// database).
fn load_agent_task_ids() -> Vec<String> {
    use dsh_builtin::shell_capabilities::AgentTaskStore as _;

    let dir = dsh_builtin::config_paths::agent_state_dir();
    if !dir.exists() {
        return Vec::new();
    }
    let Ok(store) = crate::agent::SqliteTaskStore::open(&dir) else {
        return Vec::new();
    };
    store
        .list()
        .map(|tasks| tasks.into_iter().map(|task| task.id).collect())
        .unwrap_or_default()
}

/// Job names for `cron`'s completions (`show`/`edit`/`rm`/`run`/`pause`/
/// `resume`/`history`/`notepad`).
///
/// Opens the store read-only and lists names; any failure (store not yet
/// created, permissions) yields an empty list rather than an error, matching
/// every other loader in this table.
fn load_cron_job_names() -> Vec<String> {
    use dsh_builtin::shell_capabilities::CronStore;

    let Ok(store) =
        crate::cron::store::SqliteCronStore::open(&dsh_builtin::config_paths::cron_state_dir())
    else {
        return Vec::new();
    };
    store
        .list()
        .map(|jobs| jobs.into_iter().map(|job| job.name).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod agent_task_tests {
    use super::load_agent_task_ids;
    use std::ffi::OsString;

    /// A no-store environment must yield an empty list, not a panic or an
    /// error surfaced to the completion popup - every other loader in this
    /// table treats "nothing to read yet" the same way. It must also leave
    /// no state behind: completion is a read path, not a first-run setup.
    #[test]
    fn no_store_yields_an_empty_list_rather_than_failing() {
        let _guard = crate::test_env_lock();
        let previous = std::env::var_os("XDG_STATE_HOME");
        let dir = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("XDG_STATE_HOME", OsString::from(dir.path()));
        }

        let ids = load_agent_task_ids();
        let state_created = dsh_builtin::config_paths::agent_state_dir().exists();

        match previous {
            Some(value) => unsafe { std::env::set_var("XDG_STATE_HOME", value) },
            None => unsafe { std::env::remove_var("XDG_STATE_HOME") },
        }
        assert!(ids.is_empty(), "{ids:?}");
        assert!(
            !state_created,
            "completion must not create agent state"
        );
    }
}

#[cfg(test)]
mod cron_job_tests {
    use super::load_cron_job_names;
    use std::ffi::OsString;

    /// A no-store environment must yield an empty list, not a panic or an
    /// error surfaced to the completion popup - every other loader in this
    /// table treats "nothing to read yet" the same way.
    #[test]
    fn no_store_yields_an_empty_list_rather_than_failing() {
        let _guard = crate::test_env_lock();
        let previous = std::env::var_os("XDG_STATE_HOME");
        let dir = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("XDG_STATE_HOME", OsString::from(dir.path()));
        }

        let names = load_cron_job_names();

        match previous {
            Some(value) => unsafe { std::env::set_var("XDG_STATE_HOME", value) },
            None => unsafe { std::env::remove_var("XDG_STATE_HOME") },
        }
        assert!(names.is_empty(), "{names:?}");
    }
}
