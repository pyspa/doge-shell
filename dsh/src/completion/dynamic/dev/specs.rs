//! This family's `LocalSpec` rows for `local::collect`. Table only: which
//! family's table a provider's row lives in does not affect routing, so a fixed
//! shape belongs here rather than in `collect`'s match.
use super::*;

/// This family's rows for `local::collect` - see `local` for what belongs
/// here. Table only; routing is unaffected by which family's table a
/// provider's row lives in.
pub(crate) const LOCAL_SPECS: &[crate::completion::dynamic::local::LocalSpec] = &[
    crate::completion::dynamic::local::LocalSpec {
        provider: "rustup.component",
        command_name: "rustup",
        value_kind: "component",
        scope: crate::completion::dynamic::local::Scope::FixedCwd("/"),
        source: crate::completion::dynamic::local::Source::Lines {
            executable: "rustup",
            args: &["component", "list"],
            parser: parse_rustup_components,
        },
        description: "rustup component",
    },
    crate::completion::dynamic::local::LocalSpec {
        provider: "rustup.target",
        command_name: "rustup",
        value_kind: "target",
        scope: crate::completion::dynamic::local::Scope::FixedCwd("/"),
        source: crate::completion::dynamic::local::Source::Lines {
            executable: "rustup",
            args: &["target", "list"],
            parser: parse_rustup_targets,
        },
        description: "rustup target",
    },
    crate::completion::dynamic::local::LocalSpec {
        provider: "cargo.installed_binary",
        command_name: "cargo",
        value_kind: "installed-binary",
        scope: crate::completion::dynamic::local::Scope::FixedCwd("/"),
        source: crate::completion::dynamic::local::Source::Lines {
            executable: "cargo",
            args: &["install", "--list"],
            parser: parse_cargo_installed_crates,
        },
        description: "cargo installed crate",
    },
    crate::completion::dynamic::local::LocalSpec {
        provider: "bat.theme",
        command_name: "bat",
        value_kind: "theme",
        scope: crate::completion::dynamic::local::Scope::FixedCwd("/"),
        source: crate::completion::dynamic::local::Source::Lines {
            executable: "bat",
            args: &["--list-themes"],
            parser: parse_plain_lines,
        },
        description: "bat theme",
    },
    crate::completion::dynamic::local::LocalSpec {
        provider: "bat.language",
        command_name: "bat",
        value_kind: "language",
        scope: crate::completion::dynamic::local::Scope::FixedCwd("/"),
        source: crate::completion::dynamic::local::Source::Lines {
            executable: "bat",
            args: &["--list-languages"],
            parser: parse_colon_prefixed_names,
        },
        description: "bat language",
    },
    crate::completion::dynamic::local::LocalSpec {
        provider: "rg.file_type",
        command_name: "rg",
        value_kind: "file-type",
        scope: crate::completion::dynamic::local::Scope::FixedCwd("/"),
        source: crate::completion::dynamic::local::Source::Lines {
            executable: "rg",
            args: &["--type-list"],
            parser: parse_colon_prefixed_names,
        },
        description: "ripgrep file type",
    },
    crate::completion::dynamic::local::LocalSpec {
        provider: "ffmpeg.encoder",
        command_name: "ffmpeg",
        value_kind: "encoder",
        scope: crate::completion::dynamic::local::Scope::FixedCwd("/"),
        source: crate::completion::dynamic::local::Source::Lines {
            executable: "ffmpeg",
            args: &["-hide_banner", "-encoders"],
            parser: parse_ffmpeg_table,
        },
        description: "ffmpeg encoder",
    },
    crate::completion::dynamic::local::LocalSpec {
        provider: "ffmpeg.decoder",
        command_name: "ffmpeg",
        value_kind: "decoder",
        scope: crate::completion::dynamic::local::Scope::FixedCwd("/"),
        source: crate::completion::dynamic::local::Source::Lines {
            executable: "ffmpeg",
            args: &["-hide_banner", "-decoders"],
            parser: parse_ffmpeg_table,
        },
        description: "ffmpeg decoder",
    },
    crate::completion::dynamic::local::LocalSpec {
        provider: "ffmpeg.format",
        command_name: "ffmpeg",
        value_kind: "format",
        scope: crate::completion::dynamic::local::Scope::FixedCwd("/"),
        source: crate::completion::dynamic::local::Source::Lines {
            executable: "ffmpeg",
            args: &["-hide_banner", "-formats"],
            parser: parse_ffmpeg_table,
        },
        description: "ffmpeg format",
    },
    crate::completion::dynamic::local::LocalSpec {
        provider: "go.env_key",
        command_name: "go",
        value_kind: "env-key",
        scope: crate::completion::dynamic::local::Scope::FixedCwd("/"),
        source: crate::completion::dynamic::local::Source::Lines {
            executable: "go",
            args: &["env"],
            parser: parse_go_env_keys,
        },
        description: "go environment key",
    },
    crate::completion::dynamic::local::LocalSpec {
        provider: "pipx.installed_package",
        command_name: "pipx",
        value_kind: "installed-package",
        scope: crate::completion::dynamic::local::Scope::FixedCwd("/"),
        source: crate::completion::dynamic::local::Source::Lines {
            executable: "pipx",
            args: &["list", "--short"],
            parser: parse_first_field_lines,
        },
        description: "pipx installed package",
    },
    crate::completion::dynamic::local::LocalSpec {
        provider: "asdf.plugin",
        command_name: "asdf",
        value_kind: "plugin",
        scope: crate::completion::dynamic::local::Scope::FixedCwd("/"),
        source: crate::completion::dynamic::local::Source::Lines {
            executable: "asdf",
            args: &["plugin", "list"],
            parser: parse_first_field_lines,
        },
        description: "asdf plugin",
    },
    crate::completion::dynamic::local::LocalSpec {
        provider: "mise.tool",
        command_name: "mise",
        value_kind: "tool",
        scope: crate::completion::dynamic::local::Scope::FixedCwd("/"),
        source: crate::completion::dynamic::local::Source::Lines {
            executable: "mise",
            args: &["ls", "--installed"],
            parser: parse_mise_tools,
        },
        description: "mise tool",
    },
    crate::completion::dynamic::local::LocalSpec {
        provider: "op.item",
        command_name: "op",
        value_kind: "item",
        scope: crate::completion::dynamic::local::Scope::FixedCwd("/"),
        source: crate::completion::dynamic::local::Source::Lines {
            executable: "op",
            args: &["item", "list", "--format", "json"],
            parser: parse_op_items,
        },
        description: "1Password item",
    },
    crate::completion::dynamic::local::LocalSpec {
        provider: "vagrant.box",
        command_name: "vagrant",
        value_kind: "box",
        scope: crate::completion::dynamic::local::Scope::FixedCwd("/"),
        source: crate::completion::dynamic::local::Source::Lines {
            executable: "vagrant",
            args: &["box", "list"],
            parser: parse_first_field_lines,
        },
        description: "vagrant box",
    },
    crate::completion::dynamic::local::LocalSpec {
        provider: "code.extension",
        command_name: "code",
        value_kind: "extension",
        scope: crate::completion::dynamic::local::Scope::FixedCwd("/"),
        source: crate::completion::dynamic::local::Source::Lines {
            executable: "code",
            args: &["--list-extensions"],
            parser: parse_plain_lines,
        },
        description: "VS Code extension",
    },
    crate::completion::dynamic::local::LocalSpec {
        provider: "golangci_lint.linter",
        command_name: "golangci-lint",
        value_kind: "linter",
        scope: crate::completion::dynamic::local::Scope::CurrentDir,
        source: crate::completion::dynamic::local::Source::Lines {
            executable: "golangci-lint",
            args: &["linters"],
            parser: parse_golangci_linters,
        },
        description: "golangci-lint linter",
    },
];
