//! Render literal argv as dogesh source, without introducing shell expansion.
use anyhow::{Result, bail};

/// One executable name and literal arguments, never a shell program.
///
/// Spaces, empty arguments, patterns, substitutions and separators are data.
/// NUL cannot be represented by exec argv; an empty executable is rejected.
/// Compound shell source must use a separate source contract, not this API.
/// Newlines are retained inside quotes, so the result may span display lines.
pub(crate) fn render_literal_command(command: &str, args: &[String]) -> Result<String> {
    if command.is_empty() || command.contains('\0') || args.iter().any(|arg| arg.contains('\0')) {
        bail!("literal argv requires a nonempty executable and no NUL bytes");
    }
    // Quoted argv0 also bypasses syntax-time aliases, which could otherwise
    // change the executable and add arguments to a literal response.
    let mut source = render_word(command, true);
    for arg in args {
        source.push(' ');
        source.push_str(&render_word(arg, false));
    }
    Ok(source)
}

fn render_word(value: &str, force_quote: bool) -> String {
    // The single-quoted empty word carries an explicit empty literal in the
    // planner; an empty double-quoted word currently has no parts.
    if value.is_empty() {
        return "''".into();
    }
    // Conservative bare spelling also excludes assignment syntax, comments,
    // tilde, braces and patterns. Quoting must follow dogesh's double-quote
    // escapes; POSIX single-quote splicing differs from its reader grammar.
    if !force_quote
        && !value.is_empty()
        && value.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '/' | '.' | ':' | '@' | '+' | ',')
        })
    {
        return value.to_owned();
    }
    let mut source = String::from("\"");
    for c in value.chars() {
        if matches!(c, '\\' | '"' | '$' | '`') {
            source.push('\\');
        }
        source.push(c);
    }
    source.push('"');
    source
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unrepresentable_argv() {
        assert!(render_literal_command("", &[]).is_err());
        assert!(render_literal_command("ec\0ho", &[]).is_err());
        assert!(render_literal_command("echo", &["a\0b".into()]).is_err());
    }
}
