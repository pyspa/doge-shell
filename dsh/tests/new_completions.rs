use doge_shell::completion::command::ArgumentType;
use doge_shell::completion::json_loader::JsonCompletionLoader;

use std::path::PathBuf;

// A generic "does this command's completion load and does `command` match the
// file name" sweep over every embedded completion file already lives in
// dsh/src/completion/json_loader/tests.rs
// (test_all_embedded_completion_files_load_correctly,
// embedded_completion_definitions_are_valid). The tests below check something
// that sweep cannot: that a *specific* argument or option on a *specific*
// command resolves to the *right* dynamic provider, not merely a known one.

#[test]
fn test_dev_cli_completions_use_dynamic_providers() {
    let root_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = root_dir.parent().unwrap();
    let completions_dir = repo_root.join("completions");

    let loader = JsonCompletionLoader::with_dirs(vec![completions_dir]);

    let uv = loader
        .load_command_completion("uv")
        .unwrap()
        .expect("uv completion not found in json");
    let uv_remove = uv
        .subcommands
        .iter()
        .find(|sub| sub.name == "remove")
        .expect("missing uv remove");
    assert!(
        matches!(
            uv_remove.arguments.first().and_then(|arg| arg.arg_type.as_ref()),
            Some(ArgumentType::Dynamic { provider, .. }) if provider == "python.project_dependency"
        ),
        "uv remove should complete local Python project dependencies"
    );

    let npx = loader
        .load_command_completion("npx")
        .unwrap()
        .expect("npx completion not found in json");
    assert!(
        matches!(
            npx.arguments.first().and_then(|arg| arg.arg_type.as_ref()),
            Some(ArgumentType::Dynamic { provider, .. }) if provider == "node.bin"
        ),
        "npx should complete local node_modules/.bin commands"
    );

    let go = loader
        .load_command_completion("go")
        .unwrap()
        .expect("go completion not found in json");
    let go_test = go
        .subcommands
        .iter()
        .find(|sub| sub.name == "test")
        .expect("missing go test");
    assert!(
        matches!(
            go_test.arguments.first().and_then(|arg| arg.arg_type.as_ref()),
            Some(ArgumentType::Dynamic { provider, .. }) if provider == "go.package"
        ),
        "go test should complete local Go packages"
    );

    let nx = loader
        .load_command_completion("nx")
        .unwrap()
        .expect("nx completion not found in json");
    let nx_run = nx
        .subcommands
        .iter()
        .find(|sub| sub.name == "run")
        .expect("missing nx run");
    assert!(
        matches!(
            nx_run.arguments.first().and_then(|arg| arg.arg_type.as_ref()),
            Some(ArgumentType::Dynamic { provider, scope }) if provider == "project.task" && scope.as_deref() == Some("nx.run")
        ),
        "nx run should complete qualified Nx run targets"
    );

    let python = loader
        .load_command_completion("python")
        .unwrap()
        .expect("python completion not found in json");
    let python_m = python
        .global_options
        .iter()
        .find(|option| option.short.as_deref() == Some("-m"))
        .expect("missing python -m");
    assert!(
        matches!(
            python_m.value_type(),
            Some(ArgumentType::Dynamic { provider, .. }) if provider == "python.module"
        ),
        "python -m should complete importable modules"
    );

    let npm = loader
        .load_command_completion("npm")
        .unwrap()
        .expect("npm completion not found in json");
    let npm_workspace = npm
        .global_options
        .iter()
        .find(|option| option.long.as_deref() == Some("--workspace"))
        .expect("missing npm --workspace");
    assert!(
        matches!(
            npm_workspace.value_type(),
            Some(ArgumentType::Dynamic { provider, .. }) if provider == "node.workspace"
        ),
        "npm --workspace should complete local workspaces"
    );

    let aws = loader
        .load_command_completion("aws")
        .unwrap()
        .expect("aws completion not found in json");
    let profile = aws
        .global_options
        .iter()
        .find(|option| option.long.as_deref() == Some("--profile"))
        .expect("missing aws --profile");
    assert!(
        matches!(
            profile.value_type(),
            Some(ArgumentType::Dynamic { provider, .. }) if provider == "aws.profile"
        ),
        "aws --profile should complete local AWS profiles"
    );

    let gcloud = loader
        .load_command_completion("gcloud")
        .unwrap()
        .expect("gcloud completion not found in json");
    let configuration = gcloud
        .global_options
        .iter()
        .find(|option| option.long.as_deref() == Some("--configuration"))
        .expect("missing gcloud --configuration");
    let project = gcloud
        .global_options
        .iter()
        .find(|option| option.long.as_deref() == Some("--project"))
        .expect("missing gcloud --project");
    assert!(
        matches!(
            configuration.value_type(),
            Some(ArgumentType::Dynamic { provider, .. }) if provider == "gcloud.configuration"
        ),
        "gcloud --configuration should complete local configurations"
    );
    assert!(
        matches!(
            project.value_type(),
            Some(ArgumentType::Dynamic { provider, .. }) if provider == "gcloud.project"
        ),
        "gcloud --project should complete local projects"
    );

    let terraform = loader
        .load_command_completion("terraform")
        .unwrap()
        .expect("terraform completion not found in json");
    let workspace = terraform
        .subcommands
        .iter()
        .find(|sub| sub.name == "workspace")
        .expect("missing terraform workspace");
    let select = workspace
        .subcommands
        .iter()
        .find(|sub| sub.name == "select")
        .expect("missing terraform workspace select");
    assert!(
        matches!(
            select.arguments.first().and_then(|arg| arg.arg_type.as_ref()),
            Some(ArgumentType::Dynamic { provider, .. }) if provider == "terraform.workspace"
        ),
        "terraform workspace select should complete local Terraform workspaces"
    );

    let podman = loader
        .load_command_completion("podman")
        .unwrap()
        .expect("podman completion not found in json");
    let podman_run = podman
        .subcommands
        .iter()
        .find(|sub| sub.name == "run")
        .expect("missing podman run");
    assert!(
        matches!(
            podman_run.arguments.first().and_then(|arg| arg.arg_type.as_ref()),
            Some(ArgumentType::Dynamic { provider, .. }) if provider == "podman.image"
        ),
        "podman run should complete local images"
    );

    let docker = loader
        .load_command_completion("docker")
        .unwrap()
        .expect("docker completion not found in json");
    let network = docker
        .subcommands
        .iter()
        .find(|sub| sub.name == "network")
        .expect("missing docker network");
    let network_rm = network
        .subcommands
        .iter()
        .find(|sub| sub.name == "rm")
        .expect("missing docker network rm");
    assert!(
        matches!(
            network_rm.arguments.first().and_then(|arg| arg.arg_type.as_ref()),
            Some(ArgumentType::Dynamic { provider, .. }) if provider == "docker.network"
        ),
        "docker network rm should complete local Docker networks"
    );
}

#[test]
fn test_basic_commands_use_precise_dynamic_and_value_completions() {
    let root_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = root_dir.parent().unwrap();
    let completions_dir = repo_root.join("completions");

    let loader = JsonCompletionLoader::with_dirs(vec![completions_dir]);
    let chown_completion = loader
        .load_command_completion("chown")
        .unwrap()
        .expect("chown completion not found in json");

    let owner_group = chown_completion
        .arguments
        .first()
        .expect("missing chown owner/group argument");
    assert!(
        matches!(
            owner_group.arg_type,
            Some(ArgumentType::Dynamic { ref provider, .. })
                if provider == "system.owner_group"
        ),
        "chown owner/group should complete owner[:group] values"
    );

    let from = chown_completion
        .global_options
        .iter()
        .find(|option| option.long.as_deref() == Some("--from"))
        .expect("missing chown --from option");
    assert!(
        matches!(
            from.value_type(),
            Some(ArgumentType::Dynamic { provider, .. })
                if provider == "system.owner_group"
        ),
        "chown --from should complete owner[:group] values"
    );

    for (command, provider) in [
        ("man", "man.page"),
        ("fg", "shell.job"),
        ("bg", "shell.job"),
        ("wait", "shell.job"),
        ("tar", "archive.entry"),
    ] {
        let completion = loader
            .load_command_completion(command)
            .unwrap()
            .unwrap_or_else(|| panic!("{command} completion not found in json"));
        assert!(
            completion.arguments.iter().any(|argument| matches!(
                argument.arg_type,
                Some(ArgumentType::Dynamic {
                    provider: ref actual,
                    ..
                }) if actual == provider
            )),
            "{command} should use dynamic provider {provider}"
        );
    }

    let unzip = loader
        .load_command_completion("unzip")
        .unwrap()
        .expect("unzip completion not found in json");
    assert!(
        matches!(
            unzip.arguments.get(1).and_then(|argument| argument.arg_type.as_ref()),
            Some(ArgumentType::Dynamic { provider, .. }) if provider == "archive.entry"
        ),
        "unzip members should use archive entry completion"
    );

    for (command, expected_type) in [
        ("cat", ArgumentType::File { extensions: None }),
        ("mkdir", ArgumentType::Directory),
    ] {
        let completion = loader
            .load_command_completion(command)
            .unwrap()
            .unwrap_or_else(|| panic!("{command} completion not found in json"));
        assert_eq!(
            completion
                .arguments
                .first()
                .and_then(|argument| argument.arg_type.clone()),
            Some(expected_type),
            "{command} should use a filesystem-aware argument type"
        );
    }
}

#[test]
fn test_wait_completion_offers_next_option_and_job_arguments() {
    let root_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = root_dir.parent().unwrap();
    let completions_dir = repo_root.join("completions");

    let loader = JsonCompletionLoader::with_dirs(vec![completions_dir]);
    let wait = loader
        .load_command_completion("wait")
        .unwrap()
        .expect("wait completion not found in json");

    assert!(
        wait.global_options
            .iter()
            .any(|option| option.short.as_deref() == Some("-n")),
        "wait should offer -n for wait-any"
    );
    let id = wait.arguments.first().expect("missing wait id argument");
    assert!(
        id.multiple,
        "wait id arg must accept several PIDs/job specs"
    );
    assert!(
        matches!(
            id.arg_type,
            Some(ArgumentType::Dynamic { ref provider, .. }) if provider == "shell.job"
        ),
        "wait id should complete shell jobs"
    );
}

#[test]
fn test_jobs_and_bg_completion_cover_job_specs() {
    let root_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = root_dir.parent().unwrap();
    let completions_dir = repo_root.join("completions");

    let loader = JsonCompletionLoader::with_dirs(vec![completions_dir]);

    let jobs = loader
        .load_command_completion("jobs")
        .unwrap()
        .expect("jobs completion not found in json");
    for (short, long) in [("-l", "--list"), ("-p", "--pgid")] {
        assert!(
            jobs.global_options
                .iter()
                .any(|option| option.short.as_deref() == Some(short)
                    && option.long.as_deref() == Some(long)),
            "jobs should offer {short}/{long}"
        );
    }
    let job_arg = jobs.arguments.first().expect("missing jobs job argument");
    assert!(!job_arg.multiple, "jobs accepts at most one jobspec");
    assert!(
        matches!(
            job_arg.arg_type,
            Some(ArgumentType::Dynamic { ref provider, .. }) if provider == "shell.job"
        ),
        "jobs job should complete shell jobs"
    );

    let bg = loader
        .load_command_completion("bg")
        .unwrap()
        .expect("bg completion not found in json");
    let bg_arg = bg.arguments.first().expect("missing bg job argument");
    assert!(bg_arg.multiple, "bg job arg must accept several job specs");
    assert!(
        matches!(
            bg_arg.arg_type,
            Some(ArgumentType::Dynamic { ref provider, .. }) if provider == "shell.job"
        ),
        "bg job should complete shell jobs"
    );
}

#[test]
fn test_git_completion_with_real_json() {
    use doge_shell::completion::command::CommandCompletionDatabase;
    use doge_shell::completion::generator::CompletionGenerator;
    use doge_shell::completion::parser::{CommandLineParser, CompletionContext};

    let root_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = root_dir.parent().unwrap();
    let completions_dir = repo_root.join("completions");

    let loader = JsonCompletionLoader::with_dirs(vec![completions_dir]);

    // Load git completion
    let git_completion = loader
        .load_command_completion("git")
        .unwrap()
        .expect("git completion not found in json");

    // Build DB
    let mut db = CommandCompletionDatabase::new();
    db.add_command(git_completion);

    let parser = CommandLineParser::new();
    let generator = CompletionGenerator::new(&db);

    // Test Case 1: git add -
    // Should return options like --all (and -A if generator is fixed)
    let input = "git add -";
    let parsed = parser.parse(input, input.len());

    // Verify context
    if parsed.completion_context != CompletionContext::LongOption {
        // It might be ShortOption depending on parser logic tweaks, but usually LongOption for "-"
        // Just print it if verification fails
        println!("Context for 'git add -': {:?}", parsed.completion_context);
    }

    let candidates = generator.generate_candidates(&parsed).unwrap();

    // Check for expected candidates from actual git.json
    // git.json defines "add" with options: -A / --all, -u / --update
    let has_all = candidates.iter().any(|c| c.text == "--all");
    let has_short_all = candidates.iter().any(|c| c.text == "-A");

    assert!(has_all, "Should suggest --all for 'git add -'");
    assert!(has_short_all, "Should suggest -A for 'git add -'");

    // Test Case 2: git -
    // Should suggest global options
    let input2 = "git -";
    let parsed2 = parser.parse(input2, input2.len());
    let candidates2 = generator.generate_candidates(&parsed2).unwrap();

    let has_version = candidates2.iter().any(|c| c.text == "--version");
    assert!(has_version, "Should suggest --version for 'git -'");

    // Test Case 3: git commit -
    // Even if commit subcommand is missing options or definition, global options should appear
    let input3 = "git commit -";
    let parsed3 = parser.parse(input3, input3.len());
    let candidates3 = generator.generate_candidates(&parsed3).unwrap();

    let has_version_commit = candidates3.iter().any(|c| c.text == "--version");
    assert!(
        has_version_commit,
        "Should suggest --version for 'git commit -'"
    );
}

#[test]
fn test_pacman_completion_uses_dynamic_package_arguments() {
    let root_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = root_dir.parent().unwrap();
    let completions_dir = repo_root.join("completions");

    let loader = JsonCompletionLoader::with_dirs(vec![completions_dir]);
    let pacman_completion = loader
        .load_command_completion("pacman")
        .unwrap()
        .expect("pacman completion not found in json");

    let sync = pacman_completion
        .subcommands
        .iter()
        .find(|sub| sub.name == "-S")
        .expect("missing pacman -S subcommand");
    let remove = pacman_completion
        .subcommands
        .iter()
        .find(|sub| sub.name == "-R")
        .expect("missing pacman -R subcommand");

    let sync_arg = sync
        .arguments
        .first()
        .expect("missing pacman -S package arg");
    assert!(sync_arg.multiple, "pacman -S package arg must be multiple");
    assert!(
        matches!(
            sync_arg.arg_type,
            Some(ArgumentType::Dynamic {
                ref provider,
                ..
            }) if provider == "pacman.package"
        ),
        "pacman -S package arg should use reusable dynamic package candidates"
    );

    let remove_arg = remove
        .arguments
        .first()
        .expect("missing pacman -R package arg");
    assert!(
        remove_arg.multiple,
        "pacman -R package arg must be multiple"
    );
    assert!(
        matches!(
            remove_arg.arg_type,
            Some(ArgumentType::Dynamic {
                ref provider,
                ..
            }) if provider == "pacman.package"
        ),
        "pacman -R package arg should use reusable dynamic package candidates"
    );
}
