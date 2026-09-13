//! The collectors whose shape is fixed once the command is known: a value kind
//! plus a loader handed straight to the cache engine. Covers nmcli values,
//! installed packages, mountpoints, kernel modules, blkid attributes, sysctl
//! keys, WireGuard configs, tcpdump interfaces, cargo features and metadata,
//! users/groups, systemd units and JS dependencies.
use super::*;

impl DynamicCompletionProvider {
    pub(super) fn collect_nmcli_value_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        spec: NmcliCompletionSpec<'_>,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("nmcli");
        let current_dir = current_dir.to_path_buf();
        let args = spec
            .args
            .iter()
            .map(|arg| arg.to_string())
            .collect::<Vec<_>>();
        self.collect_cached_value_candidates(
            "nmcli",
            spec.kind,
            canonicalize_path(&current_dir),
            current_token,
            spec.description,
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                let args = args.iter().map(String::as_str).collect::<Vec<_>>();
                let lines = run_command_lines(&command_path, &args, &current_dir)?;
                Ok((spec.parser)(&lines))
            },
        )
    }

    pub(super) fn collect_pip_installed_package_candidates(
        &self,
        current_dir: &Path,
        command_name: &str,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path(command_name);
        let current_dir = current_dir.to_path_buf();
        self.collect_cached_value_candidates(
            command_name,
            "installed-package",
            canonicalize_path(&current_dir),
            current_token,
            "installed python package",
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                Ok(parse_pip_freeze_packages(&run_command_lines(
                    &command_path,
                    &["list", "--format=freeze"],
                    &current_dir,
                )?))
            },
        )
    }

    pub(super) fn collect_mountpoint_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("findmnt");
        let current_dir = current_dir.to_path_buf();
        self.collect_cached_value_candidates(
            "umount",
            "mount-target",
            canonicalize_path(&current_dir),
            current_token,
            "mount target",
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                Ok(
                    run_command_lines(&command_path, &["-rno", "TARGET"], &current_dir)?
                        .into_iter()
                        .filter(|target| target != "/")
                        .collect(),
                )
            },
        )
    }

    /// `scope: "loaded"` restricts the candidates to the modules currently in
    /// the kernel, which is what `rmmod` and friends can actually act on. The
    /// default lists every installable module, as `modprobe` needs.
    pub(super) fn collect_kernel_module_candidates(
        &self,
        scope: Option<&str>,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        if scope == Some("loaded") {
            return self.collect_cached_value_candidates(
                "lsmod",
                "loaded-kernel-module",
                PathBuf::from("/proc/modules"),
                current_token,
                "loaded kernel module",
                cached_only,
                || Ok(load_loaded_kernel_module_names(Path::new("/proc/modules"))),
            );
        }
        self.collect_cached_value_candidates(
            "modprobe",
            "kernel-module",
            PathBuf::from("/lib/modules"),
            current_token,
            "kernel module",
            cached_only,
            || Ok(load_kernel_module_names()),
        )
    }

    pub(super) fn collect_blkid_attribute_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        attribute: &'static str,
        description: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("blkid");
        let current_dir = current_dir.to_path_buf();
        let value_kind = attribute.to_ascii_lowercase();
        self.collect_cached_value_candidates(
            "blkid",
            &value_kind,
            PathBuf::from("/run/blkid"),
            current_token,
            description,
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                Ok(parse_blkid_export_attribute(
                    &run_command_lines(&command_path, &["-o", "export"], &current_dir)?,
                    attribute,
                ))
            },
        )
    }

    pub(super) fn collect_sysctl_key_candidates(
        &self,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        if current_token.contains('=') {
            return Vec::new();
        }
        self.collect_cached_value_candidates(
            "sysctl",
            "key",
            PathBuf::from("/proc/sys"),
            current_token,
            "sysctl key",
            cached_only,
            || Ok(load_sysctl_keys()),
        )
    }

    pub(super) fn collect_wireguard_config_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
    ) -> Vec<EnhancedCandidate> {
        collect_wireguard_config_names_from_dirs([Path::new("/etc/wireguard"), current_dir])
            .into_iter()
            .filter(|value| matches_prefix(current_token, value))
            .map(|value| EnhancedCandidate {
                text: value,
                description: Some("WireGuard config".to_string()),
                candidate_type: CandidateType::Argument,
                priority: 130,
            })
            .collect()
    }

    pub(crate) fn collect_tcpdump_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        cache_policy: CachePolicy,
    ) -> Vec<EnhancedCandidate> {
        let cached_only = cache_policy.is_cached_only();
        let CompletionContext::OptionValue { option_name, .. } =
            &parsed_command_line.completion_context
        else {
            return Vec::new();
        };
        if option_name != "-i" {
            return Vec::new();
        }
        self.collect_cached_value_candidates(
            "tcpdump",
            "interface",
            PathBuf::from("/sys/class/net"),
            parsed_command_line.current_token.as_str(),
            "network interface",
            cached_only,
            || Ok(load_network_interfaces()),
        )
    }

    pub(super) fn collect_cargo_feature_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let (completed_prefix, active_token) = cargo_feature_token_parts(current_token);
        let mut candidates = self.collect_cargo_metadata_candidates(
            current_dir,
            active_token,
            CargoMetadataValueKind::Feature,
            "cargo feature",
            cached_only,
        );
        if !completed_prefix.is_empty() {
            for candidate in &mut candidates {
                candidate.text.insert_str(0, completed_prefix);
            }
        }
        candidates
    }

    pub(super) fn collect_owner_group_candidates(
        &self,
        current_token: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let values = self.load_or_lookup_command_values(
            "system",
            "owner-group",
            PathBuf::from("/etc"),
            cached_only,
            CommandQueryPolicy::LOCAL,
            || Ok(load_owner_group_values()),
        );
        owner_group_candidates(&values, current_token)
    }

    pub(super) fn collect_cargo_metadata_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        kind: CargoMetadataValueKind,
        description: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let command_path = self.resolve_command_path("cargo");
        let current_dir = current_dir.to_path_buf();
        let scope_dir = self.cached_project_root(&current_dir);
        let value_kind = match kind {
            CargoMetadataValueKind::Package => "package",
            CargoMetadataValueKind::Bin => "bin",
            CargoMetadataValueKind::Example => "example",
            CargoMetadataValueKind::Feature => "feature",
            CargoMetadataValueKind::Test => "test",
            CargoMetadataValueKind::Bench => "bench",
        };
        self.collect_cached_value_candidates(
            "cargo",
            value_kind,
            scope_dir,
            current_token,
            description,
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                let output = run_command_stdout(
                    &command_path,
                    &["metadata", "--no-deps", "--format-version", "1"],
                    &current_dir,
                )?;
                Ok(parse_cargo_metadata_values(&output, kind))
            },
        )
    }

    pub(super) fn collect_systemd_unit_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
        query: SystemdUnitQuery,
        description: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let SystemdUnitQuery {
            kind,
            manager_scope,
            unit_type,
        } = query;
        let command_path = self.resolve_command_path("systemctl");
        let current_dir = current_dir.to_path_buf();
        let base_value_kind = match kind {
            SystemdUnitListKind::All => "unit-all",
            SystemdUnitListKind::Running => "unit-running",
            SystemdUnitListKind::Enabled => "unit-enabled",
            SystemdUnitListKind::Disabled => "unit-disabled",
            SystemdUnitListKind::UnitFiles => "unit-files",
        };
        let value_kind = match manager_scope {
            Some(SystemdManagerScope::System) => format!("system-{base_value_kind}"),
            Some(SystemdManagerScope::User) => format!("user-{base_value_kind}"),
            Some(SystemdManagerScope::Global) => format!("global-{base_value_kind}"),
            None => base_value_kind.to_string(),
        };
        let value_kind = match unit_type {
            Some(unit_type) => format!("{value_kind}:{unit_type}"),
            None => value_kind,
        };
        self.collect_cached_value_candidates(
            "systemctl",
            &value_kind,
            canonicalize_path(&current_dir),
            current_token,
            description,
            cached_only,
            move || {
                let Some(command_path) = command_path else {
                    return Ok(Vec::new());
                };
                let mut args: Vec<&str> = Vec::new();
                match manager_scope {
                    Some(SystemdManagerScope::System) => args.push("--system"),
                    Some(SystemdManagerScope::User) => args.push("--user"),
                    Some(SystemdManagerScope::Global) => args.push("--global"),
                    None => {}
                }
                args.extend(match kind {
                    SystemdUnitListKind::All => {
                        vec!["list-units", "--all", "--no-pager", "--no-legend"]
                    }
                    SystemdUnitListKind::Running => {
                        vec!["list-units", "--state=running", "--no-pager", "--no-legend"]
                    }
                    SystemdUnitListKind::Enabled => vec![
                        "list-unit-files",
                        "--state=enabled",
                        "--no-pager",
                        "--no-legend",
                    ],
                    SystemdUnitListKind::Disabled => vec![
                        "list-unit-files",
                        "--state=disabled",
                        "--no-pager",
                        "--no-legend",
                    ],
                    SystemdUnitListKind::UnitFiles => {
                        vec!["list-unit-files", "--no-pager", "--no-legend"]
                    }
                });
                if let Some(unit_type) = unit_type {
                    args.push(unit_type);
                }
                Ok(parse_first_fields(&run_command_lines(
                    &command_path,
                    &args,
                    &current_dir,
                )?))
            },
        )
    }

    pub(crate) fn collect_js_dependency_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        command_name: &str,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        if !matches!(
            parsed_command_line.completion_context,
            CompletionContext::SubCommand | CompletionContext::Argument { .. }
        ) {
            return Vec::new();
        }

        let project_root = self.cached_project_root(current_dir);
        let package_json = project_root.join("package.json");
        self.collect_cached_value_candidates(
            command_name,
            "package-json-dependency",
            project_root,
            parsed_command_line.current_token.as_str(),
            "package.json dependency",
            cached_only,
            move || Ok(load_package_json_dependencies(&package_json)),
        )
    }
}
