//! Reload command handler.

use crate::shell::Shell;
use anyhow::Result;
use dsh_types::Context;

/// Execute the `reload` builtin command.
///
/// Reloads the shell configuration from config.lisp.
pub fn execute(shell: &mut Shell, ctx: &Context, _argv: Vec<String>) -> Result<()> {
    match shell.lisp_engine.borrow().run_config_lisp() {
        Ok(_) => {
            // Get the config file path for the success message
            match crate::environment::get_config_file(crate::lisp::CONFIG_FILE) {
                Ok(config_path) => {
                    shell.reload_mcp_config();
                    ctx.write_stdout(&format!(
                        "Configuration reloaded successfully from {}",
                        config_path.display()
                    ))?;
                }
                Err(_) => {
                    // Fallback to generic message if path resolution fails
                    shell.reload_mcp_config();
                    ctx.write_stdout(
                        "Configuration reloaded successfully from ~/.config/dsh/config.lisp",
                    )?;
                }
            }
        }
        Err(err) => {
            // Format error message based on error type for better user experience
            let error_msg = format_reload_error(&err);
            ctx.write_stderr(&error_msg)?;
            return Err(err);
        }
    }
    Ok(())
}

/// Format reload error messages based on error type for better user experience.
pub fn format_reload_error(err: &anyhow::Error) -> String {
    let error_string = err.to_string();

    // Handle file not found errors
    if error_string.contains("No such file or directory")
        || error_string.contains("Failed to read config file")
    {
        if let Some(path_start) = error_string.find("~/.config/dsh/config.lisp") {
            let path_end = path_start + "~/.config/dsh/config.lisp".len();
            let config_path = &error_string[path_start..path_end];
            return format!("reload: file not found: {config_path}");
        } else if let Some(path_start) = error_string.rfind('/') {
            // Extract just the filename if full path is shown
            if let Some(path_end) = error_string[path_start..].find(' ') {
                let filename = &error_string[path_start + 1..path_start + path_end];
                return format!("reload: file not found: ~/.config/dsh/{filename}");
            }
        }
        return "reload: file not found: ~/.config/dsh/config.lisp".to_string();
    }

    // Handle permission denied errors
    if error_string.contains("Permission denied") {
        return "reload: permission denied: cannot read ~/.config/dsh/config.lisp".to_string();
    }

    // Handle XDG directory errors
    if error_string.contains("failed get xdg directory") {
        return "reload: configuration directory error: unable to access ~/.config/dsh/"
            .to_string();
    }

    // Handle Lisp parsing errors
    if error_string.contains("Parse error:") {
        // Extract the parse error details
        if let Some(parse_start) = error_string.find("Parse error:") {
            let parse_error = &error_string[parse_start..];
            return format!(
                "reload: syntax error: {}",
                parse_error.trim_start_matches("Parse error: ")
            );
        }
        return format!("reload: syntax error: {error_string}");
    }

    // Handle Lisp runtime errors
    if error_string.contains("Runtime error:") {
        // Extract the runtime error details
        if let Some(runtime_start) = error_string.find("Runtime error:") {
            let runtime_error = &error_string[runtime_start..];
            return format!(
                "reload: runtime error: {}",
                runtime_error.trim_start_matches("Runtime error: ")
            );
        }
        return format!("reload: runtime error: {error_string}");
    }

    // Handle other I/O errors
    if error_string.contains("I/O error") || error_string.contains("io::Error") {
        return format!("reload: I/O error: {error_string}");
    }

    // Generic error fallback with reload prefix
    format!("reload: {error_string}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_reload_error_file_not_found() {
        let err = anyhow::anyhow!("No such file or directory");
        let formatted = format_reload_error(&err);
        assert_eq!(
            formatted,
            "reload: file not found: ~/.config/dsh/config.lisp"
        );
    }

    #[test]
    fn test_format_reload_error_permission_denied() {
        let err = anyhow::anyhow!("Permission denied");
        let formatted = format_reload_error(&err);
        assert_eq!(
            formatted,
            "reload: permission denied: cannot read ~/.config/dsh/config.lisp"
        );
    }

    #[test]
    fn test_format_reload_error_xdg_directory() {
        let err = anyhow::anyhow!("failed get xdg directory");
        let formatted = format_reload_error(&err);
        assert_eq!(
            formatted,
            "reload: configuration directory error: unable to access ~/.config/dsh/"
        );
    }

    #[test]
    fn test_format_reload_error_parse_error() {
        let err = anyhow::anyhow!("Parse error: unexpected token ')' at index 15");
        let formatted = format_reload_error(&err);
        assert_eq!(
            formatted,
            "reload: syntax error: unexpected token ')' at index 15"
        );
    }

    #[test]
    fn test_format_reload_error_runtime_error() {
        let err = anyhow::anyhow!("Runtime error: undefined function 'invalid-func'");
        let formatted = format_reload_error(&err);
        assert_eq!(
            formatted,
            "reload: runtime error: undefined function 'invalid-func'"
        );
    }

    #[test]
    fn test_format_reload_error_generic() {
        let err = anyhow::anyhow!("some generic error");
        let formatted = format_reload_error(&err);
        assert_eq!(formatted, "reload: some generic error");
    }
}
