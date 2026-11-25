pub mod editor;
pub mod skim;

/// Wrap a value in single quotes so it is treated as a single literal argument
/// by a POSIX shell. Any embedded single quote is escaped using the standard
/// `'\''` sequence. Use this before splicing any user-controlled value into a
/// string that is later executed via `sh -c` (script completion templates,
/// preview commands, etc.).
pub(crate) fn shell_quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}
