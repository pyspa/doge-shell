//! The collectors `collect` dispatches to when the candidates need more than a
//! fixed command or file read: systemd units, btrfs subvolumes, device-mapper
//! devices, SELinux modules, mkinitcpio presets and snapper snapshots.
use super::*;

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

    pub(super) fn collect_mkinitcpio_preset_candidates(
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

    pub(super) fn collect_snapper_snapshot_candidates(
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
