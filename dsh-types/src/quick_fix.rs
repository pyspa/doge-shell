//! Deterministic, side-effect-free command failure suggestions.
//!
//! Providers in this module only inspect the failed command, exit status, and
//! captured output. They must not perform I/O, spawn processes, or contact the
//! network so the REPL can use them synchronously before considering AI.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuickFix {
    pub id: String,
    pub title: String,
    pub replacement: String,
}

pub trait QuickFixProvider {
    fn suggest(&self, command: &str, exit_code: i32, output: &str) -> Vec<QuickFix>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct DeterministicQuickFixProvider;

impl QuickFixProvider for DeterministicQuickFixProvider {
    fn suggest(&self, command: &str, exit_code: i32, output: &str) -> Vec<QuickFix> {
        if exit_code == 0 || command.trim().is_empty() {
            return Vec::new();
        }

        let mut fixes = Vec::new();
        push_git_typo(command, output, &mut fixes);
        push_upstream(command, output, &mut fixes);
        push_port_inspection(output, &mut fixes);
        push_permission_fix(command, output, &mut fixes);
        push_runtime_diagnosis(output, &mut fixes);
        push_command_not_found(command, output, &mut fixes);
        fixes
    }
}

fn push_git_typo(command: &str, output: &str, fixes: &mut Vec<QuickFix>) {
    if !first_word(command).is_some_and(|word| word == "git") {
        return;
    }
    let Some(wrong) = between(output, "git: '", "' is not a git command") else {
        return;
    };
    let suggestion = output
        .lines()
        .skip_while(|line| !line.contains("most similar command"))
        .skip(1)
        .map(str::trim)
        .find(|line| !line.is_empty());
    let Some(suggestion) = suggestion else {
        return;
    };
    let Some(replacement) = replace_word(command, wrong, suggestion) else {
        return;
    };
    fixes.push(QuickFix {
        id: "git-typo".to_string(),
        title: format!("Git subcommand `{wrong}` to `{suggestion}`"),
        replacement,
    });
}

fn push_upstream(command: &str, output: &str, fixes: &mut Vec<QuickFix>) {
    if !output.contains("has no upstream branch") {
        return;
    }
    let suggested = output
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("git push --set-upstream "));
    if let Some(replacement) = suggested {
        fixes.push(QuickFix {
            id: "git-upstream".to_string(),
            title: "Set the upstream branch before pushing".to_string(),
            replacement: replacement.to_string(),
        });
    } else if command.trim_start().starts_with("git push") {
        fixes.push(QuickFix {
            id: "git-upstream".to_string(),
            title: "Set the current branch upstream".to_string(),
            replacement: "git push --set-upstream origin HEAD".to_string(),
        });
    }
}

fn push_port_inspection(output: &str, fixes: &mut Vec<QuickFix>) {
    let port = output.lines().find_map(port_from_address_in_use_line);
    if let Some(port) = port {
        fixes.push(QuickFix {
            id: "port-in-use".to_string(),
            title: format!("Inspect the process listening on port {port}"),
            replacement: format!("lsof -nP -iTCP:{port} -sTCP:LISTEN"),
        });
    }
}

fn port_from_address_in_use_line(line: &str) -> Option<u16> {
    let lower = line.to_ascii_lowercase();
    if !(lower.contains("address already in use") || lower.contains("eaddrinuse")) {
        return None;
    }

    let colon_port = line
        .match_indices(':')
        .filter_map(|(offset, _)| {
            let digits = line[offset + 1..]
                .trim_start_matches(':')
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>();
            parse_port(&digits)
        })
        .next_back();
    colon_port.or_else(|| {
        lower
            .split_ascii_whitespace()
            .enumerate()
            .find_map(|(index, word)| {
                if word.trim_matches(|ch: char| !ch.is_ascii_alphanumeric()) != "port" {
                    return None;
                }
                lower
                    .split_ascii_whitespace()
                    .nth(index + 1)
                    .map(|value| value.trim_matches(|ch: char| !ch.is_ascii_digit()))
                    .and_then(parse_port)
            })
    })
}

fn parse_port(value: &str) -> Option<u16> {
    value.parse::<u16>().ok().filter(|port| *port > 0)
}

fn push_permission_fix(command: &str, output: &str, fixes: &mut Vec<QuickFix>) {
    if !output.to_ascii_lowercase().contains("permission denied") {
        return;
    }
    let Some(program) = first_word(command) else {
        return;
    };
    if !program.starts_with("./") || program.contains(['\'', '"']) {
        return;
    }
    fixes.push(QuickFix {
        id: "local-executable-permission".to_string(),
        title: format!("Make `{program}` executable, then retry"),
        replacement: format!("chmod u+x {program} && {command}"),
    });
}

fn push_runtime_diagnosis(output: &str, fixes: &mut Vec<QuickFix>) {
    let lower = output.to_ascii_lowercase();
    if lower.contains("no version is set for command")
        || lower.contains("required tool is missing")
        || lower.contains("runtime is not installed")
    {
        fixes.push(QuickFix {
            id: "project-runtime".to_string(),
            title: "Inspect the project runtime without installing anything".to_string(),
            replacement: "pm status".to_string(),
        });
    }
}

fn push_command_not_found(command: &str, output: &str, fixes: &mut Vec<QuickFix>) {
    if !output.to_ascii_lowercase().contains("command not found") {
        return;
    }
    let Some(program) = first_word(command) else {
        return;
    };
    let replacement_program = match program {
        "gti" => Some("git"),
        "nmp" => Some("npm"),
        "pnmp" => Some("pnpm"),
        "cargp" => Some("cargo"),
        "pyhton" => Some("python"),
        _ => None,
    };
    let Some(replacement_program) = replacement_program else {
        return;
    };
    if let Some(replacement) = replace_first_word(command, replacement_program) {
        fixes.push(QuickFix {
            id: "command-not-found".to_string(),
            title: format!("Replace `{program}` with `{replacement_program}`"),
            replacement,
        });
    }
}

fn first_word(command: &str) -> Option<&str> {
    command.split_ascii_whitespace().next()
}

fn replace_first_word(command: &str, replacement: &str) -> Option<String> {
    let start = command.find(|ch: char| !ch.is_ascii_whitespace())?;
    let end = command[start..]
        .find(|ch: char| ch.is_ascii_whitespace())
        .map_or(command.len(), |offset| start + offset);
    Some(format!(
        "{}{}{}",
        &command[..start],
        replacement,
        &command[end..]
    ))
}

fn replace_word(command: &str, word: &str, replacement: &str) -> Option<String> {
    let start = command
        .split_ascii_whitespace()
        .enumerate()
        .find_map(|(index, candidate)| (index == 1 && candidate == word).then_some(candidate))?;
    let byte_start = start.as_ptr() as usize - command.as_ptr() as usize;
    Some(format!(
        "{}{}{}",
        &command[..byte_start],
        replacement,
        &command[byte_start + word.len()..]
    ))
}

fn between<'a>(input: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let rest = input.split_once(start)?.1;
    Some(rest.split_once(end)?.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn suggest(command: &str, exit_code: i32, output: &str) -> Vec<QuickFix> {
        DeterministicQuickFixProvider.suggest(command, exit_code, output)
    }

    #[test]
    fn fixes_command_not_found_from_a_bounded_typo_table() {
        let fixes = suggest("gti status", 127, "zsh: command not found: gti");
        assert_eq!(fixes[0].replacement, "git status");
        assert!(suggest("unknown-tool", 127, "command not found").is_empty());
    }

    #[test]
    fn fixes_git_typo_from_git_owned_hint() {
        let output = "git: 'statsu' is not a git command. See 'git --help'.\n\nThe most similar command is\n\tstatus";
        let fixes = suggest("git statsu --short", 1, output);
        assert_eq!(fixes[0].replacement, "git status --short");
    }

    #[test]
    fn proposes_upstream_without_executing_it() {
        let output = "fatal: The current branch topic has no upstream branch.\n    git push --set-upstream origin topic";
        let fixes = suggest("git push", 128, output);
        assert_eq!(fixes[0].replacement, "git push --set-upstream origin topic");
    }

    #[test]
    fn diagnoses_port_and_local_permission() {
        assert_eq!(
            suggest(
                "npm run dev",
                1,
                "listen EADDRINUSE: address already in use :::8080"
            )[0]
            .replacement,
            "lsof -nP -iTCP:8080 -sTCP:LISTEN"
        );
        assert_eq!(
            suggest("./scripts/check.sh --fast", 126, "permission denied")[0].replacement,
            "chmod u+x ./scripts/check.sh && ./scripts/check.sh --fast"
        );
    }

    #[test]
    fn port_fix_ignores_versions_pids_and_errno_values() {
        let output = "Node.js v22.1.0 pid 12345: listen EADDRINUSE: address already in use :::8080";
        assert_eq!(
            suggest("npm run dev", 1, output)[0].replacement,
            "lsof -nP -iTCP:8080 -sTCP:LISTEN"
        );
        assert!(
            suggest(
                "python server.py",
                1,
                "OSError: [Errno 48] Address already in use"
            )
            .is_empty()
        );
    }

    #[test]
    fn ignores_success_and_unrelated_failures() {
        assert!(suggest("false", 0, "command not found").is_empty());
        assert!(suggest("false", 1, "ordinary error").is_empty());
    }

    #[test]
    fn missing_project_runtime_routes_to_non_installing_status() {
        let fixes = suggest(
            "node app.js",
            1,
            "No version is set for command node; required tool is missing",
        );
        assert_eq!(fixes[0].replacement, "pm status");
    }
}
