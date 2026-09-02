use regex::Regex;
use std::path::Path;
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SafetyLevel {
    Strict,
    Normal,
    Loose,
}

/// Parse a level the way `(safety-level 'strict)` and `SAFETY_LEVEL=strict` spell it.
///
/// `from_env_value` silently falls back to `Normal`, which is what an
/// environment variable wants; this reports the typo, which is what an explicit
/// `(safety-level ...)` call wants.
impl std::str::FromStr for SafetyLevel {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "strict" => Ok(Self::Strict),
            "normal" => Ok(Self::Normal),
            "loose" => Ok(Self::Loose),
            other => Err(format!(
                "Invalid safety level: {other}. Valid levels are: strict, normal, loose"
            )),
        }
    }
}

impl SafetyLevel {
    /// The lowercase spelling, which is what both `SAFETY_LEVEL` and
    /// `(safety-level)` hand back.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Strict => "strict",
            Self::Normal => "normal",
            Self::Loose => "loose",
        }
    }

    pub fn from_env_value(value: Option<String>) -> Self {
        match value
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("strict") => Self::Strict,
            Some("loose") => Self::Loose,
            _ => Self::Normal,
        }
    }

    pub fn requires_confirmation_for_sensitive_access(self) -> bool {
        !matches!(self, Self::Loose)
    }
}

static SECRET_ASSIGNMENT: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?i)\b([A-Z_][A-Z0-9_]*)=([^\s]+)").ok());

static SECRET_OPTION: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)(--?(?:password|passwd|passphrase|token|secret|api[-_]?key|access[-_]?token)(?:\s+|=)|-p\s+)([^\s"']+|"[^"]*"|'[^']*')"#,
    )
    .ok()
});

static AUTH_BEARER: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r#"(?i)(authorization\s*:\s*bearer\s+)([A-Za-z0-9._~+/=-]+)"#).ok()
});

static QUERY_SECRET: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r#"(?i)([?&](?:token|access_token|api_key|apikey|auth|password)=)([^&\s]+)"#).ok()
});

static PRIVATE_KEY_MARKER: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"-----BEGIN [A-Z ]*PRIVATE KEY-----").ok());

/// Commands that fetch remote content.
///
/// Shared so the `curl | sh` judgement is the same one everywhere: `safe-run`
/// used to look for the literal substring `"curl "`, which missed `wget`, any
/// absolute path and every spacing but one.
pub fn is_network_fetch_command(program: &str) -> bool {
    matches!(
        command_stem(program),
        "curl" | "wget" | "fetch" | "scp" | "aria2c" | "httpie" | "http"
    )
}

/// Commands that run whatever they are handed.
pub fn is_code_execution_command(program: &str) -> bool {
    matches!(
        command_stem(program),
        "sh" | "bash"
            | "zsh"
            | "fish"
            | "ksh"
            | "dash"
            | "python"
            | "python3"
            | "perl"
            | "ruby"
            | "node"
            | "sudo"
    )
}

/// Commands that destroy a device or a filesystem outright.
///
/// `dd` is here rather than behind an `if=` test because a `dd` with no `if=`
/// is still writing over something.
pub fn is_disk_destroying_command(program: &str) -> bool {
    let stem = command_stem(program);
    matches!(stem, "dd" | "mkswap" | "format" | "fdisk" | "parted") || stem.starts_with("mkfs")
}

/// Commands whose plain invocation ends the session or the machine.
pub fn is_system_power_command(program: &str) -> bool {
    matches!(
        command_stem(program),
        "reboot" | "shutdown" | "poweroff" | "halt"
    )
}

/// Split a command line at the operators that end one command.
///
/// Done on the raw text, not on tokens: `shell_words` splits on whitespace, so
/// `echo hi; rm -rf ~` tokenizes as `["echo", "hi;", "rm", …]` and a
/// token-level split never sees the `;` at all. Quotes are honoured, so the
/// `;` inside `git commit -m 'a; b'` does not start a command.
///
/// Only `|`, `&` and `;` - the operators that separate whole commands.
pub fn split_command_segments(command: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut chars = command.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '\\' if !in_single => {
                current.push(ch);
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            '\'' if !in_double => {
                in_single = !in_single;
                current.push(ch);
            }
            '"' if !in_single => {
                in_double = !in_double;
                current.push(ch);
            }
            '|' | '&' | ';' if !in_single && !in_double => {
                if !current.trim().is_empty() {
                    segments.push(current.trim().to_string());
                }
                current.clear();
            }
            _ => current.push(ch),
        }
    }

    if !current.trim().is_empty() {
        segments.push(current.trim().to_string());
    }

    segments
}

/// The leading word of a compound statement `sh` understands and dsh does not.
///
/// dsh's grammar has no grouping or control flow, so `{ rm -rf ~; }` parses as
/// a command literally named `{` with `rm` as an argument - and a command named
/// `{` has no rule, so every dangerous-command check passed while `sh -c` ran
/// the group. Nothing inside a compound statement can be classified, so a
/// safety check has to say so rather than approve the wrapper.
pub fn compound_statement_keyword(program: &str) -> Option<&'static str> {
    const KEYWORDS: &[&str] = &[
        "{", "}", "(", ")", "if", "then", "elif", "else", "fi", "for", "while", "until", "do",
        "done", "case", "esac", "select", "function",
    ];
    KEYWORDS
        .iter()
        .find(|keyword| **keyword == program)
        .copied()
}

/// Programs that run another program, so the name in front is not the one that
/// does the work.
///
/// `sudo rm -rf ~` classified as `sudo` - which has no rule - passed every
/// dangerous-command check. Shared so the guard and the chat `execute` tool
/// look through the same list.
pub const COMMAND_WRAPPERS: &[&str] = &[
    "sudo", "doas", "env", "nice", "ionice", "nohup", "time", "timeout", "command", "xargs",
    "stdbuf", "setsid", "chroot",
];

pub fn is_command_wrapper(program: &str) -> bool {
    COMMAND_WRAPPERS.contains(&command_stem(program))
}

/// Whether a token is a `NAME=value` assignment rather than a program.
pub fn is_assignment_token(token: &str) -> bool {
    let Some((name, _)) = token.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Every program a wrapped command line might actually run.
///
/// A wrapper's own options can take a value (`timeout 5 …`, `nice -n 10 …`,
/// `chroot /new …`), and that value is not an option, so "the first token that
/// is not an option" picks the value and never reaches the real program. Rather
/// than encode each wrapper's option arity, every remaining non-option token is
/// offered as a candidate: a wrong guess costs one extra confirmation, while a
/// missed one runs `rm -rf ~` unasked.
///
/// Returns `(program, args)` pairs, the leading command first.
pub fn command_candidates(tokens: &[String]) -> Vec<(String, Vec<String>)> {
    let start = tokens
        .iter()
        .position(|token| !is_assignment_token(token))
        .unwrap_or(tokens.len());
    let tokens = &tokens[start..];

    let Some(program) = tokens.first() else {
        return Vec::new();
    };
    let mut candidates = vec![(program.clone(), tokens[1..].to_vec())];

    if !is_command_wrapper(program) {
        return candidates;
    }

    for index in 1..tokens.len() {
        let token = &tokens[index];
        if token.starts_with('-') || is_assignment_token(token) {
            continue;
        }
        candidates.push((token.clone(), tokens[index + 1..].to_vec()));
    }

    candidates
}

/// The program name without its directory, so `/bin/rm` is judged as `rm`.
pub fn command_stem(program: &str) -> &str {
    program.rsplit('/').next().unwrap_or(program)
}

/// What an `rm` invocation is about to do, if it is worth saying out loud.
///
/// Token-based: `rm -rf /` and `rm -fr /` are the same thing, and neither is
/// `rm ./rf-notes`. The substring version in `safe-run` answered both wrong.
pub fn destructive_rm_warning(args: &[String]) -> Option<String> {
    let mut recursive = false;
    let mut force = false;
    let mut root_path = false;

    for arg in args {
        match arg.as_str() {
            "-r" | "-R" | "--recursive" => recursive = true,
            "-f" | "--force" => force = true,
            "/" | "/*" => root_path = true,
            _ => {}
        }

        // A short-option cluster: `-rf`, `-fr`, `-rfv`. Only clusters, so
        // `--reflink` and a file called `-rest` are left alone.
        if let Some(flags) = arg.strip_prefix('-')
            && !arg.starts_with("--")
            && flags.chars().all(|c| c.is_ascii_alphabetic())
        {
            if flags.contains('r') || flags.contains('R') {
                recursive = true;
            }
            if flags.contains('f') {
                force = true;
            }
        }
    }

    if recursive && force && root_path {
        return Some("High Risk: 'rm -rf /' detected. This is extremely dangerous.".to_string());
    }
    if recursive && force {
        return Some("High Risk: Recursive forced deletion ('rm -rf') detected.".to_string());
    }
    if recursive {
        return Some("Recursive deletion detected. Proceed?".to_string());
    }
    None
}

pub fn is_sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_uppercase();
    [
        "API_KEY",
        "_KEY",
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "PASSPHRASE",
        "AUTH",
        "COOKIE",
        "SESSION",
        "CREDENTIAL",
        "PRIVATE",
        "ACCESS_KEY",
        "SECRET_KEY",
    ]
    .iter()
    .any(|needle| key.contains(needle))
}

pub fn is_sensitive_path(path: &Path) -> bool {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    file_name == ".env"
        || file_name.starts_with(".env.")
        || file_name.ends_with("_history")
        || file_name == "id_rsa"
        || file_name == "id_ed25519"
        || file_name.ends_with(".pem")
        || file_name.ends_with(".key")
        || has_path_component(path, ".ssh")
        || has_path_component_sequence(path, &[".aws", "credentials"])
        || has_path_component_sequence(path, &[".config", "gcloud"])
        || has_path_component(path, ".azure")
        || has_path_component(path, "credentials")
        || has_path_component(path, "secrets")
}

fn has_path_component(path: &Path, needle: &str) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|value| value.eq_ignore_ascii_case(needle))
    })
}

fn has_path_component_sequence(path: &Path, sequence: &[&str]) -> bool {
    if sequence.is_empty() {
        return false;
    }

    let components = path
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    components
        .windows(sequence.len())
        .any(|window| window.iter().zip(sequence).all(|(a, b)| a.as_str() == *b))
}

pub fn contains_sensitive_text(text: &str) -> bool {
    contains_sensitive_text_with(text, is_sensitive_key)
}

pub fn contains_sensitive_text_with<F>(text: &str, is_key_sensitive: F) -> bool
where
    F: Fn(&str) -> bool,
{
    if SECRET_ASSIGNMENT.as_ref().is_some_and(|pattern| {
        pattern
            .captures_iter(text)
            .any(|cap| cap.get(1).is_some_and(|key| is_key_sensitive(key.as_str())))
    }) {
        return true;
    }

    SECRET_OPTION
        .as_ref()
        .is_some_and(|pattern| pattern.is_match(text))
        || AUTH_BEARER
            .as_ref()
            .is_some_and(|pattern| pattern.is_match(text))
        || QUERY_SECRET
            .as_ref()
            .is_some_and(|pattern| pattern.is_match(text))
        || PRIVATE_KEY_MARKER
            .as_ref()
            .is_some_and(|pattern| pattern.is_match(text))
}

pub fn redact_sensitive_text(text: &str) -> String {
    redact_sensitive_text_with(text, is_sensitive_key)
}

pub fn redact_sensitive_text_with<F>(text: &str, is_key_sensitive: F) -> String
where
    F: Fn(&str) -> bool,
{
    let mut redacted = text.to_string();

    if let Some(pattern) = SECRET_ASSIGNMENT.as_ref() {
        redacted = pattern
            .replace_all(&redacted, |caps: &regex::Captures<'_>| {
                let key = caps.get(1).map(|m| m.as_str()).unwrap_or("");
                if is_key_sensitive(key) {
                    format!("{key}=***")
                } else {
                    caps.get(0)
                        .map(|m| m.as_str())
                        .unwrap_or_default()
                        .to_string()
                }
            })
            .to_string();
    }

    if let Some(pattern) = SECRET_OPTION.as_ref() {
        redacted = pattern
            .replace_all(&redacted, |caps: &regex::Captures<'_>| {
                format!("{}***", caps.get(1).map(|m| m.as_str()).unwrap_or(""))
            })
            .to_string();
    }

    if let Some(pattern) = AUTH_BEARER.as_ref() {
        redacted = pattern
            .replace_all(&redacted, |caps: &regex::Captures<'_>| {
                format!("{}***", caps.get(1).map(|m| m.as_str()).unwrap_or(""))
            })
            .to_string();
    }

    if let Some(pattern) = QUERY_SECRET.as_ref() {
        redacted = pattern
            .replace_all(&redacted, |caps: &regex::Captures<'_>| {
                format!("{}***", caps.get(1).map(|m| m.as_str()).unwrap_or(""))
            })
            .to_string();
    }

    if let Some(pattern) = PRIVATE_KEY_MARKER.as_ref() {
        redacted = pattern
            .replace_all(&redacted, "-----BEGIN *** PRIVATE KEY-----")
            .to_string();
    }

    redacted
}

pub fn mask_env_value(key: &str, value: &str) -> String {
    if is_sensitive_key(key) || contains_sensitive_text(value) {
        "***".to_string()
    } else {
        value.to_string()
    }
}

/// The program itself, without the directory it was found in.
fn program_name(program: &str) -> String {
    Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(program)
        .to_string()
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitive_path_detects_common_secret_files() {
        assert!(is_sensitive_path(Path::new(".env")));
        assert!(is_sensitive_path(Path::new("/home/me/.ssh/id_ed25519")));
        assert!(is_sensitive_path(Path::new("/home/me/.aws/credentials")));
        assert!(is_sensitive_path(Path::new("/repo/secrets/token.txt")));
        assert!(is_sensitive_path(Path::new("/repo/credentials/token.json")));
        assert!(!is_sensitive_path(Path::new("src/main.rs")));
        assert!(!is_sensitive_path(Path::new("dsh/src/secrets.rs")));
        assert!(!is_sensitive_path(Path::new("src/credentials.rs")));
    }

    /// `SafetyGuard` used to keep a literal set, so a versioned or suffixed
    /// spelling of the same destructive tool walked straight past it.
    #[test]
    fn disk_destroying_commands_are_matched_by_family_and_by_stem() {
        for cmd in ["dd", "mkfs", "mkfs.ext4", "mkswap", "fdisk", "parted"] {
            assert!(is_disk_destroying_command(cmd), "{cmd}");
        }
        // Also when it arrives with a directory in front of it. Built rather
        // than written as a literal so the portability lint does not read it as
        // a claim about where the binary lives.
        let qualified = format!("{}/mkfs.xfs", "/sbin");
        assert!(is_disk_destroying_command(&qualified));
        for cmd in ["ddrescue", "mkdir", "make", "cat"] {
            assert!(!is_disk_destroying_command(cmd), "{cmd}");
        }
    }

    #[test]
    fn rm_flags_are_read_as_tokens_not_substrings() {
        let args = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        // Clusters in either order, and with extra flags.
        assert!(
            destructive_rm_warning(&args(&["-rf", "/"]))
                .is_some_and(|m| m.contains("extremely dangerous"))
        );
        assert!(
            destructive_rm_warning(&args(&["-fr", "/"]))
                .is_some_and(|m| m.contains("extremely dangerous"))
        );
        assert!(
            destructive_rm_warning(&args(&["-rfv", "target"]))
                .is_some_and(|m| m.contains("Recursive forced"))
        );

        // A long option that merely starts with the same letters is not `-r`.
        assert_eq!(destructive_rm_warning(&args(&["--reflink", "a"])), None);
        // A plain delete says nothing.
        assert_eq!(destructive_rm_warning(&args(&["a.txt"])), None);
    }

    /// The wrapper's option value used to be mistaken for the program, so
    /// `timeout 5 rm -rf /` was judged as a command called `5`.
    #[test]
    fn a_wrapper_offers_every_program_it_might_be_running() {
        let split = |line: &str| {
            shell_words::split(line)
                .unwrap()
                .into_iter()
                .collect::<Vec<_>>()
        };
        let programs = |line: &str| {
            command_candidates(&split(line))
                .into_iter()
                .map(|(program, _)| program)
                .collect::<Vec<_>>()
        };

        assert_eq!(programs("rm -rf /"), vec!["rm"]);
        assert_eq!(programs("FOO=bar rm -rf /"), vec!["rm"]);
        assert_eq!(programs("sudo rm -rf /"), vec!["sudo", "rm", "/"]);
        assert_eq!(
            programs("timeout 5 rm -rf /"),
            vec!["timeout", "5", "rm", "/"]
        );
        assert_eq!(
            programs("nice -n 10 rm -rf /"),
            vec!["nice", "10", "rm", "/"]
        );
        assert_eq!(
            programs("chroot /new rm -rf /"),
            vec!["chroot", "/new", "rm", "/"]
        );
        // A non-wrapper is not expanded: its arguments are arguments.
        assert_eq!(programs("echo rm -rf /"), vec!["echo"]);
    }

    #[test]
    fn compound_statements_are_recognised_by_their_leading_word() {
        assert_eq!(compound_statement_keyword("{"), Some("{"));
        assert_eq!(compound_statement_keyword("for"), Some("for"));
        // Ordinary commands, including ones that merely start the same.
        assert_eq!(compound_statement_keyword("iftop"), None);
        assert_eq!(compound_statement_keyword("rm"), None);
        assert_eq!(compound_statement_keyword("doas"), None);
    }

    #[test]
    fn segments_split_on_unquoted_operators_only() {
        assert_eq!(
            split_command_segments("echo hi; rm -rf /"),
            vec!["echo hi", "rm -rf /"]
        );
        assert_eq!(
            split_command_segments("true | rm -rf /"),
            vec!["true", "rm -rf /"]
        );
        assert_eq!(split_command_segments("a && b || c"), vec!["a", "b", "c"]);
        assert_eq!(split_command_segments("sleep 1 &"), vec!["sleep 1"]);
        // A quoted operator belongs to the argument.
        assert_eq!(
            split_command_segments("git commit -m 'a; b'"),
            vec!["git commit -m 'a; b'"]
        );
        assert_eq!(
            split_command_segments("echo \"x | y\""),
            vec!["echo \"x | y\""]
        );
    }

    #[test]
    fn assignment_tokens_are_names_not_options() {
        assert!(is_assignment_token("FOO=bar"));
        assert!(is_assignment_token("_x="));
        assert!(!is_assignment_token("rm"));
        assert!(!is_assignment_token("--opt=value"));
        assert!(!is_assignment_token("=bare"));
        assert!(!is_assignment_token("9FOO=bar"));
    }

    #[test]
    fn a_program_is_judged_by_its_stem() {
        assert_eq!(command_stem("curl"), "curl");
        assert_eq!(command_stem(&format!("{}/curl", "/usr/bin")), "curl");
        assert!(is_network_fetch_command(&format!("{}/curl", "/usr/bin")));
        assert!(is_code_execution_command(&format!("{}/bash", "/bin")));
        // A name that merely starts the same is a different program.
        assert!(!is_code_execution_command("bashful"));
        assert!(!is_network_fetch_command("curly"));
    }

    #[test]
    fn sensitive_text_redacts_common_secret_shapes() {
        let input =
            "API_KEY=abc curl --token qwe -H 'Authorization: Bearer xyz' https://x?token=123";
        let redacted = redact_sensitive_text(input);
        assert!(redacted.contains("API_KEY=***"));
        assert!(redacted.contains("--token ***"));
        assert!(redacted.contains("Authorization: Bearer ***"));
        assert!(redacted.contains("?token=***"));
        assert!(!redacted.contains("abc"));
        assert!(!redacted.contains("qwe"));
    }

    #[test]
    fn sensitive_text_accepts_custom_key_detector() {
        assert!(contains_sensitive_text_with("CUSTOM=value", |key| key == "CUSTOM"));
        let redacted = redact_sensitive_text_with("CUSTOM=value HOME=/tmp", |key| key == "CUSTOM");
        assert!(redacted.contains("CUSTOM=***"));
        assert!(redacted.contains("HOME=/tmp"));
        assert!(!redacted.contains("CUSTOM=value"));
    }
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
