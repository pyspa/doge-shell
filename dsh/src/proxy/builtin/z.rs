//! Z (directory jump) command handler.

use crate::shell::Shell;
use anyhow::Result;
use dsh_builtin::ShellProxy;
use dsh_frecency::SortMethod;
use dsh_types::Context;

/// Parse arguments for z command.
///
/// Returns (interactive, list, clean, query).
pub fn parse_z_args(argv: &[String]) -> (bool, bool, bool, String) {
    let mut interactive = false;
    let mut list = false;
    let mut clean = false;
    let mut query_parts = Vec::new();

    // Start from index 1, skip command name
    for arg in argv.iter().skip(1) {
        if arg == "-i" || arg == "--interactive" {
            interactive = true;
        } else if arg == "-l" || arg == "--list" {
            list = true;
        } else if arg == "-c" || arg == "--clean" {
            clean = true;
        } else {
            query_parts.push(arg.clone());
        }
    }
    (interactive, list, clean, query_parts.join(" "))
}

/// Execute the `z` builtin command.
///
/// Jump to a frequently/recently used directory.
pub fn execute(shell: &mut Shell, ctx: &Context, argv: Vec<String>) -> Result<()> {
    // Handle subcommands first
    if argv.len() >= 2 {
        match argv[1].as_str() {
            "add" => {
                // z add <alias> [path]
                if argv.len() < 3 {
                    ctx.write_stderr("z add: usage: z add <alias> [path]")?;
                    return Err(anyhow::anyhow!("z add: missing alias name"));
                }
                let alias = &argv[2];
                let path = if argv.len() >= 4 {
                    argv[3].clone()
                } else {
                    std::env::current_dir()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default()
                };
                if shell.add_dir_alias(alias.clone(), path.clone()) {
                    ctx.write_stdout(&format!("✓ Added alias: {} → {}", alias, path))?;
                } else {
                    ctx.write_stderr(&format!("z add: failed to add alias '{}'", alias))?;
                }
                return Ok(());
            }
            "remove" | "rm" => {
                if argv.len() < 3 {
                    ctx.write_stderr("z remove: usage: z remove <alias>")?;
                    return Err(anyhow::anyhow!("z remove: missing alias name"));
                }
                let alias = &argv[2];
                if shell.remove_dir_alias(alias) {
                    ctx.write_stdout(&format!("✓ Removed alias: {}", alias))?;
                } else {
                    ctx.write_stderr(&format!("z remove: no alias named '{}'", alias))?;
                }
                return Ok(());
            }
            "aliases" | "alias" => {
                let aliases = shell.list_dir_aliases();
                if aliases.is_empty() {
                    ctx.write_stdout(
                        "No directory aliases defined. Use 'z add <alias> [path]' to create one.",
                    )?;
                } else {
                    ctx.write_stdout("Directory aliases:")?;
                    for (name, path) in aliases {
                        ctx.write_stdout(&format!("  {} → {}", name, path))?;
                    }
                }
                return Ok(());
            }
            _ => {}
        }
    }

    let (interactive, list, clean, query) = parse_z_args(&argv);

    if clean {
        if let Some(ref mut history) = shell.path_history {
            let mut history = history.lock();
            history.prune();
            let _ = history.save();
        }
        if query.is_empty() && !interactive && !list {
            return Ok(());
        }
    }

    // Check for alias match first
    if !query.is_empty()
        && let Some(alias_path) = shell.get_dir_alias(&query)
    {
        ctx.write_stdout(&format!("z: jumping to {} (alias)", alias_path))?;
        shell.changepwd(&alias_path)?;
        return Ok(());
    }

    // Handle "z -" for previous directory
    if query == "-" {
        if let Some(old_pwd) = shell.get_var("OLDPWD") {
            ctx.write_stdout(&format!("z: jumping to {}\n", old_pwd))?;
            shell.changepwd(&old_pwd)?;
            return Ok(());
        } else {
            ctx.write_stderr("z: OLDPWD not set")?;
            return Err(anyhow::anyhow!("z: OLDPWD not set"));
        }
    }

    if let Some(ref mut history) = shell.path_history {
        let history = history.clone();
        // We need to release the lock before calling select_item_with_skim because it might block
        // But here we need to read history to get candidates.
        // Ideally we should clone the data we need.
        let (results, _sort_method) = {
            let history = history.lock();
            if query.is_empty() {
                (history.sorted(&SortMethod::Recent), SortMethod::Recent)
            } else {
                (history.sort_by_match(&query), SortMethod::Frecent)
            }
        };

        if list {
            if results.is_empty() {
                ctx.write_stderr("z: no matching history found")?;
            } else {
                for item in results.iter().take(20) {
                    let score = if query.is_empty() {
                        item.get_frecency()
                    } else {
                        item.match_score as f32
                    };
                    ctx.write_stdout(&format!("{:<.1}   {}\n", score, item.item))?;
                }
            }
        } else if interactive || query.is_empty() {
            // Interactive mode or no query (default to interactive)
            if !results.is_empty() {
                // Convert to Candidates for skim
                let candidates: Vec<crate::completion::Candidate> = results
                    .iter()
                    .map(|item| {
                        crate::completion::Candidate::Item(
                            item.item.clone(),
                            format!("({:.1})", item.get_frecency()),
                        )
                    })
                    .collect();

                if let Some(selected) = crate::completion::select_item_with_skim(candidates, None) {
                    shell.changepwd(&selected)?;
                }
            } else {
                ctx.write_stderr("z: no matching history found")?;
            }
        } else {
            // Direct jump (query present, not interactive, not list)
            if !results.is_empty() {
                let target = &results[0].item;
                // Echo the target directory
                ctx.write_stdout(&format!("z: jumping to {}\n", target))?;
                shell.changepwd(target)?;
            } else {
                ctx.write_stderr("z: no matching history found")?;
                return Err(anyhow::anyhow!("z: no match found"));
            }
        }
    } else {
        ctx.write_stderr("z: history not available")?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_z_args() {
        // z -i
        let args = vec!["z".to_string(), "-i".to_string()];
        let (interactive, list, clean, query) = parse_z_args(&args);
        assert!(interactive);
        assert!(!list);
        assert!(!clean);
        assert_eq!(query, "");

        // z --interactive
        let args = vec!["z".to_string(), "--interactive".to_string()];
        let (interactive, list, clean, query) = parse_z_args(&args);
        assert!(interactive);
        assert!(!list);
        assert!(!clean);
        assert_eq!(query, "");

        // z -l
        let args = vec!["z".to_string(), "-l".to_string()];
        let (interactive, list, clean, query) = parse_z_args(&args);
        assert!(!interactive);
        assert!(list);
        assert!(!clean);
        assert_eq!(query, "");

        // z -c
        let args = vec!["z".to_string(), "-c".to_string()];
        let (interactive, list, clean, query) = parse_z_args(&args);
        assert!(!interactive);
        assert!(!list);
        assert!(clean);
        assert_eq!(query, "");

        // z foo
        let args = vec!["z".to_string(), "foo".to_string()];
        let (interactive, list, clean, query) = parse_z_args(&args);
        assert!(!interactive);
        assert!(!list);
        assert!(!clean);
        assert_eq!(query, "foo");

        // z -i foo
        let args = vec!["z".to_string(), "-i".to_string(), "foo".to_string()];
        let (interactive, list, clean, query) = parse_z_args(&args);
        assert!(interactive);
        assert!(!list);
        assert!(!clean);
        assert_eq!(query, "foo");

        // z foo -i
        let args = vec!["z".to_string(), "foo".to_string(), "-i".to_string()];
        let (interactive, list, clean, query) = parse_z_args(&args);
        assert!(interactive);
        assert!(!list);
        assert!(!clean);
        assert_eq!(query, "foo");

        // z foo bar (arguments joined)
        let args = vec!["z".to_string(), "foo".to_string(), "bar".to_string()];
        let (interactive, list, clean, query) = parse_z_args(&args);
        assert!(!interactive);
        assert!(!list);
        assert!(!clean);
        assert_eq!(query, "foo bar");
    }
}
