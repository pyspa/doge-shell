use serde_json::{Value, json};
use shell_words::split;
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Command;
use xdg::BaseDirectories;

pub(crate) const NAME: &str = "execute";

const EXECUTE_TOOL_CONFIG_FILE: &str = "openai-execute-tool.json";
const EXECUTE_TOOL_ENV_ALLOWLIST: &str = "AI_CHAT_EXECUTE_ALLOWLIST";
const EXECUTE_TOOL_CONFIG_OVERRIDE_ENV: &str = "DSH_EXECUTE_TOOL_CONFIG";
const CONFIG_DIR_PREFIX: &str = "dsh";

pub(crate) fn definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": NAME,
            "description": "Execute an allowed shell command via bash and return its exit code, stdout, and stderr. Configure the allowlist in ~/.config/dsh/openai-execute-tool.json or the AI_CHAT_EXECUTE_ALLOWLIST environment variable.",
            "parameters": {
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "Command line to execute. The first program token must appear in the configured allowlist."
                    }
                },
                "required": ["command"],
                "additionalProperties": false
            }
        }
    })
}

pub(crate) fn run(arguments: &str) -> Result<String, String> {
    let parsed: Value = serde_json::from_str(arguments)
        .map_err(|err| format!("chat: invalid JSON arguments for execute tool: {err}"))?;

    let command = parsed
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "chat: execute tool requires `command`".to_string())?;

    if command.trim().is_empty() {
        return Err("chat: execute tool command must not be empty".to_string());
    }

    let allowlist = load_allowed_commands()?;
    if allowlist.is_empty() {
        return Err(format!(
            "chat: execute tool has no allowed commands configured. Add entries to ~/.config/dsh/{EXECUTE_TOOL_CONFIG_FILE} or set {EXECUTE_TOOL_ENV_ALLOWLIST}."
        ));
    }

    let program = extract_program_name(command)?;

    if !allowlist.iter().any(|item| item == &program) {
        return Err(format!(
            "chat: execute tool command `{program}` is not permitted. Allowed commands: {}",
            allowlist.join(", ")
        ));
    }

    let output = Command::new("bash")
        .arg("-lc")
        .arg(command)
        .output()
        .map_err(|err| format!("chat: failed to spawn bash: {err}"))?;

    let exit_code = output.status.code().unwrap_or(-1);
    let stdout_text = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr_text = String::from_utf8_lossy(&output.stderr).to_string();

    if !stdout_text.is_empty() {
        let mut stdout = io::stdout();
        write_all(&mut stdout, stdout_text.as_bytes())?;
    }

    if !stderr_text.is_empty() {
        let mut stderr = io::stderr();
        write_all(&mut stderr, stderr_text.as_bytes())?;
    }

    let result = json!({
        "exit_code": exit_code,
        "stdout": stdout_text,
        "stderr": stderr_text,
    });

    Ok(result.to_string())
}

fn write_all(target: &mut dyn Write, data: &[u8]) -> Result<(), String> {
    target
        .write_all(data)
        .and_then(|_| target.flush())
        .map_err(|err| format!("chat: failed to write command output: {err}"))
}

fn extract_program_name(command: &str) -> Result<String, String> {
    let tokens = split(command).map_err(|err| format!("chat: failed to parse command: {err}"))?;
    tokens
        .first()
        .cloned()
        .ok_or_else(|| "chat: execute tool command must specify a program".to_string())
}

fn load_allowed_commands() -> Result<Vec<String>, String> {
    if let Some(commands) = read_allowlist_from_env() {
        return Ok(commands);
    }

    let Some(config_path) = resolve_allowlist_path()? else {
        return Ok(Vec::new());
    };

    let contents = fs::read_to_string(&config_path).map_err(|err| {
        format!(
            "chat: failed to read execute tool config {}: {err}",
            config_path.display()
        )
    })?;

    if contents.trim().is_empty() {
        return Ok(Vec::new());
    }

    let raw: Value = serde_json::from_str(&contents).map_err(|err| {
        format!(
            "chat: failed to parse {} as JSON: {err}",
            config_path.display()
        )
    })?;

    let Some(array) = raw.get("allowed_commands").and_then(|v| v.as_array()) else {
        return Err(format!(
            "chat: execute tool config {} must contain `allowed_commands` array",
            config_path.display()
        ));
    };

    Ok(array
        .iter()
        .filter_map(|value| value.as_str())
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .collect())
}

fn read_allowlist_from_env() -> Option<Vec<String>> {
    let raw = env::var(EXECUTE_TOOL_ENV_ALLOWLIST).ok()?;
    let entries: Vec<String> = raw
        .split([',', '\n'])
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .collect();

    if entries.is_empty() {
        None
    } else {
        Some(entries)
    }
}

fn resolve_allowlist_path() -> Result<Option<PathBuf>, String> {
    if let Ok(path) = env::var(EXECUTE_TOOL_CONFIG_OVERRIDE_ENV) {
        return Ok(Some(PathBuf::from(path)));
    }

    let xdg_dirs = BaseDirectories::with_prefix(CONFIG_DIR_PREFIX)
        .map_err(|err| format!("chat: failed to determine config directory: {err}"))?;

    Ok(xdg_dirs.find_config_file(EXECUTE_TOOL_CONFIG_FILE))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn extract_program_name_returns_first_token() {
        assert_eq!(extract_program_name("ls -la").unwrap(), "ls");
        assert_eq!(extract_program_name("git status").unwrap(), "git");
    }

    #[test]
    fn load_allowlist_prefers_env() {
        let _guard = EnvGuard::set(EXECUTE_TOOL_ENV_ALLOWLIST, "ls,git\ncat");
        assert_eq!(load_allowed_commands().unwrap(), vec!["ls", "git", "cat"]);
    }

    #[test]
    fn load_allowlist_reads_config_file() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("allow.json");
        let contents = json!({ "allowed_commands": ["ls", "cargo"] }).to_string();
        std::fs::write(&config_path, contents).unwrap();

        let _env_guard = EnvGuard::set(
            EXECUTE_TOOL_CONFIG_OVERRIDE_ENV,
            config_path.to_str().unwrap(),
        );
        unsafe {
            env::remove_var(EXECUTE_TOOL_ENV_ALLOWLIST);
        }

        assert_eq!(load_allowed_commands().unwrap(), vec!["ls", "cargo"]);
    }

    struct EnvGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = env::var(key).ok();
            unsafe {
                env::set_var(key, value);
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(value) = &self.previous {
                unsafe {
                    env::set_var(self.key, value);
                }
            } else {
                unsafe {
                    env::remove_var(self.key);
                }
            }
        }
    }
}
