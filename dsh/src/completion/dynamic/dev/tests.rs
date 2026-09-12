use super::*;
use std::path::PathBuf;
use std::time::Duration;
use tempfile::tempdir;

fn lines(value: &str) -> Vec<String> {
    value.lines().map(str::to_string).collect()
}

#[test]
fn rustup_component_parser_strips_the_shared_host_triple() {
    let listing = lines(
        "cargo-x86_64-unknown-linux-gnu (installed)\n\
             clippy-x86_64-unknown-linux-gnu (installed)\n\
             rust-src-x86_64-unknown-linux-gnu\n",
    );
    assert_eq!(
        parse_rustup_components(&listing),
        vec![
            "cargo".to_string(),
            "clippy".to_string(),
            "rust-src".to_string()
        ]
    );
}

#[test]
fn rustup_component_parser_keeps_names_without_a_shared_triple() {
    let listing = lines("clippy\nrustfmt\n");
    assert_eq!(
        parse_rustup_components(&listing),
        vec!["clippy".to_string(), "rustfmt".to_string()]
    );
}

#[test]
fn rustup_component_parser_finds_the_host_triple_among_other_targets() {
    // The real listing carries one rust-std row per supported target, so no
    // suffix is shared by every name.
    let listing = lines(
        "cargo-x86_64-unknown-linux-gnu (installed)\n\
             clippy-x86_64-unknown-linux-gnu (installed)\n\
             rust-src-x86_64-unknown-linux-gnu\n\
             rust-std-x86_64-unknown-linux-gnu (installed)\n\
             rust-std-aarch64-apple-darwin\n\
             rust-std-wasm32-unknown-unknown\n\
             rust-std-x86_64-pc-windows-msvc\n",
    );
    assert_eq!(
        parse_rustup_components(&listing),
        vec![
            "cargo".to_string(),
            "clippy".to_string(),
            "rust-src".to_string(),
            "rust-std".to_string(),
            "rust-std-aarch64-apple-darwin".to_string(),
            "rust-std-wasm32-unknown-unknown".to_string(),
            "rust-std-x86_64-pc-windows-msvc".to_string(),
        ]
    );
}

#[test]
fn rustup_component_parser_handles_a_three_segment_host_triple() {
    let listing = lines(
        "cargo-aarch64-apple-darwin (installed)\n\
             clippy-aarch64-apple-darwin (installed)\n\
             rust-src-aarch64-apple-darwin\n\
             rust-std-x86_64-unknown-linux-gnu\n",
    );
    assert_eq!(
        parse_rustup_components(&listing),
        vec![
            "cargo".to_string(),
            "clippy".to_string(),
            "rust-src".to_string(),
            "rust-std-x86_64-unknown-linux-gnu".to_string(),
        ]
    );
}

#[test]
fn rustup_target_parser_drops_the_installed_marker() {
    let listing = lines("aarch64-apple-darwin\nx86_64-unknown-linux-gnu (installed)\n");
    assert_eq!(
        parse_rustup_targets(&listing),
        vec![
            "aarch64-apple-darwin".to_string(),
            "x86_64-unknown-linux-gnu".to_string()
        ]
    );
}

#[test]
fn cargo_install_list_parser_keeps_only_crate_headers() {
    // The command runner trims every line, so the parser must not rely on
    // the indentation that separates binaries from their crate header.
    let listing = lines("cargo-make v0.37.23:\ncargo-make\nmakers\nripgrep v14.1.0:\nrg\n");
    assert_eq!(
        parse_cargo_installed_crates(&listing),
        vec!["cargo-make".to_string(), "ripgrep".to_string()]
    );
}

#[test]
fn go_env_parser_keeps_keys_containing_digits() {
    let listing = lines("GO111MODULE='on'\nGOAMD64='v1'\nGO386=''\nGOROOT='/usr/lib/go'\n");
    assert_eq!(
        parse_go_env_keys(&listing),
        vec![
            "GO111MODULE".to_string(),
            "GO386".to_string(),
            "GOAMD64".to_string(),
            "GOROOT".to_string()
        ]
    );
}

#[test]
fn colon_prefixed_parser_reads_bat_and_ripgrep_listings() {
    assert_eq!(
        parse_colon_prefixed_names(&lines("Rust:rs\nApache Conf:envvars,htaccess\n")),
        vec!["Apache Conf".to_string(), "Rust".to_string()]
    );
    assert_eq!(
        parse_colon_prefixed_names(&lines("ada: *.adb, *.ads\nrust: *.rs\n")),
        vec!["ada".to_string(), "rust".to_string()]
    );
}

#[test]
fn ffmpeg_table_parser_skips_the_legend_and_splits_aliases() {
    let listing = lines(
        "File formats:\n D. = Demuxing supported\n E. = Muxing supported\n --\n \
             D  3dostr          3DO STR\n DE matroska,webm  Matroska / WebM\n",
    );
    assert_eq!(
        parse_ffmpeg_table(&listing),
        vec![
            "3dostr".to_string(),
            "matroska".to_string(),
            "webm".to_string()
        ]
    );
}

#[test]
fn go_env_parser_keeps_only_upper_case_keys() {
    let listing = lines("AR='ar'\nCGO_CFLAGS='-O2 -g'\nnot a key\n");
    assert_eq!(
        parse_go_env_keys(&listing),
        vec!["AR".to_string(), "CGO_CFLAGS".to_string()]
    );
}

#[test]
fn mise_listing_parser_drops_the_header_row() {
    let listing = lines("Tool  Version  Source\nnode  22.1.0  .mise.toml\npython  3.13.1\n");
    assert_eq!(
        parse_mise_tools(&listing),
        vec!["node".to_string(), "python".to_string()]
    );
}

#[test]
fn noxfile_parser_reads_decorated_sessions_without_executing_them() {
    let contents = r#"
import nox

VERSIONS = ["3.11", "3.12"]

@nox.session(python=VERSIONS)
def tests(session):
    session.run("pytest")

@nox.session(
    python="3.12",
    name="type-check",
)
def mypy(session):
    session.run("mypy")

@session
async def lint(session):
    session.run("ruff")

def helper():
    return 1
"#;
    assert_eq!(
        parse_nox_sessions(contents),
        vec![
            "lint".to_string(),
            "tests".to_string(),
            "type-check".to_string()
        ]
    );
}

#[test]
fn tox_ini_parser_reads_envlist_and_testenv_sections() {
    let contents =
        "[tox]\nenvlist = py311, py312\n    lint\n\n[testenv:docs]\ncommands = mkdocs build\n";
    assert_eq!(
        parse_tox_ini_environments(contents),
        vec![
            "docs".to_string(),
            "lint".to_string(),
            "py311".to_string(),
            "py312".to_string()
        ]
    );
}

#[test]
fn tox_envlist_parser_drops_inline_comments() {
    let contents = "[tox]\nenvlist = py311, py312  # run before release\n";
    assert_eq!(
        parse_tox_ini_environments(contents),
        vec!["py311".to_string(), "py312".to_string()]
    );
}

#[test]
fn hatch_environments_come_from_pyproject_and_hatch_toml() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("pyproject.toml"),
        "[tool.hatch.envs.default]\ndependencies = []\n[tool.hatch.envs.docs]\n",
    )
    .unwrap();
    fs::write(dir.path().join("hatch.toml"), "[envs.lint]\n").unwrap();
    assert_eq!(
        load_hatch_environments(dir.path()),
        vec![
            "default".to_string(),
            "docs".to_string(),
            "lint".to_string()
        ]
    );
}

#[test]
fn pre_commit_parser_reads_hook_ids() {
    let contents = "repos:\n  - repo: local\n    hooks:\n      - id: fmt\n        name: fmt\n      - id: \"clippy\"\n";
    assert_eq!(
        parse_pre_commit_hook_ids(contents),
        vec!["clippy".to_string(), "fmt".to_string()]
    );
}

#[test]
fn python_dependency_parser_reads_common_project_files() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("pyproject.toml"),
        r#"
[project]
dependencies = ["requests>=2", "fastapi[standard]"]
[project.optional-dependencies]
dev = ["pytest>=8"]
[dependency-groups]
lint = ["ruff==0.8"]
[tool.poetry.dependencies]
python = "^3.12"
pendulum = "^3"
[tool.poetry.group.docs.dependencies]
mkdocs = "^1"
"#,
    )
    .unwrap();
    fs::write(
        dir.path().join("requirements-dev.txt"),
        "black==24.0\n-r base.txt\n./local-package\ngit+https://example.invalid/pkg\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("Pipfile"),
        "[packages]\nflask = \"*\"\n[dev-packages]\ncoverage = \"*\"\n",
    )
    .unwrap();

    assert_eq!(
        load_python_project_dependencies(dir.path()),
        vec![
            "black".to_string(),
            "coverage".to_string(),
            "fastapi".to_string(),
            "flask".to_string(),
            "mkdocs".to_string(),
            "pendulum".to_string(),
            "pytest".to_string(),
            "requests".to_string(),
            "ruff".to_string(),
        ]
    );
}

#[test]
fn node_bin_loader_reads_local_package_binaries() {
    let dir = tempdir().unwrap();
    let bin_dir = dir.path().join("node_modules").join(".bin");
    fs::create_dir_all(&bin_dir).unwrap();
    fs::write(bin_dir.join("vite"), "").unwrap();
    fs::write(bin_dir.join("eslint"), "").unwrap();
    fs::write(bin_dir.join(".ignored"), "").unwrap();

    assert_eq!(
        load_node_bin_names(dir.path()),
        vec!["eslint".to_string(), "vite".to_string()]
    );
}

#[test]
fn node_bin_root_walks_up_from_workspace_subdir() {
    let dir = tempdir().unwrap();
    let bin_dir = dir.path().join("node_modules").join(".bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let package_dir = dir.path().join("packages").join("web").join("src");
    fs::create_dir_all(&package_dir).unwrap();

    assert_eq!(
        find_node_bin_root(&package_dir).as_deref(),
        Some(dir.path().canonicalize().unwrap().as_path())
    );
}

#[test]
fn python_module_loader_reads_dependencies_and_project_modules() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("pyproject.toml"),
        "[project]\ndependencies = [\"fast-api>=1\", \"google-cloud-storage\"]\n",
    )
    .unwrap();
    let package_dir = dir.path().join("src").join("demo_app");
    fs::create_dir_all(&package_dir).unwrap();
    fs::write(package_dir.join("__init__.py"), "").unwrap();
    fs::write(package_dir.join("cli.py"), "").unwrap();
    fs::write(dir.path().join("tool.py"), "").unwrap();

    assert_eq!(
        load_python_modules(dir.path()),
        vec![
            "demo_app".to_string(),
            "demo_app.cli".to_string(),
            "fast_api".to_string(),
            "google_cloud_storage".to_string(),
            "tool".to_string(),
        ]
    );
}

#[test]
fn node_workspace_loader_reads_package_json_and_pnpm_workspace() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("package.json"),
        r#"{ "workspaces": ["packages/*"] }"#,
    )
    .unwrap();
    fs::write(
        dir.path().join("pnpm-workspace.yaml"),
        "packages:\n  - apps/*\n  - '!ignored/*'\n",
    )
    .unwrap();
    let web_dir = dir.path().join("packages").join("web");
    let api_dir = dir.path().join("apps").join("api");
    fs::create_dir_all(&web_dir).unwrap();
    fs::create_dir_all(&api_dir).unwrap();
    fs::write(web_dir.join("package.json"), r#"{ "name": "@demo/web" }"#).unwrap();
    fs::write(api_dir.join("package.json"), r#"{ "name": "api" }"#).unwrap();

    assert_eq!(
        find_node_workspace_root(&web_dir).as_deref(),
        Some(dir.path().canonicalize().unwrap().as_path())
    );
    assert_eq!(
        load_node_workspaces(dir.path()),
        vec![
            "@demo/web".to_string(),
            "api".to_string(),
            "apps/api".to_string(),
            "packages/web".to_string(),
        ]
    );
}

#[test]
fn cloud_and_terraform_loaders_read_local_config_only() {
    let dir = tempdir().unwrap();
    let aws_dir = dir.path().join(".aws");
    fs::create_dir_all(&aws_dir).unwrap();
    fs::write(
        aws_dir.join("config"),
        "[default]\nregion = us-east-1\n[profile dev]\nregion = us-west-2\n",
    )
    .unwrap();
    fs::write(
        aws_dir.join("credentials"),
        "[prod]\naws_access_key_id = test\n",
    )
    .unwrap();
    assert_eq!(
        load_aws_profiles(&aws_dir.join("config"), &aws_dir.join("credentials")),
        vec!["default".to_string(), "dev".to_string(), "prod".to_string()]
    );

    let gcloud_dir = dir.path().join("gcloud");
    let configs_dir = gcloud_dir.join("configurations");
    fs::create_dir_all(&configs_dir).unwrap();
    fs::write(configs_dir.join("config_dev"), "project = demo-dev\n").unwrap();
    fs::write(configs_dir.join("config_prod"), "project = demo-prod\n").unwrap();
    assert_eq!(
        load_gcloud_configurations(&gcloud_dir),
        vec!["dev".to_string(), "prod".to_string()]
    );
    assert_eq!(
        load_gcloud_projects(&gcloud_dir),
        vec!["demo-dev".to_string(), "demo-prod".to_string()]
    );

    let azure_dir = dir.path().join(".azure");
    fs::create_dir_all(&azure_dir).unwrap();
    fs::write(
        azure_dir.join("azureProfile.json"),
        r#"{
                "subscriptions": [
                    { "id": "0000-1111", "name": "Dev Subscription" },
                    { "id": "2222-3333", "name": "Prod Subscription" }
                ]
            }"#,
    )
    .unwrap();
    assert_eq!(
        load_az_subscriptions(&azure_dir.join("azureProfile.json")),
        vec!["0000-1111".to_string(), "2222-3333".to_string()]
    );
    assert!(
        !load_az_subscriptions(&azure_dir.join("azureProfile.json"))
            .iter()
            .any(|value| value.contains(' ')),
        "subscription names are not shell-safe as raw argument candidates"
    );

    let terraform_dir = dir.path().join(".terraform");
    fs::create_dir_all(terraform_dir.join("terraform.tfstate.d").join("dev")).unwrap();
    fs::write(terraform_dir.join("environment"), "staging\n").unwrap();
    assert_eq!(
        load_terraform_workspaces(dir.path()),
        vec![
            "default".to_string(),
            "dev".to_string(),
            "staging".to_string(),
        ]
    );
}

#[test]
fn maven_loaders_read_profiles_and_modules_from_pom() {
    let dir = tempdir().unwrap();
    let pom = dir.path().join("pom.xml");
    fs::write(
        &pom,
        r#"
<project>
  <modules>
    <module>service-api</module>
    <module>service-web</module>
  </modules>
  <profiles>
    <profile><id>dev</id></profile>
    <profile><id>release</id></profile>
  </profiles>
</project>
"#,
    )
    .unwrap();

    assert_eq!(
        load_maven_modules(&pom),
        vec!["service-api".to_string(), "service-web".to_string()]
    );
    assert_eq!(
        load_maven_profiles(&pom),
        vec!["dev".to_string(), "release".to_string()]
    );
}

#[test]
fn ansible_inventory_parser_reads_ini_and_yaml_names() {
    let inventory = r#"
[web]
web-1 ansible_host=192.0.2.10

[db:children]
postgres

all:
  children:
    api:
      hosts:
        api-1:
"#;

    assert_eq!(
        parse_ansible_inventory_values(inventory),
        vec![
            "api".to_string(),
            "api-1".to_string(),
            "db".to_string(),
            "postgres".to_string(),
            "web".to_string(),
            "web-1".to_string(),
        ]
    );
}

#[test]
fn go_list_parser_exposes_import_and_relative_package_values() {
    let root = PathBuf::from("/workspace/app");
    let lines = vec![
        "/workspace/app\t/workspace/app".to_string(),
        "example.com/app/pkg/api\t/workspace/app/pkg/api".to_string(),
    ];

    assert_eq!(
        parse_go_list_package_values(&lines, &root),
        vec![
            ".".to_string(),
            "./...".to_string(),
            "./pkg/api".to_string(),
            "/workspace/app".to_string(),
            "example.com/app/pkg/api".to_string(),
        ]
    );
}

#[test]
fn dynamic_collectors_filter_cached_values_by_prefix() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("pyproject.toml"),
        "[project]\ndependencies = [\"requests>=2\", \"pytest\"]\n",
    )
    .unwrap();
    let bin_dir = dir.path().join("node_modules").join(".bin");
    fs::create_dir_all(&bin_dir).unwrap();
    fs::write(bin_dir.join("vite"), "").unwrap();

    let provider = DynamicCompletionProvider::new(crate::environment::Environment::new());
    let started = std::time::Instant::now();
    let py = loop {
        let candidates =
            provider.collect_python_project_dependency_candidates(dir.path(), "req", false);
        if !candidates.is_empty() {
            break candidates;
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "timed out waiting for Python dependency cache refresh"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(py[0].text, "requests");

    let started = std::time::Instant::now();
    let node = loop {
        let candidates = provider.collect_node_bin_candidates(dir.path(), "vi", false);
        if !candidates.is_empty() {
            break candidates;
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "timed out waiting for Node binary cache refresh"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(node[0].text, "vite");
}

#[test]
fn new_developer_provider_parsers_read_project_metadata() {
    let dir = tempdir().unwrap();
    fs::write(
            dir.path().join("bacon.toml"),
            "[jobs.check]\ncommand = [\"cargo\", \"check\"]\n[jobs.test]\ncommand = [\"cargo\", \"test\"]\n",
        )
        .unwrap();
    fs::write(
        dir.path().join("pyproject.toml"),
        "[tool.pdm.scripts]\ntest = \"pytest\"\nlint = \"ruff check\"\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("Pipfile"),
        "[scripts]\ntest = \"pytest\"\nserve = \"python -m app\"\n",
    )
    .unwrap();

    assert_eq!(
        load_toml_table_keys(&dir.path().join("bacon.toml"), &["jobs"]),
        vec!["check".to_string(), "test".to_string()]
    );
    assert_eq!(
        load_toml_table_keys(
            &dir.path().join("pyproject.toml"),
            &["tool", "pdm", "scripts"]
        ),
        vec!["lint".to_string(), "test".to_string()]
    );
    assert_eq!(
        load_toml_table_keys(&dir.path().join("Pipfile"), &["scripts"]),
        vec!["serve".to_string(), "test".to_string()]
    );
}

#[test]
fn new_developer_command_parsers_ignore_headers_and_malformed_json() {
    assert_eq!(
        parse_golangci_linters(&[
            "Enabled by default linters:".to_string(),
            "errcheck: Errcheck is a program for checking errors".to_string(),
            "  govet: Vet examines Go source code".to_string(),
            "Disabled by default linters:".to_string(),
            "gocyclo: Computes cyclomatic complexity".to_string(),
        ]),
        vec![
            "errcheck".to_string(),
            "gocyclo".to_string(),
            "govet".to_string(),
        ]
    );
    assert_eq!(
        parse_meson_targets(r#"[{"name":"app","id":"app@exe"},{"name":"tests","id":"tests@run"}]"#),
        vec!["app".to_string(), "tests".to_string()]
    );
    assert!(parse_meson_targets("not-json").is_empty());
}

#[test]
fn meson_build_directory_and_jj_root_are_context_scoped() {
    use crate::completion::parser::CommandLineParser;

    let dir = tempdir().unwrap();
    fs::write(dir.path().join("meson.build"), "project('demo', 'c')\n").unwrap();
    fs::create_dir_all(dir.path().join("out")).unwrap();
    fs::create_dir_all(dir.path().join(".jj")).unwrap();
    let child = dir.path().join("src");
    fs::create_dir_all(&child).unwrap();

    let input = "meson compile -C out ";
    let parsed = CommandLineParser::new().parse(input, input.len());
    assert_eq!(
        selected_meson_build_dir(&parsed, dir.path()),
        dir.path().join("out")
    );
    let default_input = "meson compile ";
    let default_parsed = CommandLineParser::new().parse(default_input, default_input.len());
    assert_eq!(
        selected_meson_build_dir(&default_parsed, dir.path()),
        dir.path().join("build")
    );
    assert_eq!(find_jj_root(&child), Some(dir.path().to_path_buf()));

    let repository = dir.path().join("other");
    for input in [
        format!("jj -R {} bookmark delete ", repository.display()),
        format!("jj --repository={} bookmark delete ", repository.display()),
    ] {
        let parsed = CommandLineParser::new().parse(&input, input.len());
        assert_eq!(
            selected_jj_repository(&parsed, dir.path()),
            Some(repository.clone()),
            "{input}"
        );
    }
}
