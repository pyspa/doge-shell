//! The `TaskGrant` options `agent run` and `cron add --agent` both accept.
//!
//! Kept in one place so an unattended cron job's grant means exactly what an
//! interactive `agent run`'s does: the same directory validation, the same
//! exact-host rule for `--network`, the same flag spellings a person's muscle
//! memory already has. Splitting them was how `--network 10.0.0.0/8` used to
//! slip past one caller and not the other.

use anyhow::{Result, bail};
use dsh_types::agent::TaskGrant;
use std::path::PathBuf;

/// Applies one grant-shaped option (`--read`, `--write`, `--allow-command`,
/// `--allow-mcp`, `--network` or `--env`) to `grant`.
///
/// Returns `Ok(true)` when `option` named one of these — the caller consumed
/// `value` and should move on — or `Ok(false)` when it did not, so the caller
/// can try its own options next. `Err` means it was a grant option but
/// `value` was not a valid one.
pub fn apply_grant_option(grant: &mut TaskGrant, option: &str, value: &str) -> Result<bool> {
    match option {
        "--read" | "--write" => {
            let path = PathBuf::from(shellexpand::tilde(value).as_ref()).canonicalize()?;
            if !path.is_dir() {
                bail!("grant must name an existing directory");
            }
            if option == "--read" {
                grant.read_roots.push(path);
            } else {
                grant.write_roots.push(path);
            }
            Ok(true)
        }
        "--allow-command" => {
            grant.commands.push(value.to_string());
            Ok(true)
        }
        "--allow-mcp" => {
            grant.mcp_calls.push(value.to_string());
            Ok(true)
        }
        "--network" => {
            // Validate and store the same (trimmed) string - checking
            // `value.trim().is_empty()` but pushing the untrimmed `value`
            // let a host with stray leading/trailing whitespace pass
            // validation while being stored padded, silently breaking the
            // exact-match comparison this grant exists to guarantee.
            let host = value.trim();
            if host.contains(['/', ':', '*']) || host.is_empty() {
                bail!("network grant must be an exact host");
            }
            grant.network_hosts.push(host.to_string());
            Ok(true)
        }
        "--env" => {
            // A name-only grant: the value is read from the shell's own
            // environment at run time (`proxy.get_var`), never from this
            // flag. `--env NAME=value` looks like it sets `value`, but
            // nothing ever reads a `name=value` pair back out of `environment`
            // - it silently granted a variable named `NAME=value`, which
            // matches nothing, while whoever wrote the job believed the
            // right-hand side was in effect.
            if value.contains('=') {
                bail!(
                    "--env takes a variable NAME only (its value comes from the environment at \
                     run time, not from this flag) - got `{value}`"
                );
            }
            grant.environment.push(value.to_string());
            Ok(true)
        }
        _ => Ok(false),
    }
}

#[cfg(test)]
mod tests;
