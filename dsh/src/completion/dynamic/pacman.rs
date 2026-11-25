//! Read-only pacman queries against the database selected on the command line.
use super::*;

/// Copy only database selection options. Transaction options must never become
/// arguments of the subprocess used to populate the completion cache.
fn database_args(parsed: &ParsedCommandLine, repos: bool) -> Vec<String> {
    let mut args = vec![];
    if parsed.command != "pacman" {
        return args;
    }
    let Some(source) = &parsed.source else {
        return args;
    };
    let mut words = source
        .words
        .iter()
        .take(source.cursor_word)
        .skip(source.command_word + 1);
    let mut argv = std::iter::from_fn(|| {
        loop {
            let word = &words.next()?.raw;
            if crate::completion::parser::CommandLineParser::is_redirect_operator(word) {
                words.next()?;
            } else {
                return Some(crate::completion::shell_path::decode_literal_word(word));
            }
        }
    });
    while let Some(word) = argv.next() {
        if word == "--" {
            break;
        }
        let (name, value) = if word.starts_with("--") {
            word.split_once('=')
                .map_or((word.as_str(), None), |(n, v)| (n, Some(v)))
        } else if let Some((name, val)) = crate::completion::context::pacman_short_path(&word) {
            (name, (!val.is_empty()).then_some(val))
        } else {
            continue;
        };
        let flag = match name {
            "--config" => "--config",
            "--sysroot" => "--sysroot",
            "--root" | "-r" if repos => "--rootdir",
            "--root" | "-r" => "--root",
            "--dbpath" | "-b" if !repos => "--dbpath",
            "--arch" if !repos => "--arch",
            "--dbpath" | "-b" | "--arch" | "--cachedir" | "--color" | "--gpgdir" | "--hookdir"
            | "--logfile" | "--overwrite" | "--ignore" | "--ignoregroup" | "--assume-installed"
            | "--print-format" => {
                if value.is_none() {
                    argv.next();
                }
                continue;
            }
            _ => continue,
        };
        if let Some(value) = value.map(str::to_owned).or_else(|| argv.next()) {
            args.push(format!("{flag}={value}"));
        }
    }
    args
}

pub(super) fn collect(
    collector: &DynamicCompletionProvider,
    request: &registry::DynamicProviderRequest<'_>,
) -> Vec<EnhancedCandidate> {
    let id = request.provider.as_str();
    let (command, operation, description) = match id {
        "pacman.sync_group" => ("pacman", "-Sg", "sync package group"),
        "pacman.installed_group" => ("pacman", "-Qg", "installed package group"),
        "pacman.installed_package" => ("pacman", "-Qq", "installed package"),
        "pacman.sync_package" => ("pacman", "-Slq", "sync package"),
        "pacman.repository" => ("pacman-conf", "--repo-list", "pacman repository"),
        _ => unreachable!(),
    };
    let parsed = request.parsed_command_line;
    let args = database_args(parsed, command == "pacman-conf");
    let kind = format!("{id}:{args:?}");
    let cwd = request.current_dir.to_owned();
    let path = collector.resolve_command_path(command);
    let list = matches!(&parsed.completion_context, CompletionContext::OptionValue { option_name, .. } if matches!(option_name.as_str(), "--ignore" | "--ignoregroup"));
    let (prefix, query) = if list {
        parsed
            .current_token
            .rsplit_once(',')
            .map_or(("", parsed.current_token.as_str()), |(prefix, tail)| {
                (&parsed.current_token[..prefix.len() + 1], tail)
            })
    } else {
        ("", parsed.current_token.as_str())
    };
    let mut candidates = collector.collect_cached_value_candidates(
        command,
        &kind,
        canonicalize_path(&cwd),
        query,
        description,
        request.cache_policy.is_cached_only(),
        move || {
            let Some(path) = path else {
                return Ok(vec![]);
            };
            let mut flags = vec![operation];
            flags.extend(args.iter().map(String::as_str));
            let lines = run_command_lines(&path, &flags, &cwd)?;
            Ok(if matches!(operation, "-Qg" | "-Sg") {
                parse_first_fields(&lines)
            } else {
                parse_non_empty_lines(&lines)
            })
        },
    );
    if !prefix.is_empty() {
        let prefix = crate::completion::shell_path::decode_literal_word(prefix);
        for c in &mut candidates {
            c.text.insert_str(0, &prefix);
        }
    }
    candidates
}
