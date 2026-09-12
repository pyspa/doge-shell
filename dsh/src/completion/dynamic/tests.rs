use super::*;
use crate::completion::parser::CommandLineParser;
use std::os::unix::fs::PermissionsExt;
use tempfile::tempdir;

fn parsed(input: &str) -> ParsedCommandLine {
    CommandLineParser::new().parse(input, input.len())
}

/// These three used to read Linux-only paths, so on macOS every one of
/// them returned an empty list and the completions they back went silent.
/// The values asserted here exist on both platforms.
/// `chown <TAB>` offered only service accounts on macOS while the user
/// generator already knew better; both halves of the `user:group` value
/// have to come from a source that platform actually populates.
#[test]
fn an_owner_and_a_group_are_offered_for_chown() {
    let values = load_owner_group_values();

    assert!(
        values.iter().any(|value| value == "u:root"),
        "root is missing from the owner list"
    );
    assert!(
        values.iter().any(|value| value.starts_with("g:")),
        "no groups among {} values",
        values.len()
    );

    // The account running the test is the one a person is most likely to
    // type, and it is the one /etc/passwd omits on macOS. Both arms are
    // spelled out because the asymmetry is deliberate, not an oversight.
    #[cfg(not(target_os = "macos"))]
    {
        // Nothing to assert: on Linux the owner list deliberately stops at
        // /etc/passwd, so an LDAP- or SSSD-provided account is legitimately
        // absent from it. Asserting the current user here would fail on any
        // host whose accounts come from NSS rather than the file.
        let _ = &values;
    }
    #[cfg(target_os = "macos")]
    {
        // macOS, for the reason `user::tests` gives: `getpwuid` goes through
        // Open Directory, which names the interactive account that this
        // platform's /etc/passwd omits.
        //
        // SAFETY: `getpwuid` returns a pointer into a static buffer valid
        // until the next call on this thread; the name is copied out
        // immediately.
        let me = unsafe {
            let entry = libc::getpwuid(libc::getuid());
            assert!(!entry.is_null(), "no passwd entry for the current uid");
            std::ffi::CStr::from_ptr((*entry).pw_name)
                .to_string_lossy()
                .into_owned()
        };
        let expected = format!("u:{me}");
        assert!(
            values.contains(&expected),
            "{expected} is missing from {} values",
            values.len()
        );
    }
}

#[test]
fn the_running_process_is_offered_for_pkill() {
    let ids = load_process_ids();
    let names = load_process_names();
    let me = std::process::id().to_string();

    assert!(
        ids.contains(&me),
        "the running test process ({me}) is missing from {} pids",
        ids.len()
    );
    assert!(!names.is_empty(), "no process names at all");
}

#[test]
fn the_loopback_interface_is_offered_for_tcpdump() {
    let interfaces = load_network_interfaces();

    assert!(
        interfaces.iter().any(|name| name.starts_with("lo")),
        "no loopback interface among {interfaces:?}"
    );
}

/// Naming a type here would be host-dependent -- `/proc/filesystems` lists
/// only the modules this kernel registered, so a btrfs-rooted container
/// need not have ext4 -- so assert the shape instead: a non-empty list of
/// bare type tokens, which is what `mount -t` takes and what both readers
/// are supposed to produce.
#[test]
fn a_local_filesystem_type_is_offered_for_mount() {
    let types = load_filesystem_types();

    assert!(!types.is_empty(), "no filesystem types at all");
    for name in &types {
        assert!(
            !name.contains(char::is_whitespace) && !name.contains('/') && !name.ends_with(".fs"),
            "{name:?} is not a bare filesystem type"
        );
    }
}

#[test]
fn a_well_known_sysctl_key_is_offered() {
    let keys = load_sysctl_keys();

    // kern.hostname on macOS, kernel.hostname on Linux.
    assert!(
        keys.iter()
            .any(|key| key == "kern.hostname" || key == "kernel.hostname"),
        "no hostname key among {} sysctl keys",
        keys.len()
    );
}

fn write_executable_script(path: &Path, content: &str) {
    fs::write(path, content).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

fn wait_until(timeout: Duration, mut predicate: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if predicate() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

#[test]
fn pacman_sync_mode_resolves_bundled_and_long_operations() {
    // Installed packages (the only list that contains AUR packages).
    for input in [
        "pacman -R ",
        "pacman -Rns ",
        "pacman -Rs ",
        "pacman -Rdd ",
        "pacman --remove ",
        "pacman -Q ",
        "pacman -Qi ",
        "pacman -F ",
        "pacman --noconfirm -R ",
        "pacman -b /var/lib/pacman -R ",
    ] {
        assert_eq!(
            pacman_sync_mode(&parsed(input)),
            Some(false),
            "{input} should offer installed packages"
        );
    }

    for input in [
        "pacman -S ",
        "pacman -Syu ",
        "pacman -Sy ",
        "pacman --sync ",
    ] {
        assert_eq!(
            pacman_sync_mode(&parsed(input)),
            Some(true),
            "{input} should offer sync packages"
        );
    }

    // `-U` takes package files, and bare/unknown flags name no operation.
    for input in [
        "pacman -U ",
        "pacman --upgrade ",
        "pacman -h ",
        "pacman -V ",
        "pacman ",
    ] {
        assert_eq!(
            pacman_sync_mode(&parsed(input)),
            None,
            "{input} should offer no package candidates"
        );
    }

    // The operation flag under the cursor is still being typed.
    assert_eq!(pacman_sync_mode(&parsed("pacman -R")), None);
    assert_eq!(pacman_sync_mode(&parsed("pacman -Rn")), None);
}

#[test]
fn every_registered_provider_has_a_dispatch_arm() {
    let provider = DynamicCompletionProvider::new(Environment::new());
    let parsed = parsed("");
    let current_dir = std::env::current_dir().unwrap();

    for registration in registry::registrations() {
        assert!(
            provider
                .try_collect_declared_dynamic_candidates(
                    registration.id.as_str(),
                    None,
                    &parsed,
                    &current_dir,
                    CachePolicy::CachedOnly,
                )
                .is_some(),
            "registered provider '{}' ({:?}) has no dispatch arm",
            registration.id,
            registration.family
        );
    }
}

#[test]
fn unknown_provider_is_not_registered_or_dispatched() {
    let provider = DynamicCompletionProvider::new(Environment::new());
    let parsed = parsed("");
    let current_dir = std::env::current_dir().unwrap();

    assert!(registry::registration("unknown.provider").is_none());
    assert!(
        provider
            .try_collect_declared_dynamic_candidates(
                "unknown.provider",
                None,
                &parsed,
                &current_dir,
                CachePolicy::CachedOnly,
            )
            .is_none()
    );
}

#[test]
fn full_worker_queue_restores_pending_state() {
    let runtime = Arc::new(CompletionRuntime::with_worker_counts(
        runtime::CompletionWorkerCounts::new(1, 1, 1),
    ));
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
    assert!(runtime.submit_command(Box::new(move || {
        started_tx.send(()).unwrap();
        let _ = release_rx.recv_timeout(Duration::from_secs(2));
    })));
    started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    for _ in 0..4 {
        assert!(runtime.submit_command(Box::new(|| {})));
    }

    let provider = DynamicCompletionProvider::with_runtime(Environment::new(), runtime.clone());
    assert!(
        provider
            .load_command_values(
                DynamicCommandCacheKind::GitBranch,
                PathBuf::from("/queue-full"),
                || Ok(vec!["should-not-run".to_string()]),
            )
            .is_empty()
    );
    assert!(!provider.has_pending_refresh());
    assert!(
        runtime
            .diagnostics_lines()
            .iter()
            .any(|line| line.contains("dropped=1"))
    );

    release_tx.send(()).unwrap();
}

#[test]
fn cached_value_matches_prefers_prefix_before_fuzzy_fallback() {
    let prefix_matches = cached_value_matches(
        vec![
            "hotfix/feature".to_string(),
            "feature/main".to_string(),
            "feature/probe".to_string(),
        ],
        "feat",
    );
    assert_eq!(prefix_matches, vec!["feature/main", "feature/probe"]);

    let fuzzy_matches = cached_value_matches(
        vec!["hotfix/feature".to_string(), "release/main".to_string()],
        "hf",
    );
    assert_eq!(fuzzy_matches, vec!["hotfix/feature"]);
}

#[test]
fn command_cache_prunes_oldest_entries_to_limit() {
    let mut cache = ProjectDynamicCache::default();
    for index in 0..=DYNAMIC_COMMAND_CACHE_LIMIT {
        cache.commands.insert(
            DynamicCommandCacheKey {
                kind: DynamicCommandCacheKind::CommandValue {
                    command: "test".to_string(),
                    value_kind: format!("value-{index}"),
                },
                scope_dir: PathBuf::from(format!("/scope-{index}")),
            },
            CommandValueCacheEntry {
                values: vec![index.to_string()],
                cached_at: Instant::now(),
                last_load_duration: None,
                last_error: None,
            },
        );
    }

    prune_command_cache(&mut cache);

    assert_eq!(cache.commands.len(), DYNAMIC_COMMAND_CACHE_LIMIT);
    assert_eq!(cache.command_pruned_total, 1);
}

#[test]
fn recent_command_error_suppresses_immediate_retry() {
    let provider = DynamicCompletionProvider::new(Environment::new());
    let kind = DynamicCommandCacheKind::CommandValue {
        command: "remote".to_string(),
        value_kind: "resource".to_string(),
    };
    let scope_dir = PathBuf::from("/remote-scope");
    provider.cache.write().command_errors.insert(
        DynamicCommandCacheKey {
            kind: kind.clone(),
            scope_dir: scope_dir.clone(),
        },
        CommandValueErrorEntry {
            recorded_at: Instant::now(),
            last_load_duration: Duration::from_millis(1),
            error: "unauthenticated".to_string(),
        },
    );

    let values = provider.load_command_values_with_policy(
        kind,
        scope_dir,
        CommandQueryPolicy::REMOTE,
        || panic!("loader should not run during backoff"),
    );

    assert!(values.is_empty());
    assert!(!provider.has_pending_refresh());
}

#[test]
fn command_error_cache_prunes_oldest_entries_to_limit() {
    let mut cache = ProjectDynamicCache::default();
    for index in 0..=DYNAMIC_COMMAND_CACHE_LIMIT {
        cache.command_errors.insert(
            DynamicCommandCacheKey {
                kind: DynamicCommandCacheKind::CommandValue {
                    command: "remote".to_string(),
                    value_kind: format!("error-{index}"),
                },
                scope_dir: PathBuf::from(format!("/scope-{index}")),
            },
            CommandValueErrorEntry {
                recorded_at: Instant::now(),
                last_load_duration: Duration::from_millis(1),
                error: "failed".to_string(),
            },
        );
    }

    prune_command_error_cache(&mut cache);

    assert_eq!(cache.command_errors.len(), DYNAMIC_COMMAND_CACHE_LIMIT);
    assert_eq!(cache.command_pruned_total, 1);
}

#[test]
fn parse_cargo_metadata_values_extracts_packages_and_targets() {
    let metadata = r#"{
          "packages": [
            {
              "name": "app",
              "features": {
                "default": ["cli"],
                "cli": [],
                "serde": []
              },
              "targets": [
                { "name": "app", "kind": ["bin"] },
                { "name": "demo", "kind": ["example"] },
                { "name": "app", "kind": ["lib"] }
              ]
            },
            {
              "name": "lib",
              "features": {
                "serde": [],
                "tokio": []
              },
              "targets": [
                { "name": "tool", "kind": ["bin"] }
              ]
            }
          ]
        }"#;

    assert_eq!(
        parse_cargo_metadata_values(metadata, CargoMetadataValueKind::Package),
        vec!["app".to_string(), "lib".to_string()]
    );
    assert_eq!(
        parse_cargo_metadata_values(metadata, CargoMetadataValueKind::Bin),
        vec!["app".to_string(), "tool".to_string()]
    );
    assert_eq!(
        parse_cargo_metadata_values(metadata, CargoMetadataValueKind::Example),
        vec!["demo".to_string()]
    );
    assert_eq!(
        parse_cargo_metadata_values(metadata, CargoMetadataValueKind::Feature),
        vec![
            "cli".to_string(),
            "default".to_string(),
            "serde".to_string(),
            "tokio".to_string()
        ]
    );
    assert_eq!(cargo_feature_token_parts("serde,to"), ("serde,", "to"));
    assert_eq!(cargo_feature_token_parts("serde"), ("", "serde"));
}

#[test]
fn cluster_and_model_parsers_ignore_headers_and_read_profiles() {
    assert_eq!(
        parse_first_column_lines(&[
            "NAME ID SIZE".to_string(),
            "llama3.2:latest abc 2 GB".to_string(),
            "qwen3:8b def 5 GB".to_string(),
        ]),
        vec!["llama3.2:latest".to_string(), "qwen3:8b".to_string()]
    );
    assert_eq!(
        parse_minikube_profiles(
            r#"{
                  "valid": [{"Name": "dev"}, {"Name": "prod"}],
                  "invalid": [{"name": "broken"}]
                }"#
        ),
        vec!["broken".to_string(), "dev".to_string(), "prod".to_string()]
    );
}

#[test]
fn parse_ssh_hosts_filters_patterns_and_known_host_ports() {
    let config_hosts = parse_ssh_config_hosts(
        "Host dev prod-*\n  HostName example.com\nHost staging\nHost !blocked wildcard?\n",
    );
    assert_eq!(config_hosts, vec!["dev".to_string(), "staging".to_string()]);

    let known_hosts = parse_known_hosts(
        "github.com,140.82.112.3 ssh-ed25519 AAA\n[dev.local]:2222 ssh-rsa BBB\n|1|hashed entry\n",
    );
    assert_eq!(
        known_hosts,
        vec![
            "140.82.112.3".to_string(),
            "dev.local".to_string(),
            "github.com".to_string()
        ]
    );

    assert_eq!(
        format_ssh_host_candidate_text("ssh", Some("alice"), "dev".to_string()),
        "alice@dev"
    );
    assert_eq!(
        format_ssh_host_candidate_text("scp", Some("alice"), "dev".to_string()),
        "alice@dev:"
    );
    assert_eq!(
        format_ssh_host_candidate_text("rsync", Some("alice"), "dev".to_string()),
        "alice@dev:"
    );
    assert_eq!(
        format_ssh_host_candidate_text("rsync", None, "dev".to_string()),
        "dev:"
    );
}

#[test]
fn basic_dynamic_value_helpers_preserve_completion_context() {
    assert_eq!(man_page_name_from_file("printf.1.gz"), Some("printf"));
    assert_eq!(
        man_page_name_from_file("systemd.service.5"),
        Some("systemd.service")
    );
    assert_eq!(man_page_name_from_file("README"), None);

    let values = vec![
        "g:staff".to_string(),
        "g:storage".to_string(),
        "u:alice".to_string(),
        "u:bob".to_string(),
    ];
    assert_eq!(
        owner_group_candidates(&values, "ali")
            .into_iter()
            .map(|candidate| candidate.text)
            .collect::<Vec<_>>(),
        vec!["alice"]
    );
    assert_eq!(
        owner_group_candidates(&values, "alice:st")
            .into_iter()
            .map(|candidate| candidate.text)
            .collect::<Vec<_>>(),
        vec!["alice:staff", "alice:storage"]
    );
    assert_eq!(
        owner_group_candidates(&values, ":staf")
            .into_iter()
            .map(|candidate| candidate.text)
            .collect::<Vec<_>>(),
        vec![":staff"]
    );
}

#[test]
fn archive_helpers_find_read_archive_without_treating_create_as_read() {
    let dir = Path::new("/work");

    let tar = parsed("tar -xzf backup.tar.gz etc/");
    assert!(tar_reads_archive(&tar));
    assert_eq!(
        selected_tar_archive(&tar, dir),
        Some(dir.join("backup.tar.gz"))
    );

    let tar = parsed("tar --extract --file=backup.tar member");
    assert!(tar_reads_archive(&tar));
    assert_eq!(
        selected_tar_archive(&tar, dir),
        Some(dir.join("backup.tar"))
    );

    let tar = parsed("tar -cf backup.tar src/");
    assert!(!tar_reads_archive(&tar));

    let unzip = parsed("unzip backup.zip etc/");
    assert_eq!(
        selected_unzip_archive(&unzip, dir),
        Some(dir.join("backup.zip"))
    );
}

#[test]
fn command_value_parsers_normalize_common_command_outputs() {
    let unit_lines = vec![
        "ssh.service enabled".to_string(),
        "docker.service loaded active running Docker".to_string(),
        "ssh.service enabled".to_string(),
    ];
    assert_eq!(
        parse_first_fields(&unit_lines),
        vec!["docker.service".to_string(), "ssh.service".to_string()]
    );

    assert_eq!(
        parse_screen_sessions(&[
            "There is a screen on:".to_string(),
            "\t1234.dev-session\t(Detached)".to_string(),
            "1 Socket in /run/screen/S-user.".to_string(),
        ]),
        vec!["1234.dev-session".to_string()]
    );

    assert_eq!(
        parse_pip_freeze_packages(&["requests==2.32.0".to_string(), "pytest==8.0.0".to_string(),]),
        vec!["pytest".to_string(), "requests".to_string()]
    );
    assert_eq!(
        parse_package_lines(&[
            " bash ".to_string(),
            "".to_string(),
            "coreutils".to_string(),
        ]),
        vec!["bash".to_string(), "coreutils".to_string()]
    );
    assert_eq!(
        parse_blkid_export_attribute(
            &[
                "DEVNAME=/dev/sda1".to_string(),
                "UUID=abcd-1234".to_string(),
                "LABEL=rootfs".to_string(),
                "UUID=efgh-5678".to_string(),
            ],
            "UUID",
        ),
        vec!["abcd-1234".to_string(), "efgh-5678".to_string()]
    );
    assert_eq!(
        parse_busctl_services(&[
            "NAME PID PROCESS USER CONNECTION UNIT SESSION DESCRIPTION".to_string(),
            "org.freedesktop.login1 1 systemd root - - - Login".to_string(),
            ":1.10 100 demo user - - - App".to_string(),
        ]),
        vec![":1.10".to_string(), "org.freedesktop.login1".to_string()]
    );
    assert_eq!(
        parse_loginctl_sessions(&[
            "SESSION UID USER SEAT TTY".to_string(),
            "2 1000 alice seat0 tty2".to_string(),
        ]),
        vec!["2".to_string()]
    );
    assert_eq!(
        parse_losetup_devices(&["NAME".to_string(), "loop0".to_string()]),
        vec!["/dev/loop0".to_string()]
    );

    assert_eq!(
        parse_nmcli_first_field(&[
            "home-wifi:802-11-wireless".to_string(),
            "eth0:ethernet".to_string(),
        ]),
        vec!["eth0".to_string(), "home-wifi".to_string()]
    );

    assert_eq!(
        parse_nmcli_connected_devices(&[
            "wlan0:connected".to_string(),
            "eth0:disconnected".to_string(),
            "lo:unmanaged".to_string(),
        ]),
        vec!["wlan0".to_string()]
    );

    assert_eq!(
        parse_lsblk_devices(&[
            "sda disk".to_string(),
            "sda1 part".to_string(),
            "sr0 rom".to_string(),
        ]),
        vec!["/dev/sda".to_string(), "/dev/sda1".to_string()]
    );
    assert_eq!(
        parse_fstab_mountpoints(
            "# comment\nUUID=abc / ext4 defaults 0 1\nserver:/share /mnt/with\\040space nfs defaults 0 0\n"
        ),
        vec!["/".to_string(), "/mnt/with space".to_string()]
    );

    assert_eq!(
        git::parse_remote_branches(
            &[
                "origin/main".to_string(),
                "upstream/dev".to_string(),
                "origin/HEAD".to_string(),
            ],
            Some("origin"),
        ),
        vec!["main".to_string()]
    );
    assert_eq!(
        git::parse_stash_refs(&[
            "stash@{0}: WIP on main".to_string(),
            "stash@{1}: On dev".to_string(),
        ]),
        vec!["stash@{0}".to_string(), "stash@{1}".to_string()]
    );
    assert_eq!(
        git::parse_status_porcelain_paths(" M src/lib.rs\0R  src/new.rs\0src/old.rs\0"),
        vec!["src/lib.rs".to_string(), "src/new.rs".to_string()]
    );
}

#[test]
fn wireguard_config_names_strip_conf_suffix_and_ignore_other_files() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("wg0.conf"), "").unwrap();
    fs::write(dir.path().join("wg-dev.conf"), "").unwrap();
    fs::write(dir.path().join("notes.txt"), "").unwrap();
    fs::create_dir(dir.path().join("nested.conf")).unwrap();

    assert_eq!(
        collect_wireguard_config_names_from_dirs([dir.path()]),
        vec!["wg-dev".to_string(), "wg0".to_string()]
    );
}

#[test]
fn journalctl_user_unit_completion_queries_user_systemd_units() {
    let _guard = crate::completion::subprocess::external_process_test_guard();
    let dir = tempdir().unwrap();
    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable_script(
        &bin_dir.join("systemctl"),
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > systemctl-args.txt\nprintf 'ssh.service loaded active running SSH\\n'\n",
    );

    let environment = Environment::new();
    {
        let mut env = environment.write();
        env.variable_state.paths = vec![bin_dir.display().to_string()];
        env.clear_command_cache();
    }
    let provider = DynamicCompletionProvider::new(environment);
    let cold = provider.collect_declared_dynamic_candidates(
        "systemctl.unit",
        None,
        &parsed("journalctl --user-unit ssh"),
        dir.path(),
        CachePolicy::RefreshInBackground,
    );
    assert!(cold.is_empty(), "cold provider must not block TAB");
    assert!(wait_until(Duration::from_secs(20), || {
        provider
            .collect_declared_dynamic_candidates(
                "systemctl.unit",
                None,
                &parsed("journalctl --user-unit ssh"),
                dir.path(),
                CachePolicy::CachedOnly,
            )
            .iter()
            .any(|candidate| candidate.text == "ssh.service")
    }));
    let candidates = provider.collect_declared_dynamic_candidates(
        "systemctl.unit",
        None,
        &parsed("journalctl --user-unit ssh"),
        dir.path(),
        CachePolicy::CachedOnly,
    );

    assert!(
        candidates
            .iter()
            .any(|candidate| candidate.text == "ssh.service"),
        "expected user unit candidate in {:?}",
        candidates
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("systemctl-args.txt")).unwrap(),
        "--user\nlist-units\n--all\n--no-pager\n--no-legend\n"
    );
}

#[test]
fn package_json_dependency_parser_collects_all_dependency_groups() {
    let dir = tempdir().unwrap();
    let package_json = dir.path().join("package.json");
    fs::write(
        &package_json,
        r#"{
                "dependencies": { "react": "latest" },
                "devDependencies": { "vite": "latest" },
                "optionalDependencies": { "fsevents": "latest" },
                "peerDependencies": { "@types/react": "latest" }
            }"#,
    )
    .unwrap();

    assert_eq!(
        load_package_json_dependencies(&package_json),
        vec![
            "@types/react".to_string(),
            "fsevents".to_string(),
            "react".to_string(),
            "vite".to_string()
        ]
    );
}

#[test]
fn dynamic_command_cache_is_scoped_by_command_and_kind() {
    let provider = DynamicCompletionProvider::new(Environment::new());
    let scope = PathBuf::from("/tmp/dsh-cache-scope");

    let first_miss = provider.collect_cached_value_candidates(
        "alpha",
        "value",
        scope.clone(),
        "o",
        "test",
        false,
        || Ok(vec!["one".to_string()]),
    );
    let second_miss = provider.collect_cached_value_candidates(
        "beta",
        "value",
        scope.clone(),
        "t",
        "test",
        false,
        || Ok(vec!["two".to_string()]),
    );
    let different_kind_miss = provider.collect_cached_value_candidates(
        "alpha",
        "other",
        scope.clone(),
        "t",
        "test",
        false,
        || Ok(vec!["three".to_string()]),
    );

    assert!(first_miss.is_empty());
    assert!(second_miss.is_empty());
    assert!(different_kind_miss.is_empty());
    assert!(wait_until(Duration::from_secs(20), || {
        provider.cache.read().commands.len() == 3
    }));

    let first = provider.collect_cached_value_candidates(
        "alpha",
        "value",
        scope.clone(),
        "o",
        "test",
        true,
        || panic!("cached lookup must not run loader"),
    );
    let second = provider.collect_cached_value_candidates(
        "beta",
        "value",
        scope.clone(),
        "t",
        "test",
        true,
        || panic!("cached lookup must not run loader"),
    );
    let different_kind = provider.collect_cached_value_candidates(
        "alpha",
        "other",
        scope,
        "t",
        "test",
        true,
        || panic!("cached lookup must not run loader"),
    );
    assert_eq!(first[0].text, "one");
    assert_eq!(second[0].text, "two");
    assert_eq!(different_kind[0].text, "three");
}

#[test]
fn cached_only_dynamic_values_do_not_run_loader_on_miss() {
    let provider = DynamicCompletionProvider::new(Environment::new());
    let candidates = provider.collect_cached_value_candidates(
        "ghost",
        "value",
        PathBuf::from("/tmp/dsh-cached-only"),
        "",
        "test",
        true,
        || panic!("cached-only lookup must not invoke loader"),
    );

    assert!(candidates.is_empty());
}

#[test]
fn shell_state_dynamic_providers_read_environment_maps() {
    let environment = Environment::new();
    {
        let mut env = environment.write();
        env.variable_state
            .alias
            .insert("gs".to_string(), "git status".to_string());
        env.variable_state
            .abbreviations
            .insert("gco".to_string(), "git checkout".to_string());
        env.variable_state
            .system_env_vars
            .insert("DSH_TEST_ENV".to_string(), "1".to_string());
    }
    let provider = DynamicCompletionProvider::new(environment);

    assert!(
        provider
            .collect_declared_dynamic_candidates(
                "shell.alias",
                None,
                &parsed("unalias g"),
                Path::new("/tmp"),
                CachePolicy::RefreshInBackground,
            )
            .iter()
            .any(|candidate| candidate.text == "gs")
    );
    assert!(
        provider
            .collect_declared_dynamic_candidates(
                "shell.abbr",
                None,
                &parsed("abbr erase gc"),
                Path::new("/tmp"),
                CachePolicy::RefreshInBackground,
            )
            .iter()
            .any(|candidate| candidate.text == "gco")
    );
    assert!(
        provider
            .collect_declared_dynamic_candidates(
                "shell.env_var",
                None,
                &parsed("unset DSH_TEST"),
                Path::new("/tmp"),
                CachePolicy::RefreshInBackground,
            )
            .iter()
            .any(|candidate| candidate.text == "DSH_TEST_ENV")
    );
}

#[test]
fn helm_release_completion_passes_namespace_and_context() {
    let _guard = crate::completion::subprocess::external_process_test_guard();
    let dir = tempdir().unwrap();
    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable_script(
        &bin_dir.join("helm"),
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > helm-args.txt\nprintf 'api\\nworker\\n'\n",
    );

    let environment = Environment::new();
    {
        let mut env = environment.write();
        env.variable_state.paths = vec![bin_dir.display().to_string()];
        env.clear_command_cache();
    }
    let provider = DynamicCompletionProvider::new(environment);
    let cold = provider.collect_declared_dynamic_candidates(
        "helm.release",
        None,
        &parsed("helm --kube-context prod -n apps status ap"),
        dir.path(),
        CachePolicy::RefreshInBackground,
    );
    assert!(cold.is_empty(), "cold provider must not block TAB");
    assert!(wait_until(Duration::from_secs(20), || {
        provider
            .collect_declared_dynamic_candidates(
                "helm.release",
                None,
                &parsed("helm --kube-context prod -n apps status ap"),
                dir.path(),
                CachePolicy::CachedOnly,
            )
            .iter()
            .any(|candidate| candidate.text == "api")
    }));
    let candidates = provider.collect_declared_dynamic_candidates(
        "helm.release",
        None,
        &parsed("helm --kube-context prod -n apps status ap"),
        dir.path(),
        CachePolicy::CachedOnly,
    );

    assert!(candidates.iter().any(|candidate| candidate.text == "api"));
    assert_eq!(
        fs::read_to_string(dir.path().join("helm-args.txt")).unwrap(),
        "list\n--short\n--namespace\napps\n--kube-context\nprod\n"
    );
}

#[test]
fn mount_cached_candidates_include_fstab_mountpoints() {
    let provider = DynamicCompletionProvider::new(Environment::new());
    provider.cache.write().commands.insert(
        DynamicCommandCacheKey {
            kind: DynamicCommandCacheKind::CommandValue {
                command: "fstab".to_string(),
                value_kind: "mountpoint".to_string(),
            },
            scope_dir: PathBuf::from("/etc/fstab"),
        },
        CommandValueCacheEntry {
            values: vec!["/mnt/data".to_string(), "/srv/share".to_string()],
            cached_at: Instant::now(),
            last_load_duration: None,
            last_error: None,
        },
    );

    let candidates = provider.collect_mount_candidates(
        &parsed("mount /mnt"),
        Path::new("/tmp"),
        CachePolicy::CachedOnly,
    );

    assert!(
        candidates
            .iter()
            .any(|candidate| candidate.text == "/mnt/data")
    );
}

#[test]
fn parse_compose_services_reads_top_level_service_names() {
    let dir = tempdir().unwrap();
    let compose_file = dir.path().join("compose.yaml");
    fs::write(
        &compose_file,
        r#"
services:
  api:
    image: example/api
  worker:
    build: .
volumes:
  cache:
"#,
    )
    .unwrap();

    let services = parse_compose_service_names(&compose_file).unwrap();
    assert_eq!(services, vec!["api".to_string(), "worker".to_string()]);
}

#[test]
fn task_completion_cache_refreshes_when_taskfile_changes() {
    let dir = tempdir().unwrap();
    let nested = dir.path().join("apps").join("api");
    fs::create_dir_all(&nested).unwrap();
    fs::write(
        dir.path().join("Taskfile.yml"),
        "version: '3'\ntasks:\n  build:\n    cmds:\n      - cargo build\n",
    )
    .unwrap();

    let provider = DynamicCompletionProvider::new(Environment::new());
    let build_candidates = provider.collect_task_candidates(
        &parsed("task bu"),
        &nested,
        CachePolicy::RefreshInBackground,
    );

    assert!(
        build_candidates
            .iter()
            .any(|candidate| candidate.text == "build"),
        "expected task completion from project root"
    );

    std::thread::sleep(Duration::from_millis(20));
    fs::write(
            dir.path().join("Taskfile.yml"),
            "version: '3'\ntasks:\n  build:\n    cmds:\n      - cargo build\n  test:\n    cmds:\n      - cargo test\n",
        )
        .unwrap();

    let test_candidates = provider.collect_task_candidates(
        &parsed("task te"),
        &nested,
        CachePolicy::RefreshInBackground,
    );

    assert!(
        test_candidates
            .iter()
            .any(|candidate| candidate.text == "test"),
        "expected task cache invalidation after Taskfile change"
    );
}

#[test]
fn project_task_cached_only_reuses_existing_cache_without_loading() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("Taskfile.yml"),
        "version: '3'\ntasks:\n  build:\n    cmds:\n      - cargo build\n",
    )
    .unwrap();

    let provider = DynamicCompletionProvider::new(Environment::new());
    assert!(
        provider
            .collect_task_candidates(&parsed("bun run bu"), dir.path(), CachePolicy::CachedOnly,)
            .is_empty(),
        "cached-only task completion should not load on miss"
    );

    let loaded = provider.collect_task_candidates(
        &parsed("bun run bu"),
        dir.path(),
        CachePolicy::RefreshInBackground,
    );
    assert!(loaded.iter().any(|candidate| candidate.text == "build"));

    let cached = provider.collect_task_candidates(
        &parsed("bun run bu"),
        dir.path(),
        CachePolicy::CachedOnly,
    );
    assert!(cached.iter().any(|candidate| candidate.text == "build"));
}

#[test]
fn compose_service_cache_refreshes_when_compose_file_changes() {
    let dir = tempdir().unwrap();
    let nested = dir.path().join("services").join("api");
    fs::create_dir_all(&nested).unwrap();
    fs::write(
        dir.path().join("compose.yaml"),
        "services:\n  api:\n    image: example/api\n",
    )
    .unwrap();

    let provider = DynamicCompletionProvider::new(Environment::new());
    let api_candidates = provider.collect_compose_service_candidates(&nested, "ap", None);

    assert!(
        api_candidates
            .iter()
            .any(|candidate| candidate.text == "api"),
        "expected compose service completion from ancestor compose file"
    );

    std::thread::sleep(Duration::from_millis(20));
    fs::write(
        dir.path().join("compose.yaml"),
        "services:\n  api:\n    image: example/api\n  worker:\n    image: example/worker\n",
    )
    .unwrap();

    let worker_candidates = provider.collect_compose_service_candidates(&nested, "wo", None);
    assert!(
        worker_candidates
            .iter()
            .any(|candidate| candidate.text == "worker"),
        "expected compose cache invalidation after file change"
    );
}

#[test]
fn docker_compose_service_completion_uses_explicit_file_option() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("compose.yaml"),
        "services:\n  api:\n    image: example/api\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("compose.dev.yml"),
        "services:\n  worker:\n    image: example/worker\n",
    )
    .unwrap();

    let provider = DynamicCompletionProvider::new(Environment::new());
    let candidates = provider.collect_docker_candidates(
        &parsed("docker compose -f compose.dev.yml up wo"),
        dir.path(),
        CachePolicy::RefreshInBackground,
    );

    assert!(
        candidates
            .iter()
            .any(|candidate| candidate.text == "worker"),
        "expected service completion from docker compose -f file"
    );
    assert!(
        candidates.iter().all(|candidate| candidate.text != "api"),
        "explicit compose file should take precedence over ancestor defaults"
    );
}

#[test]
fn docker_compose_service_completion_uses_standalone_explicit_file_option() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("compose.yaml"),
        "services:\n  api:\n    image: example/api\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("compose.dev.yml"),
        "services:\n  worker:\n    image: example/worker\n",
    )
    .unwrap();

    let provider = DynamicCompletionProvider::new(Environment::new());
    let parsed = parsed("docker-compose -f compose.dev.yml up wo");
    assert_eq!(selected_docker_compose_command(&parsed), Some("up"));

    let candidates = provider.collect_declared_dynamic_candidates(
        "docker.compose_service",
        None,
        &parsed,
        dir.path(),
        CachePolicy::RefreshInBackground,
    );

    assert!(
        candidates
            .iter()
            .any(|candidate| candidate.text == "worker"),
        "expected service completion from standalone docker-compose -f file"
    );
    assert!(
        candidates.iter().all(|candidate| candidate.text != "api"),
        "explicit compose file should take precedence over ancestor defaults"
    );
}

#[test]
fn docker_compose_service_completion_allows_docker_global_option_before_compose() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("compose.yaml"),
        "services:\n  api:\n    image: example/api\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("compose.dev.yml"),
        "services:\n  worker:\n    image: example/worker\n",
    )
    .unwrap();

    let provider = DynamicCompletionProvider::new(Environment::new());
    let candidates = provider.collect_docker_candidates(
        &parsed("docker --context prod compose -f compose.dev.yml up wo"),
        dir.path(),
        CachePolicy::RefreshInBackground,
    );

    assert!(
        candidates
            .iter()
            .any(|candidate| candidate.text == "worker"),
        "expected service completion when docker global option precedes compose"
    );
    assert!(
        candidates.iter().all(|candidate| candidate.text != "api"),
        "explicit compose file should take precedence over ancestor defaults"
    );
}

#[test]
fn docker_compose_service_completion_allows_inline_docker_global_option_before_compose() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("compose.yaml"),
        "services:\n  api:\n    image: example/api\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("compose.dev.yml"),
        "services:\n  worker:\n    image: example/worker\n",
    )
    .unwrap();

    let provider = DynamicCompletionProvider::new(Environment::new());
    let candidates = provider.collect_docker_candidates(
        &parsed("docker --context=prod compose -f compose.dev.yml up wo"),
        dir.path(),
        CachePolicy::RefreshInBackground,
    );

    assert!(
        candidates
            .iter()
            .any(|candidate| candidate.text == "worker"),
        "expected service completion when inline docker global option precedes compose"
    );
    assert!(
        candidates.iter().all(|candidate| candidate.text != "api"),
        "explicit compose file should take precedence over ancestor defaults"
    );
}

#[test]
fn nx_task_cache_refreshes_when_descendant_project_json_changes() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("workspace.json"), r#"{ "projects": {} }"#).unwrap();
    let api_dir = dir.path().join("apps").join("api");
    fs::create_dir_all(&api_dir).unwrap();
    fs::write(
        api_dir.join("project.json"),
        r#"{ "name": "api", "targets": { "build": {} } }"#,
    )
    .unwrap();

    let provider = DynamicCompletionProvider::new(Environment::new());
    let build_candidates = provider.collect_project_task_candidates_for_sources_with_mode(
        &parsed("nx run api:bu"),
        dir.path(),
        NX_PROJECT_TASK_SOURCES,
        false,
        ProjectTaskCandidateText::Name,
    );
    assert!(
        build_candidates
            .iter()
            .any(|candidate| candidate.text == "api:build"),
        "expected Nx task completion from descendant project.json"
    );

    std::thread::sleep(Duration::from_millis(20));
    fs::write(
        api_dir.join("project.json"),
        r#"{ "name": "api", "targets": { "build": {}, "test": {} } }"#,
    )
    .unwrap();

    let test_candidates = provider.collect_project_task_candidates_for_sources_with_mode(
        &parsed("nx run api:te"),
        dir.path(),
        NX_PROJECT_TASK_SOURCES,
        false,
        ProjectTaskCandidateText::Name,
    );
    assert!(
        test_candidates
            .iter()
            .any(|candidate| candidate.text == "api:test"),
        "expected task cache invalidation after descendant project.json changes"
    );
}

#[test]
fn non_nx_source_task_signature_ignores_descendant_project_json_changes() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("package.json"),
        r#"{ "scripts": { "build": "vite build" } }"#,
    )
    .unwrap();
    let api_dir = dir.path().join("apps").join("api");
    fs::create_dir_all(&api_dir).unwrap();
    fs::write(
        api_dir.join("project.json"),
        r#"{ "name": "api", "targets": { "build": {} } }"#,
    )
    .unwrap();

    let before = task_completion_signature(dir.path(), Some(JS_PROJECT_TASK_SOURCES));
    std::thread::sleep(Duration::from_millis(20));
    fs::write(
        api_dir.join("project.json"),
        r#"{ "name": "api", "targets": { "build": {}, "test": {} } }"#,
    )
    .unwrap();
    let after = task_completion_signature(dir.path(), Some(JS_PROJECT_TASK_SOURCES));

    assert_eq!(
        before, after,
        "non-Nx source-scoped task signatures should not scan descendant project.json files"
    );
}

#[test]
fn kubectl_resource_name_completion_respects_namespace_and_resource_name_token() {
    let _guard = crate::completion::subprocess::external_process_test_guard();
    let dir = tempdir().unwrap();
    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let kubectl = bin_dir.join("kubectl");
    write_executable_script(
        &kubectl,
        "#!/bin/sh\n\
             printf '%s\\n' \"$@\" > kubectl-args.txt\n\
             resource=''\n\
             if [ \"$1\" = get ] && [ \"$2\" = -n ]; then\n\
               resource=\"$4\"\n\
             elif [ \"$1\" = get ]; then\n\
               resource=\"$2\"\n\
             fi\n\
             case \"$resource\" in\n\
               pod|pods) printf 'api\\nworker\\n' ;;\n\
             esac\n",
    );
    let environment = Environment::new();
    {
        let mut env = environment.write();
        env.variable_state.paths = vec![bin_dir.display().to_string()];
        env.clear_command_cache();
    }
    let provider = DynamicCompletionProvider::new(environment);

    let cold = provider.collect_kubectl_candidates(
        &parsed("kubectl get -n prod pods ap"),
        dir.path(),
        CachePolicy::RefreshInBackground,
    );
    assert!(cold.is_empty(), "cold provider must not block TAB");
    assert!(wait_until(Duration::from_secs(20), || {
        provider
            .collect_kubectl_candidates(
                &parsed("kubectl get -n prod pods ap"),
                dir.path(),
                CachePolicy::RefreshInBackground,
            )
            .iter()
            .any(|candidate| candidate.text == "api")
    }));
    let names = provider.collect_kubectl_candidates(
        &parsed("kubectl get -n prod pods ap"),
        dir.path(),
        CachePolicy::RefreshInBackground,
    );
    assert!(
        names.iter().any(|candidate| candidate.text == "api"),
        "expected resource names to be queried inside the selected namespace"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("kubectl-args.txt")).unwrap(),
        "get\n-n\nprod\npods\n-o\njsonpath={range .items[*]}{.metadata.name}{\"\\n\"}{end}\n"
    );

    let resource_name_cold = provider.collect_kubectl_candidates(
        &parsed("kubectl get pods/ap"),
        dir.path(),
        CachePolicy::RefreshInBackground,
    );
    assert!(
        resource_name_cold.is_empty(),
        "a new namespace/resource cache key should schedule a worker"
    );
    assert!(wait_until(Duration::from_secs(20), || {
        provider
            .collect_kubectl_candidates(
                &parsed("kubectl get pods/ap"),
                dir.path(),
                CachePolicy::RefreshInBackground,
            )
            .iter()
            .any(|candidate| candidate.text == "pods/api")
    }));
    let resource_name = provider.collect_kubectl_candidates(
        &parsed("kubectl get pods/ap"),
        dir.path(),
        CachePolicy::RefreshInBackground,
    );
    assert!(
        resource_name
            .iter()
            .any(|candidate| candidate.text == "pods/api"),
        "expected resource/name token completion to preserve the resource prefix"
    );
}

#[test]
fn git_branch_cache_is_shared_per_project_root_and_expires_by_ttl() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("repo");
    let nested = root.join("apps").join("web");
    fs::create_dir_all(&nested).unwrap();
    fs::write(root.join("package.json"), "{\"name\":\"demo\"}\n").unwrap();

    let provider = DynamicCompletionProvider::new(Environment::new());
    let load_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fake_loader = || {
        let load_count = Arc::clone(&load_count);
        move || {
            load_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(vec!["feature/cache".to_string(), "main".to_string()])
        }
    };
    let root_scope = provider.cached_project_root(&root);

    let cold_candidates = provider.collect_cached_command_candidates(
        DynamicCommandCacheKind::GitBranch,
        root_scope.clone(),
        "fe",
        "git branch",
        false,
        fake_loader(),
    );
    assert!(
        cold_candidates.is_empty(),
        "first miss should schedule git branch completion"
    );
    assert!(wait_until(Duration::from_secs(20), || {
        provider
            .collect_cached_command_candidates(
                DynamicCommandCacheKind::GitBranch,
                root_scope.clone(),
                "fe",
                "git branch",
                true,
                fake_loader(),
            )
            .iter()
            .any(|candidate| candidate.text == "feature/cache")
    }));
    let first_candidates = provider.collect_cached_command_candidates(
        DynamicCommandCacheKind::GitBranch,
        root_scope,
        "fe",
        "git branch",
        true,
        fake_loader(),
    );
    assert!(
        first_candidates
            .iter()
            .any(|candidate| candidate.text == "feature/cache"),
        "worker result should be available from the cache"
    );

    let nested_scope = provider.cached_project_root(&nested);
    let nested_candidates = provider.collect_cached_command_candidates(
        DynamicCommandCacheKind::GitBranch,
        nested_scope.clone(),
        "ma",
        "git branch",
        false,
        fake_loader(),
    );
    assert!(
        nested_candidates
            .iter()
            .any(|candidate| candidate.text == "main"),
        "expected cached git branch completion from nested cwd"
    );

    assert_eq!(
        load_count.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "git command should run once within the same project root"
    );

    std::thread::sleep(Duration::from_millis(DYNAMIC_COMMAND_CACHE_TTL_MS + 50));
    let refreshed_candidates = provider.collect_cached_command_candidates(
        DynamicCommandCacheKind::GitBranch,
        nested_scope,
        "fe",
        "git branch",
        false,
        fake_loader(),
    );
    assert!(
        refreshed_candidates
            .iter()
            .any(|candidate| candidate.text == "feature/cache"),
        "expected stale git branch completion while ttl refresh runs"
    );

    assert!(
        wait_until(Duration::from_secs(20), || {
            load_count.load(std::sync::atomic::Ordering::SeqCst) == 2
        }),
        "git command should run again after ttl expiry"
    );
}

#[test]
fn external_completer_parses_filters_and_receives_context() {
    let _guard = crate::completion::subprocess::external_process_test_guard();
    let dir = tempdir().unwrap();
    let script = dir.path().join("external-completer.sh");
    write_executable_script(
        &script,
        "#!/bin/sh\n\
             {\n\
             printf 'input=%s\\n' \"$DSH_COMPLETION_INPUT\"\n\
             printf 'cursor=%s\\n' \"$DSH_COMPLETION_CURSOR\"\n\
             printf 'command=%s\\n' \"$DSH_COMPLETION_COMMAND\"\n\
             printf 'token=%s\\n' \"$DSH_COMPLETION_CURRENT_TOKEN\"\n\
             } > external-env.txt\n\
             printf 'zzext-alpha\\tExternal alpha\\n'\n\
             printf 'other-candidate\\tOther candidate\\n'\n",
    );

    let environment = Environment::new();
    environment.write().variable_state.variables.insert(
        "DSH_EXTERNAL_COMPLETER".to_string(),
        script.display().to_string(),
    );
    let provider = DynamicCompletionProvider::new(environment);
    let input = "unknown-command zzext";

    let cold = provider.collect_external_candidates(dir.path(), input, input.len(), &parsed(input));
    assert!(
        cold.is_empty(),
        "cold external completer must not block TAB"
    );
    assert!(wait_until(Duration::from_secs(20), || {
        !provider
            .collect_external_candidates(dir.path(), input, input.len(), &parsed(input))
            .is_empty()
    }));
    let candidates =
        provider.collect_external_candidates(dir.path(), input, input.len(), &parsed(input));

    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].text, "zzext-alpha");
    assert_eq!(candidates[0].description.as_deref(), Some("External alpha"));
    assert_eq!(candidates[0].candidate_type, CandidateType::Argument);
    assert_eq!(candidates[0].priority, 200);

    assert_eq!(
        fs::read_to_string(dir.path().join("external-env.txt")).unwrap(),
        "input=unknown-command zzext\ncursor=21\ncommand=unknown-command\ntoken=zzext\n"
    );
}

#[test]
fn external_completer_parses_jsonl_candidates_and_legacy_fallback() {
    let json_candidate = external::parse_line(
            r#"{"text":"display alpha","description":"JSON alpha","type":"long-option","priority":240,"replacement":"--alpha"}"#,
            "--al",
        )
        .unwrap();
    assert_eq!(json_candidate.text, "--alpha");
    assert_eq!(json_candidate.description.as_deref(), Some("JSON alpha"));
    assert_eq!(json_candidate.candidate_type, CandidateType::LongOption);
    assert_eq!(json_candidate.priority, 240);

    let invalid_json_candidate = external::parse_line("{not-json", "{not").unwrap();
    assert_eq!(invalid_json_candidate.text, "{not-json");
    assert_eq!(
        invalid_json_candidate.candidate_type,
        CandidateType::Argument
    );

    let legacy_candidate = external::parse_line("zzext-alpha\tExternal alpha", "zz").unwrap();
    assert_eq!(legacy_candidate.text, "zzext-alpha");
    assert_eq!(
        legacy_candidate.description.as_deref(),
        Some("External alpha")
    );
}

#[test]
fn fish_fallback_parses_tab_descriptions_and_low_priority() {
    let candidate = external::parse_fish_line("checkout\tSwitch branches", "che").unwrap();
    assert_eq!(candidate.text, "checkout");
    assert_eq!(candidate.description.as_deref(), Some("Switch branches"));
    assert_eq!(candidate.candidate_type, CandidateType::Argument);
    assert_eq!(candidate.priority, 35);

    let option = external::parse_fish_line("--help\tShow help", "--he").unwrap();
    assert_eq!(option.candidate_type, CandidateType::LongOption);

    assert!(external::matches_fish_prefix("'zz", "zz/"));
    let quoted_path = external::parse_fish_line("zz/\tQuoted path", "'zz").unwrap();
    assert_eq!(quoted_path.candidate_type, CandidateType::Directory);
    assert_eq!(quoted_path.description.as_deref(), Some("Quoted path"));

    assert!(external::parse_fish_line("checkout\tSwitch branches", "zz").is_none());
}

#[test]
fn fish_fallback_auto_requires_fish_command_and_respects_disable() {
    let _guard = crate::completion::subprocess::external_process_test_guard();
    let dir = tempdir().unwrap();
    let environment = Environment::new();
    {
        let mut env = environment.write();
        env.variable_state.paths = vec![];
        env.clear_command_cache();
    }
    let provider = DynamicCompletionProvider::new(environment);
    let input = "git che";
    assert!(
        provider
            .collect_fish_fallback_candidates(dir.path(), input, input.len(), &parsed(input))
            .is_empty(),
        "auto fallback still needs fish on PATH"
    );

    let environment = Environment::new();
    {
        let mut env = environment.write();
        env.variable_state.paths = vec![];
        env.variable_state
            .variables
            .insert("DSH_COMPLETION_FISH_FALLBACK".to_string(), "1".to_string());
        env.clear_command_cache();
    }
    let provider = DynamicCompletionProvider::new(environment);
    assert!(
        provider
            .collect_fish_fallback_candidates(dir.path(), input, input.len(), &parsed(input))
            .is_empty(),
        "enabled fallback still needs fish on PATH"
    );

    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable_script(
        &bin_dir.join("fish"),
        "#!/bin/sh\nprintf 'checkout\\tFish checkout\\n'\n",
    );
    let disabled_environment = Environment::new();
    {
        let mut env = disabled_environment.write();
        env.variable_state.paths = vec![bin_dir.display().to_string()];
        env.variable_state
            .variables
            .insert("DSH_COMPLETION_FISH_FALLBACK".to_string(), "0".to_string());
        env.clear_command_cache();
    }
    let provider = DynamicCompletionProvider::new(disabled_environment);
    assert!(
        provider
            .collect_fish_fallback_candidates(dir.path(), input, input.len(), &parsed(input))
            .is_empty(),
        "false-like flag disables fish fallback"
    );
}

#[test]
fn fish_fallback_runs_with_cursor_prefix_and_timeout() {
    let _guard = crate::completion::subprocess::external_process_test_guard();
    let dir = tempdir().unwrap();
    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let fish = bin_dir.join("fish");
    write_executable_script(
        &fish,
        "#!/bin/sh\nprintf '%s\\n' \"$4\" > fish-input.txt\nprintf 'zzfish-alpha\\tFish alpha\\n'\n",
    );

    let environment = Environment::new();
    {
        let mut env = environment.write();
        env.variable_state.paths = vec![bin_dir.display().to_string()];
        env.clear_command_cache();
    }
    let provider = DynamicCompletionProvider::new(environment);
    let input = "unknown zzfish trailing";
    let cursor = "unknown zzfish".len();

    let parsed_at_cursor = CommandLineParser::new().parse(input, cursor);
    let cold =
        provider.collect_fish_fallback_candidates(dir.path(), input, cursor, &parsed_at_cursor);
    assert!(cold.is_empty(), "cold fish fallback must not block TAB");
    assert!(wait_until(Duration::from_secs(20), || {
        !provider
            .collect_fish_fallback_candidates(dir.path(), input, cursor, &parsed_at_cursor)
            .is_empty()
    }));
    let candidates =
        provider.collect_fish_fallback_candidates(dir.path(), input, cursor, &parsed_at_cursor);

    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].text, "zzfish-alpha");
    assert_eq!(candidates[0].description.as_deref(), Some("Fish alpha"));
    assert_eq!(
        fs::read_to_string(dir.path().join("fish-input.txt")).unwrap(),
        "unknown zzfish\n"
    );

    let slow_dir = tempdir().unwrap();
    let slow_bin = slow_dir.path().join("bin");
    fs::create_dir_all(&slow_bin).unwrap();
    write_executable_script(
        &slow_bin.join("fish"),
        "#!/bin/sh\nsleep 2\nprintf 'late\\n'\n",
    );
    let slow_environment = Environment::new();
    {
        let mut env = slow_environment.write();
        env.variable_state.paths = vec![slow_bin.display().to_string()];
        env.variable_state
            .variables
            .insert("DSH_COMPLETION_FISH_FALLBACK".to_string(), "1".to_string());
        env.clear_command_cache();
    }
    let slow_provider = DynamicCompletionProvider::new(slow_environment);
    let started = Instant::now();
    let slow_candidates = slow_provider.collect_fish_fallback_candidates(
        slow_dir.path(),
        "slow la",
        "slow la".len(),
        &parsed("slow la"),
    );
    assert!(slow_candidates.is_empty());
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "the test runner lock may queue this probe, but it must remain bounded"
    );
}

#[test]
fn timed_out_completion_command_kills_stdout_holding_descendants() {
    let _guard = crate::completion::subprocess::external_process_test_guard();
    let dir = tempdir().unwrap();
    let script = dir.path().join("holds-stdout.sh");
    let survived = dir.path().join("survived.txt");
    write_executable_script(
        &script,
        "#!/bin/sh\n(sleep 2; printf survived > survived.txt) &\nwait\n",
    );

    let started = Instant::now();
    let output = run_command_stdout(script.to_str().unwrap(), &[], dir.path()).unwrap();

    assert_eq!(output, "");
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "the test runner lock may queue this probe, but it must remain bounded"
    );
    std::thread::sleep(Duration::from_millis(1200));
    assert!(
        !survived.exists(),
        "timeout should kill descendant processes that inherited stdout"
    );
}

#[test]
fn external_completer_failure_returns_empty_candidates() {
    let _guard = crate::completion::subprocess::external_process_test_guard();
    let dir = tempdir().unwrap();
    let script = dir.path().join("external-completer-fails.sh");
    write_executable_script(&script, "#!/bin/sh\nexit 42\n");

    let environment = Environment::new();
    environment.write().variable_state.variables.insert(
        "DSH_EXTERNAL_COMPLETER".to_string(),
        script.display().to_string(),
    );
    let provider = DynamicCompletionProvider::new(environment);
    let input = "unknown-command zzext";

    let candidates =
        provider.collect_external_candidates(dir.path(), input, input.len(), &parsed(input));

    assert!(candidates.is_empty());
}

#[test]
fn external_completer_returns_stale_candidates_while_refreshing() {
    let _guard = crate::completion::subprocess::external_process_test_guard();
    let dir = tempdir().unwrap();
    let script = dir.path().join("external-completer-refresh.sh");
    let counter = dir.path().join("external-count");
    write_executable_script(
        &script,
        &format!(
            "#!/bin/sh\n\
                 count_file=\"{}\"\n\
                 count=0\n\
                 if [ -f \"$count_file\" ]; then count=$(cat \"$count_file\"); fi\n\
                 count=$((count + 1))\n\
                 printf '%s' \"$count\" > \"$count_file\"\n\
                 if [ \"$count\" = \"1\" ]; then\n\
                 printf 'zzext-alpha\\tExternal alpha\\n'\n\
                 else\n\
                 printf 'zzext-beta\\tExternal beta\\n'\n\
                 fi\n",
            counter.display()
        ),
    );

    let environment = Environment::new();
    environment.write().variable_state.variables.insert(
        "DSH_EXTERNAL_COMPLETER".to_string(),
        script.display().to_string(),
    );
    let provider = DynamicCompletionProvider::new(environment);
    let input = "unknown-command zzext";

    let cold = provider.collect_external_candidates(dir.path(), input, input.len(), &parsed(input));
    assert!(
        cold.is_empty(),
        "cold external completer must not block TAB"
    );
    assert!(wait_until(Duration::from_secs(20), || {
        provider
            .collect_external_candidates(dir.path(), input, input.len(), &parsed(input))
            .first()
            .is_some_and(|candidate| candidate.text == "zzext-alpha")
    }));
    let first =
        provider.collect_external_candidates(dir.path(), input, input.len(), &parsed(input));
    assert_eq!(first[0].text, "zzext-alpha");

    std::thread::sleep(Duration::from_millis(DYNAMIC_COMMAND_CACHE_TTL_MS + 50));
    let stale =
        provider.collect_external_candidates(dir.path(), input, input.len(), &parsed(input));
    assert_eq!(stale[0].text, "zzext-alpha");

    assert!(
        wait_until(Duration::from_secs(20), || fs::read_to_string(&counter)
            .is_ok_and(|count| count == "2")),
        "external completer should refresh in background"
    );

    assert!(
        wait_until(Duration::from_secs(20), || {
            provider
                .collect_external_candidates(dir.path(), input, input.len(), &parsed(input))
                .first()
                .is_some_and(|candidate| candidate.text == "zzext-beta")
        }),
        "external completer should expose refreshed candidates after background refresh"
    );
}

#[test]
fn external_completion_cache_prunes_oldest_entries() {
    let _guard = crate::completion::subprocess::external_process_test_guard();
    let dir = tempdir().unwrap();
    let script = dir.path().join("external-completer-cache.sh");
    write_executable_script(
        &script,
        "#!/bin/sh\nprintf '%s-candidate\\n' \"$DSH_COMPLETION_CURRENT_TOKEN\"\n",
    );

    let environment = Environment::new();
    environment.write().variable_state.variables.insert(
        "DSH_EXTERNAL_COMPLETER".to_string(),
        script.display().to_string(),
    );
    let provider = DynamicCompletionProvider::new(environment);

    for index in 0..(EXTERNAL_COMPLETION_CACHE_LIMIT + 3) {
        let input = format!("unknown-command zz{index}");
        let expected = format!("zz{index}-candidate");
        assert!(
            wait_until(Duration::from_secs(20), || provider
                .collect_external_candidates(dir.path(), &input, input.len(), &parsed(&input))
                .first()
                .is_some_and(|candidate| candidate.text == expected)),
            "external cache prune setup should load candidate for {input}"
        );
    }

    let cache = provider.cache.read();
    assert_eq!(cache.external.len(), EXTERNAL_COMPLETION_CACHE_LIMIT);
    assert_eq!(cache.external_pruned_total, 3);
}
