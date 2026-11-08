//! Spotting the places a command line hands code to another interpreter: the
//! short/long option tables that make `bash -ic` and `python3 -Ec` read as
//! `-c` (`EvalFlags`/`string_eval_flag`), and the textual scan for `$(...)`,
//! backticks and process substitution that runs before the parser
//! (`substitution_construct`), interpreters reading code no argument shows
//! (`hidden_code_source`), and all of them together for a raw shell line
//! (`unjudgeable_command_construct`).
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
    /// The program parses long options with `getopt_long`, which accepts any
    /// unambiguous prefix (`env --split` is `--split-string`).
    long_prefix: bool,
}

fn eval_flags(name: &str) -> Option<EvalFlags> {
    let flags = match name {
        "sh" | "bash" | "zsh" | "ksh" | "dash" => EvalFlags {
            eval: &['c'],
            value: &['o'],
            long: &["command"],
            long_prefix: false,
        },
        // `-C`/`--init-command` runs commands before the shell starts.
        "fish" => EvalFlags {
            eval: &['c', 'C'],
            value: &[],
            long: &["command", "init-command"],
            long_prefix: false,
        },
        "python" | "python3" => EvalFlags {
            eval: &['c'],
            value: &['m', 'W', 'X', 'Q'],
            long: &[],
            long_prefix: false,
        },
        // `-l` and `-n` combine with `-e`, so only the letters that always take
        // a value stop the scan.
        "perl" => EvalFlags {
            eval: &['e', 'E'],
            value: &['M', 'I', 'F'],
            long: &[],
            long_prefix: false,
        },
        // Lowercase `-e` evaluates; uppercase `-E` sets the encoding.
        "ruby" => EvalFlags {
            eval: &['e'],
            value: &['I', 'r', 'E', 'C', 'F'],
            long: &[],
            long_prefix: false,
        },
        // `-p` is `-e` with the result printed.
        "node" | "nodejs" | "deno" | "bun" => EvalFlags {
            eval: &['e', 'p'],
            value: &['r'],
            long: &["eval", "print"],
            long_prefix: false,
        },
        // `-S`/`--split-string` splits its value into a whole new command
        // line and runs it - `sh -c` without the shell. `-u`, `-C`, `-P` and
        // `-a` take a value. Options end at the first operand (the command),
        // which the scan below already honours.
        "env" => EvalFlags {
            eval: &['S'],
            value: &['u', 'C', 'P', 'a'],
            long: &["split-string"],
            long_prefix: true,
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
            let matches = |name: &&str| {
                if flags.long_prefix {
                    !long.is_empty() && name.starts_with(long)
                } else {
                    *name == long
                }
            };
            if flags.long.iter().any(matches) {
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

/// Long options that take the next word as their value, so that word is not
/// the script operand.
fn value_long_options(name: &str) -> &'static [&'static str] {
    match name {
        "sh" | "bash" | "zsh" | "ksh" | "dash" => &["rcfile", "init-file"],
        "python" | "python3" => &["check-hash-based-pycs"],
        _ => &[],
    }
}

/// The operand an interpreter reads its program from, if it has one.
///
/// Skips options and the values of the options that take one (`bash -o
/// errexit script`), honours `--`, and returns `None` for a module run
/// (`python3 -m json.tool`), which has no script operand.
pub fn interpreter_script_operand<'a>(program: &str, args: &'a [String]) -> Option<&'a str> {
    let name = program_name(program);
    let flags = eval_flags(&name);
    let value_long = value_long_options(&name);
    let python = matches!(name.as_str(), "python" | "python3");

    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--" {
            return args.next().map(String::as_str);
        }
        // `-` alone is an operand: it names standard input.
        if arg == "-" || !(arg.starts_with('-') || arg.starts_with('+')) {
            return Some(arg.as_str());
        }
        if let Some(long) = arg.strip_prefix("--") {
            if !long.contains('=') && value_long.contains(&long) {
                args.next();
            }
            continue;
        }
        let Some(flags) = flags.as_ref() else {
            continue;
        };
        let cluster = &arg[1..];
        for (index, letter) in cluster.char_indices() {
            if python && letter == 'm' {
                return None;
            }
            if flags.value.contains(&letter) {
                if index + letter.len_utf8() == cluster.len() {
                    args.next();
                }
                break;
            }
        }
    }
    None
}

/// Whether a path names the reading process's standard input or one of its
/// inherited descriptors: `-`, `/dev/stdin`, `/dev/fd/N`, `/proc/self/fd/N`,
/// `/proc/thread-self/fd/N` or `/proc/<pid>/fd/N`.
pub fn names_inherited_descriptor(path: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if path == "-" || path == "/dev/stdin" {
        return true;
    }
    if let Some(fd) = path.strip_prefix("/dev/fd/") {
        return digits(fd);
    }
    if let Some(rest) = path.strip_prefix("/proc/")
        && let Some((process, fd)) = rest.split_once("/fd/")
    {
        return (process == "self" || process == "thread-self" || digits(process)) && digits(fd);
    }
    false
}

/// Ways of feeding an interpreter code that no check can read.
///
/// `string_eval_flag` covers `sh -c '…'`, but a shell reading its script from
/// standard input (`printf '…' | sh`) carries no flag at all, and `eval` is not
/// an interpreter invocation the flag table describes. Both end with the guard
/// having classified `printf` or `eval` while `sh` runs something else - the
/// same hole the flag refusal exists to close.
pub fn hidden_code_source(program: &str, args: &[String]) -> Option<&'static str> {
    let name = program_name(program);

    if name == "eval" {
        return Some("`eval` runs a string that cannot be inspected first");
    }

    // A shell with no script operand reads its program from stdin.
    if is_code_execution_command(&name)
        && name != "sudo"
        && args.iter().all(|arg| arg.starts_with('-'))
    {
        return Some("it reads the code to run from standard input");
    }

    // `printf '…' | bash /dev/stdin` has a script operand, so the rule above
    // does not hold, but the operand is standard input (or an inherited
    // descriptor) and the code it reads is just as unreadable.
    if is_code_execution_command(&name)
        && name != "sudo"
        && interpreter_script_operand(program, args).is_some_and(names_inherited_descriptor)
    {
        return Some("it reads the code to run from standard input");
    }

    // `bash < script.sh` is the same hole with a redirection instead of a pipe:
    // every argument the guard reads is a flag or a filename, the interpreter
    // is classified as itself, and the file it executes is never looked at.
    // `all(starts_with('-'))` above does not hold here because `<` and the
    // filename are ordinary tokens.
    if is_code_execution_command(&name)
        && name != "sudo"
        && args.iter().any(|arg| reads_stdin_from_file(arg))
    {
        return Some("it reads the code to run from a redirected file");
    }

    None
}

/// Whether this token opens standard input from a file.
///
/// `shell_words::split` leaves the redirection as its own token (`<`) when it
/// is spaced and glued to the path (`<script.sh`) when it is not, and a
/// descriptor may lead it (`0<`).
fn reads_stdin_from_file(token: &str) -> bool {
    let rest = token.strip_prefix('0').unwrap_or(token);
    rest.starts_with('<')
}

/// Why a shell command line cannot be judged by classifying the programs it
/// names, if it cannot.
///
/// The pre-checks the agent `execute` tool applies before its guard, in one
/// place for every other caller that hands raw text to a real shell (an MCP
/// server's shell tool). Each construct hides what actually runs from a
/// program-by-program classifier: command substitution runs a body the
/// classifier never sees, a compound statement parses as a command named `{`
/// or `if`, a string-eval flag hands an interpreter a string, and an
/// interpreter reading stdin, a redirected file or `eval` runs code no
/// argument shows. A line that cannot be tokenized is reported too, rather
/// than approved.
pub fn unjudgeable_command_construct(command: &str) -> Option<String> {
    if let Some(construct) = substitution_construct(command) {
        return Some(format!("{construct} cannot be evaluated safely"));
    }

    for segment in split_command_segments(command) {
        let tokens = match shell_words::split(&segment) {
            Ok(tokens) => tokens,
            Err(err) => return Some(format!("the command could not be parsed ({err})")),
        };
        if let Some(keyword) = tokens
            .first()
            .and_then(|leading| compound_statement_keyword(leading))
        {
            return Some(format!(
                "`{keyword}` starts a compound statement, and the commands inside one \
                 cannot be judged before it runs"
            ));
        }
        for (program, args) in command_candidates(&tokens) {
            if let Some(flag) = string_eval_flag(&program, &args) {
                return Some(format!("`{flag}` hands `{program}` a string to execute"));
            }
            if let Some(reason) = hidden_code_source(&program, &args) {
                return Some(format!("`{program}` cannot be judged because {reason}"));
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
