//! Shared validation for shell variable assignment targets.

/// Accept the shell identifier grammar `[A-Za-z_][A-Za-z0-9_]*`.
pub fn is_valid_shell_var_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}
