/// Generate Lisp code for a macro definition.
///
/// Uses `fn` rather than `defun`: only `fn` marks the lambda as exported
/// (`Lambda::export`), and command dispatch resolves user-defined names through
/// `LispEngine::is_export`. A macro defined with `defun` would never be callable
/// as a shell command.
pub fn generate_macro_lisp(name: &str, commands: &[String]) -> String {
    let mut body = String::new();
    for cmd in commands {
        // Escape backslashes first to preserve them
        let escaped_backslashes = cmd.replace('\\', "\\\\");
        // Then escape quotes
        let escaped_quotes = escaped_backslashes.replace('"', "\\\"");
        body.push_str(&format!("  (sh \"{}\")\n", escaped_quotes));
    }

    format!("\n(fn {} ()\n{}\n)\n", name, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_command() {
        let commands = vec!["echo hello".to_string()];
        let lisp = generate_macro_lisp("test-macro", &commands);
        assert!(lisp.contains("(fn test-macro ()"));
        assert!(lisp.contains("  (sh \"echo hello\")"));
    }

    /// `defun` produces a non-exported lambda, which command dispatch skips.
    /// Guard against a regression back to it.
    #[test]
    fn test_uses_exported_fn_form() {
        let lisp = generate_macro_lisp("m", &["echo hi".to_string()]);
        assert!(!lisp.contains("defun"));
    }

    #[test]
    fn test_quoted_command() {
        let commands = vec!["echo \"hello world\"".to_string()];
        let lisp = generate_macro_lisp("quoted-macro", &commands);
        // "echo \"hello world\"" -> \"echo \\\"hello world\\\"\"
        // effectively: (sh "echo \"hello world\"")
        assert!(lisp.contains("  (sh \"echo \\\"hello world\\\"\")"));
    }

    #[test]
    fn test_backslash_command() {
        let commands = vec![r"echo \".to_string()];
        let lisp = generate_macro_lisp("backslash-macro", &commands);
        // echo \ -> echo \\ in string literal -> (sh "echo \\")
        assert!(lisp.contains(r#"  (sh "echo \\")"#));
    }

    #[test]
    fn test_complex_mix() {
        let commands = vec!["awk '{print $1}'".to_string()];
        let lisp = generate_macro_lisp("awk-macro", &commands);
        assert!(lisp.contains("  (sh \"awk '{print $1}'\")"));
    }

    #[test]
    fn test_multiple_commands() {
        let commands = vec!["echo one".to_string(), "echo two".to_string()];
        let lisp = generate_macro_lisp("multi-macro", &commands);
        assert!(lisp.contains("  (sh \"echo one\")"));
        assert!(lisp.contains("  (sh \"echo two\")"));
    }
}
