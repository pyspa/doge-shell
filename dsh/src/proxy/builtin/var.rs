//! Var and Read command handlers.

use crate::shell::Shell;
use anyhow::{Context as _, Result};
use dsh_types::Context;
use std::borrow::Cow;
use std::fs::File;
use std::io::prelude::*;
use std::os::unix::io::FromRawFd;
use tabled::{Table, Tabled};

struct Var {
    key: String,
    value: String,
}

impl Tabled for Var {
    const LENGTH: usize = 2;

    fn fields(&self) -> Vec<Cow<'_, str>> {
        vec![
            Cow::Borrowed(self.key.as_str()),
            Cow::Borrowed(self.value.as_str()),
        ]
    }

    fn headers() -> Vec<Cow<'static, str>> {
        vec![Cow::Borrowed("key"), Cow::Borrowed("value")]
    }
}

/// Execute the `var` builtin command.
///
/// Displays all shell variables in a table format.
pub fn execute_var(shell: &mut Shell, ctx: &Context, _argv: Vec<String>) -> Result<()> {
    let vars: Vec<Var> = shell
        .environment
        .read()
        .variable_state
        .variables
        .iter()
        .map(|x| Var {
            key: x.0.to_owned(),
            value: x.1.to_owned(),
        })
        .collect();
    let table = Table::new(vars).to_string();
    ctx.write_stdout(table.as_str())?;
    Ok(())
}

/// Execute the `read` builtin command.
///
/// Reads input from stdin and assigns it to a variable.
pub fn execute_read(shell: &mut Shell, ctx: &Context, argv: Vec<String>) -> Result<()> {
    let Some(name) = argv.get(1) else {
        return Err(anyhow::anyhow!("variable name required"));
    };
    let mut stdin = Vec::new();
    let mut infile = std::mem::ManuallyDrop::new(unsafe { File::from_raw_fd(ctx.infile) });
    infile
        .read_to_end(&mut stdin)
        .context("read: failed to read input")?;
    let key = format!("${name}");
    let output = match std::str::from_utf8(&stdin) {
        Ok(s) => s.trim_end_matches('\n').to_owned(),
        Err(err) => {
            ctx.write_stderr(&format!("read: invalid UTF-8 input: {err}"))
                .ok();
            return Err(anyhow::anyhow!("invalid UTF-8 input: {}", err));
        }
    };

    shell
        .environment
        .write()
        .variable_state
        .variables
        .insert(key, output);
    Ok(())
}
