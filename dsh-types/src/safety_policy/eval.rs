//! Spotting the places a command line hands code to another interpreter: the
//! short/long option tables that make `bash -ic` and `python3 -Ec` read as
//! `-c` (`EvalFlags`/`string_eval_flag`), and the textual scan for `$(...)`,
//! backticks and process substitution that runs before the parser
//! (`substitution_construct`).
use super::*;

/// Which short letters and long names hand an interpreter code to run.
///
/// Matching whole tokens was not enough: short options combine, so `bash -ic`
/// and `python3 -Ec` are `-c` wearing a hat, and every interpreter spells the
/// long form differently. `value` lists the letters that swallow the rest of
/// the token as their argument, which is what keeps `perl -Mencoding` from
/// looking like `-e`.
pub struct EvalFlags {
    eval: &'static [char],
    value: &'static [char],
    long: &'static [&'static str],
}

fn eval_flags(name: &str) -> Option<EvalFlags> {
    let flags = match name {
        "sh" | "bash" | "zsh" | "ksh" | "dash" => EvalFlags {
            eval: &['c'],
            value: &['o'],
            long: &["command"],
        },
        // `-C`/`--init-command` runs commands before the shell starts.
        "fish" => EvalFlags {
            eval: &['c', 'C'],
            value: &[],
            long: &["command", "init-command"],
        },
        "python" | "python3" => EvalFlags {
            eval: &['c'],
            value: &['m', 'W', 'X', 'Q'],
            long: &[],
        },
        // `-l` and `-n` combine with `-e`, so only the letters that always take
        // a value stop the scan.
        "perl" => EvalFlags {
            eval: &['e', 'E'],
            value: &['M', 'I', 'F'],
            long: &[],
        },
        // Lowercase `-e` evaluates; uppercase `-E` sets the encoding.
        "ruby" => EvalFlags {
            eval: &['e'],
            value: &['I', 'r', 'E', 'C', 'F'],
            long: &[],
        },
        // `-p` is `-e` with the result printed.
        "node" | "nodejs" | "deno" | "bun" => EvalFlags {
            eval: &['e', 'p'],
            value: &['r'],
            long: &["eval", "print"],
        },
        _ => return None,
    };
    Some(flags)
}

/// PowerShell accepts any unambiguous prefix of an option name, so `-Comm` is
/// `-Command`.
fn is_powershell_eval_option(option: &str) -> bool {
    let option = option.to_ascii_lowercase();
    !option.is_empty()
        && (["command", "encodedcommand"]
            .iter()
            .any(|name| name.starts_with(&option)))
}

/// The flag that hands `program` a string to execute, if the arguments carry
/// one. Returned rather than a bare `bool` so the refusal can say which.
pub fn string_eval_flag(program: &str, args: &[String]) -> Option<String> {
    let name = program_name(program);

    if matches!(name.as_str(), "pwsh" | "powershell") {
        return args
            .iter()
            .find(|arg| arg.strip_prefix('-').is_some_and(is_powershell_eval_option))
            .cloned();
    }

    let flags = eval_flags(&name)?;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        // Everything past `--` is an argument, however it is spelled.
        if arg == "--" {
            return None;
        }

        if let Some(long) = arg.strip_prefix("--") {
            let long = long.split('=').next().unwrap_or(long);
            if flags.long.contains(&long) {
                return Some(arg.clone());
            }
            continue;
        }

        // A word that is not an option is the script to run, and its own
        // arguments follow: `bash script.sh -c` evaluates nothing.
        let cluster = arg
            .strip_prefix(['-', '+'])
            .filter(|cluster| !cluster.is_empty())?;

        for (index, letter) in cluster.char_indices() {
            if flags.eval.contains(&letter) {
                return Some(arg.clone());
            }
            if flags.value.contains(&letter) {
                // The rest of this token is the option's value; if the token
                // ends here, the next word is.
                if index + letter.len_utf8() == cluster.len() {
                    args.next();
                }
                break;
            }
        }
    }

    None
}

/// Name the substitution construct in `command`, if it has one.
///
/// Deliberately textual and deliberately blunt: this runs *before* the parser,
/// so it cannot ask the parser what it is looking at, and a false positive
/// costs the agent one phrasing while a false negative costs an unreviewed
/// execution.
pub fn substitution_construct(command: &str) -> Option<&'static str> {
    let mut in_single = false;
    let mut in_double = false;
    let mut chars = command.char_indices().peekable();

    while let Some((index, ch)) = chars.next() {
        // Inside single quotes the shell expands nothing at all, backslash
        // included, so the only character that matters is the closing quote.
        if in_single {
            if ch == '\'' {
                in_single = false;
            }
            continue;
        }

        match ch {
            '\\' => {
                chars.next();
            }
            // An apostrophe inside double quotes is a literal. Treating it as
            // an opening quote made everything after it look quoted, so
            // `echo "it's $(whoami)"` slipped past this check and was executed
            // by the parser during the safety evaluation.
            '\'' if !in_double => in_single = true,
            '"' => in_double = !in_double,
            // Double quotes do not suppress these two.
            '`' => return Some("backtick command substitution"),
            '$' if command[index + 1..].starts_with('(') => {
                return Some("`$(...)` command substitution");
            }
            // These are only syntax outside quotes.
            '<' | '>' if !in_double && command[index + 1..].starts_with('(') => {
                return Some("process substitution");
            }
            '(' if !in_double => return Some("subshells"),
            _ => {}
        }
    }

    None
}

#[cfg(test)]
mod substitution_tests {
    use super::substitution_construct;

    /// The shell's parser evaluates these while building its job list, so a
    /// safety check that parsed one would run it. Detection happens before the
    /// parser and therefore has to read the quoting rules itself.
    #[test]
    fn every_evaluating_construct_is_named() {
        assert!(substitution_construct("echo $(date)").is_some());
        assert!(substitution_construct("echo `date`").is_some());
        assert!(substitution_construct("diff <(a) <(b)").is_some());
        assert!(substitution_construct("(cd /tmp && ls)").is_some());
        assert!(substitution_construct("printf x > >(dangerous)").is_some());
        assert!(substitution_construct("tee >(a) >(b)").is_some());
    }

    #[test]
    fn plain_command_lines_are_left_alone() {
        for command in [
            "cargo test -p dsh-builtin 2>&1 | tail -40",
            "grep -r 'needle' src && echo done",
            "printf 'a\nb\n' | sort -u > /tmp/out",
            "echo \"it's fine\"",
        ] {
            assert!(
                substitution_construct(command).is_none(),
                "{command} was refused"
            );
        }
    }

    /// Double quotes do not suppress a substitution, and the apostrophe inside
    /// them is a literal - reading it as an opening quote hid everything after.
    #[test]
    fn an_apostrophe_in_double_quotes_does_not_hide_a_substitution() {
        assert!(substitution_construct(r#"echo "it's $(whoami)""#).is_some());
        assert!(substitution_construct(r#"echo "it's `whoami`""#).is_some());
    }
}
