//! Var and Read command handlers.

use crate::environment::variables::is_valid_shell_var_name;
use crate::shell::Shell;
use anyhow::Result;
use dsh_types::{Context, ExitStatus};
use std::fs::File;
use std::io::Read as _;
use std::os::unix::io::FromRawFd;

/// Execute the `var` builtin command.
///
/// Displays all shell variables, one `KEY=VALUE` per line, sorted by key.
/// A table renderer was used here before, but with the unified
/// variable/export namespace `var` now lists every inherited variable too
/// (90+ rows with a multi-kilobyte `PATH`); the table layout hung rendering
/// that shape, while line output stays linear and preserves full values.
pub fn execute_var(shell: &mut Shell, ctx: &Context, _argv: Vec<String>) -> Result<()> {
    let mut vars: Vec<(String, String)> = shell
        .environment
        .read()
        .variable_state
        .variables
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    vars.sort_by(|a, b| a.0.cmp(&b.0));
    let mut output = String::new();
    for (key, value) in &vars {
        output.push_str(key);
        output.push('=');
        output.push_str(value);
        output.push('\n');
    }
    let output = output.strip_suffix('\n').unwrap_or("");
    if !output.is_empty() {
        ctx.write_stdout(output)?;
    }
    Ok(())
}

/// Execute the `read` builtin command.
///
/// Only `read NAME` is supported. One line is consumed from stdin without
/// read-ahead (one byte at a time), so a second `read` sees the next line.
/// EOF is a command status, never an infrastructure error.
pub fn execute_read_line(
    shell: &mut Shell,
    ctx: &Context,
    argv: Vec<String>,
) -> Result<ExitStatus> {
    let name = match argv.as_slice() {
        [_] => {
            ctx.write_stderr("read: variable name required").ok();
            return Ok(ExitStatus::ExitedWith(1));
        }
        [_, name] if name.starts_with('-') && !is_valid_shell_var_name(name) => {
            // `-r`, `--anything`, and any other dash-led token: unsupported,
            // never silently accepted.
            ctx.write_stderr(&format!("read: unsupported option: {name}"))
                .ok();
            return Ok(ExitStatus::ExitedWith(1));
        }
        [_, name] => {
            if !is_valid_shell_var_name(name) {
                ctx.write_stderr(&format!("read: invalid variable name: {name}"))
                    .ok();
                return Ok(ExitStatus::ExitedWith(1));
            }
            name.clone()
        }
        _ => {
            ctx.write_stderr("read: expected exactly one variable name")
                .ok();
            return Ok(ExitStatus::ExitedWith(1));
        }
    };

    let (bytes, terminated) = read_single_line(ctx.infile)
        .map_err(|e| anyhow::anyhow!("read: failed to read input: {e}"))?;
    if bytes.is_empty() && !terminated {
        // EOF before any byte: defined empty value, nonzero, silent.
        shell.environment.write().set_shell_var(name, String::new());
        return Ok(ExitStatus::ExitedWith(1));
    }
    let line = match String::from_utf8(bytes) {
        Ok(line) => line,
        Err(err) => {
            ctx.write_stderr(&format!("read: invalid UTF-8 input: {err}"))
                .ok();
            return Ok(ExitStatus::ExitedWith(1));
        }
    };
    shell.environment.write().set_shell_var(name, line);
    if terminated {
        Ok(ExitStatus::ExitedWith(0))
    } else {
        Ok(ExitStatus::ExitedWith(1))
    }
}

/// Read one line from a raw fd without read-ahead.
///
/// Returns the bytes before the newline and whether a terminating newline
/// was seen. `Interrupted` is retried; any other I/O error propagates.
/// The fd is borrowed, never closed.
fn read_single_line(fd: std::os::unix::io::RawFd) -> std::io::Result<(Vec<u8>, bool)> {
    let mut file = std::mem::ManuallyDrop::new(unsafe { File::from_raw_fd(fd) });
    let mut bytes = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match file.read(&mut byte) {
            Ok(0) => return Ok((bytes, false)),
            Ok(_) => {
                if byte[0] == b'\n' {
                    return Ok((bytes, true));
                }
                bytes.push(byte[0]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
}

impl dsh_builtin::shell_capabilities::ReadCapability for Shell {
    fn read_shell_line(&mut self, ctx: &Context, argv: Vec<String>) -> Result<ExitStatus> {
        execute_read_line(self, ctx, argv)
    }
}
