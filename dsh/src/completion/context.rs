use crate::completion::Candidate;
use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use tracing::{debug, warn};

/// Context-aware completion system
#[allow(dead_code)]
pub struct ContextCompletion {
    completers: HashMap<String, Box<dyn CommandCompleter>>,
}

#[allow(dead_code)]
impl ContextCompletion {
    pub fn new() -> Self {
        let mut completers: HashMap<String, Box<dyn CommandCompleter>> = HashMap::new();

        // Register built-in completers
        completers.insert("git".to_string(), Box::new(GitCompleter::new()));
        completers.insert("cargo".to_string(), Box::new(CargoCompleter::new()));
        completers.insert("npm".to_string(), Box::new(NpmCompleter::new()));

        Self { completers }
    }

    pub fn complete(
        &self,
        cmd: &str,
        args: &[String],
        cursor_pos: usize,
        current_dir: &Path,
    ) -> Vec<Candidate> {
        debug!(
            "Context completion for command: {} with args: {:?}",
            cmd, args
        );

        if let Some(completer) = self.completers.get(cmd) {
            debug!("Found built-in completer for command: {}", cmd);
            match completer.complete(args, cursor_pos, current_dir) {
                Ok(candidates) => {
                    debug!("Built-in completer returned {} candidates for {}", candidates.len(), cmd);
                    candidates
                },
                Err(e) => {
                    warn!("Context completion failed for {}: {}", cmd, e);
                    vec![]
                }
            }
        } else {
            debug!("No built-in completer found for command: {}, trying generic options", cmd);
            // Fallback to generic option completion
            let generic_candidates = self.complete_generic_options(cmd, args);
            debug!("Generic options returned {} candidates for {}", generic_candidates.len(), cmd);
            generic_candidates
        }
    }

    fn complete_generic_options(&self, cmd: &str, args: &[String]) -> Vec<Candidate> {
        // Common options for most commands
        let common_options = vec![
            "--help",
            "-h",
            "--version",
            "-v",
            "--verbose",
            "--quiet",
            "-q",
        ];

        let empty_string = String::new();
        let current_arg = args.last().unwrap_or(&empty_string);
        if current_arg.starts_with('-') {
            common_options
                .into_iter()
                .filter(|opt| opt.starts_with(current_arg))
                .map(|opt| Candidate::Option {
                    name: opt.to_string(),
                    description: format!("Common option for {}", cmd),
                })
                .collect()
        } else {
            vec![]
        }
    }
}

/// Trait for command-specific completers
pub trait CommandCompleter: Send + Sync {
    fn complete(
        &self,
        args: &[String],
        cursor_pos: usize,
        current_dir: &Path,
    ) -> Result<Vec<Candidate>>;
}

/// Git command completer
pub struct GitCompleter {
    subcommands: Vec<GitSubcommand>,
}

#[derive(Debug, Clone)]
struct GitSubcommand {
    name: String,
    description: String,
    #[allow(dead_code)]
    aliases: Vec<String>,
}

impl GitCompleter {
    pub fn new() -> Self {
        let subcommands = vec![
            GitSubcommand {
                name: "add".to_string(),
                description: "Add file contents to the index".to_string(),
                aliases: vec!["a".to_string()],
            },
            GitSubcommand {
                name: "commit".to_string(),
                description: "Record changes to the repository".to_string(),
                aliases: vec!["c".to_string()],
            },
            GitSubcommand {
                name: "checkout".to_string(),
                description: "Switch branches or restore working tree files".to_string(),
                aliases: vec!["co".to_string()],
            },
            GitSubcommand {
                name: "branch".to_string(),
                description: "List, create, or delete branches".to_string(),
                aliases: vec!["br".to_string()],
            },
            GitSubcommand {
                name: "push".to_string(),
                description: "Update remote refs along with associated objects".to_string(),
                aliases: vec![],
            },
            GitSubcommand {
                name: "pull".to_string(),
                description: "Fetch from and integrate with another repository".to_string(),
                aliases: vec![],
            },
            GitSubcommand {
                name: "status".to_string(),
                description: "Show the working tree status".to_string(),
                aliases: vec!["st".to_string()],
            },
            GitSubcommand {
                name: "log".to_string(),
                description: "Show commit logs".to_string(),
                aliases: vec![],
            },
            GitSubcommand {
                name: "diff".to_string(),
                description: "Show changes between commits, commit and working tree, etc"
                    .to_string(),
                aliases: vec![],
            },
            GitSubcommand {
                name: "merge".to_string(),
                description: "Join two or more development histories together".to_string(),
                aliases: vec![],
            },
        ];

        Self { subcommands }
    }

    fn get_git_branches(&self, current_dir: &Path) -> Result<Vec<String>> {
        let output = Command::new("git")
            .args(["branch", "--all", "--format=%(refname:short)"])
            .current_dir(current_dir)
            .output()?;

        if output.status.success() {
            let branches = String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(|line| line.trim().to_string())
                .filter(|line| !line.is_empty() && !line.starts_with("origin/HEAD"))
                .collect();
            Ok(branches)
        } else {
            Ok(vec![])
        }
    }

    fn get_modified_files(&self, current_dir: &Path) -> Result<Vec<String>> {
        let output = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(current_dir)
            .output()?;

        if output.status.success() {
            let files = String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|line| {
                    if line.len() > 3 {
                        Some(line[3..].trim().to_string())
                    } else {
                        None
                    }
                })
                .collect();
            Ok(files)
        } else {
            Ok(vec![])
        }
    }
}

impl CommandCompleter for GitCompleter {
    fn complete(
        &self,
        args: &[String],
        _cursor_pos: usize,
        current_dir: &Path,
    ) -> Result<Vec<Candidate>> {
        if args.is_empty() {
            // Complete git subcommands
            Ok(self
                .subcommands
                .iter()
                .map(|subcmd| Candidate::Command {
                    name: subcmd.name.clone(),
                    description: subcmd.description.clone(),
                })
                .collect())
        } else {
            let subcommand = &args[0];
            match subcommand.as_str() {
                "checkout" | "co" => {
                    // Complete branch names
                    let branches = self.get_git_branches(current_dir)?;
                    Ok(branches
                        .into_iter()
                        .map(|branch| Candidate::GitBranch {
                            name: branch.clone(),
                            is_current: false, // TODO: detect current branch
                        })
                        .collect())
                }
                "add" => {
                    // Complete modified files
                    let files = self.get_modified_files(current_dir)?;
                    Ok(files
                        .into_iter()
                        .map(|file| Candidate::File {
                            path: file,
                            is_dir: false,
                        })
                        .collect())
                }
                "commit" => {
                    // Complete commit options
                    let options = vec![
                        ("-m", "Use the given message as the commit message"),
                        (
                            "-a",
                            "Automatically stage files that have been modified and deleted",
                        ),
                        (
                            "--amend",
                            "Replace the tip of the current branch by creating a new commit",
                        ),
                        (
                            "--no-edit",
                            "Use the selected commit message without launching an editor",
                        ),
                    ];

                    Ok(options
                        .into_iter()
                        .map(|(opt, desc)| Candidate::Option {
                            name: opt.to_string(),
                            description: desc.to_string(),
                        })
                        .collect())
                }
                _ => Ok(vec![]),
            }
        }
    }
}

/// Cargo command completer
pub struct CargoCompleter {
    subcommands: Vec<CargoSubcommand>,
}

#[derive(Debug, Clone)]
struct CargoSubcommand {
    name: String,
    description: String,
}

impl CargoCompleter {
    pub fn new() -> Self {
        let subcommands = vec![
            CargoSubcommand {
                name: "build".to_string(),
                description: "Compile the current package".to_string(),
            },
            CargoSubcommand {
                name: "run".to_string(),
                description: "Run a binary or example of the local package".to_string(),
            },
            CargoSubcommand {
                name: "test".to_string(),
                description: "Run the tests".to_string(),
            },
            CargoSubcommand {
                name: "check".to_string(),
                description: "Analyze the current package and report errors".to_string(),
            },
            CargoSubcommand {
                name: "clippy".to_string(),
                description: "Run clippy lints".to_string(),
            },
            CargoSubcommand {
                name: "fmt".to_string(),
                description: "Format the code".to_string(),
            },
            CargoSubcommand {
                name: "add".to_string(),
                description: "Add dependencies to Cargo.toml".to_string(),
            },
            CargoSubcommand {
                name: "remove".to_string(),
                description: "Remove dependencies from Cargo.toml".to_string(),
            },
            CargoSubcommand {
                name: "update".to_string(),
                description: "Update dependencies".to_string(),
            },
            CargoSubcommand {
                name: "publish".to_string(),
                description: "Upload a package to the registry".to_string(),
            },
        ];

        Self { subcommands }
    }
}

impl CommandCompleter for CargoCompleter {
    fn complete(
        &self,
        args: &[String],
        _cursor_pos: usize,
        _current_dir: &Path,
    ) -> Result<Vec<Candidate>> {
        if args.is_empty() {
            // Complete cargo subcommands
            Ok(self
                .subcommands
                .iter()
                .map(|subcmd| Candidate::Command {
                    name: subcmd.name.clone(),
                    description: subcmd.description.clone(),
                })
                .collect())
        } else {
            let subcommand = &args[0];
            match subcommand.as_str() {
                "build" | "run" | "test" | "check" => {
                    // Complete common build options
                    let options = vec![
                        ("--release", "Build artifacts in release mode"),
                        ("--dev", "Build artifacts in development mode"),
                        ("--target", "Build for the target triple"),
                        (
                            "--features",
                            "Space or comma separated list of features to activate",
                        ),
                        ("--all-features", "Activate all available features"),
                        (
                            "--no-default-features",
                            "Do not activate the default feature",
                        ),
                    ];

                    Ok(options
                        .into_iter()
                        .map(|(opt, desc)| Candidate::Option {
                            name: opt.to_string(),
                            description: desc.to_string(),
                        })
                        .collect())
                }
                _ => Ok(vec![]),
            }
        }
    }
}

/// npm command completer
pub struct NpmCompleter {
    subcommands: Vec<NpmSubcommand>,
}

#[derive(Debug, Clone)]
struct NpmSubcommand {
    name: String,
    description: String,
}

impl NpmCompleter {
    pub fn new() -> Self {
        let subcommands = vec![
            NpmSubcommand {
                name: "install".to_string(),
                description: "Install a package".to_string(),
            },
            NpmSubcommand {
                name: "run".to_string(),
                description: "Run arbitrary package scripts".to_string(),
            },
            NpmSubcommand {
                name: "start".to_string(),
                description: "Start a package".to_string(),
            },
            NpmSubcommand {
                name: "test".to_string(),
                description: "Test a package".to_string(),
            },
            NpmSubcommand {
                name: "build".to_string(),
                description: "Build a package".to_string(),
            },
            NpmSubcommand {
                name: "init".to_string(),
                description: "Create a package.json file".to_string(),
            },
            NpmSubcommand {
                name: "update".to_string(),
                description: "Update packages".to_string(),
            },
            NpmSubcommand {
                name: "uninstall".to_string(),
                description: "Remove a package".to_string(),
            },
        ];

        Self { subcommands }
    }

    fn get_npm_scripts(&self, current_dir: &Path) -> Result<Vec<String>> {
        let package_json_path = current_dir.join("package.json");
        if !package_json_path.exists() {
            return Ok(vec![]);
        }

        let content = std::fs::read_to_string(package_json_path)?;
        let json: serde_json::Value = serde_json::from_str(&content)?;

        if let Some(scripts) = json.get("scripts").and_then(|s| s.as_object()) {
            Ok(scripts.keys().cloned().collect())
        } else {
            Ok(vec![])
        }
    }
}

impl CommandCompleter for NpmCompleter {
    fn complete(
        &self,
        args: &[String],
        _cursor_pos: usize,
        current_dir: &Path,
    ) -> Result<Vec<Candidate>> {
        if args.is_empty() {
            // Complete npm subcommands
            Ok(self
                .subcommands
                .iter()
                .map(|subcmd| Candidate::Command {
                    name: subcmd.name.clone(),
                    description: subcmd.description.clone(),
                })
                .collect())
        } else {
            let subcommand = &args[0];
            match subcommand.as_str() {
                "run" => {
                    // Complete npm scripts from package.json
                    let scripts = self.get_npm_scripts(current_dir)?;
                    Ok(scripts
                        .into_iter()
                        .map(|script| Candidate::NpmScript { name: script })
                        .collect())
                }
                _ => Ok(vec![]),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn test_context_completion_creation() {
        let completion = ContextCompletion::new();
        assert!(completion.completers.contains_key("git"));
        assert!(completion.completers.contains_key("cargo"));
        assert!(completion.completers.contains_key("npm"));
    }

    #[test]
    fn test_git_completer_subcommands() {
        let completer = GitCompleter::new();
        let current_dir = env::current_dir().unwrap();
        let result = completer.complete(&[], 0, &current_dir).unwrap();

        assert!(!result.is_empty());
        // Check if common git subcommands are present
        let names: Vec<String> = result
            .iter()
            .filter_map(|c| {
                if let Candidate::Command { name, .. } = c {
                    Some(name.clone())
                } else {
                    None
                }
            })
            .collect();

        assert!(names.contains(&"add".to_string()));
        assert!(names.contains(&"commit".to_string()));
        assert!(names.contains(&"checkout".to_string()));
    }

    #[test]
    fn test_cargo_completer_subcommands() {
        let completer = CargoCompleter::new();
        let current_dir = env::current_dir().unwrap();
        let result = completer.complete(&[], 0, &current_dir).unwrap();

        assert!(!result.is_empty());
        let names: Vec<String> = result
            .iter()
            .filter_map(|c| {
                if let Candidate::Command { name, .. } = c {
                    Some(name.clone())
                } else {
                    None
                }
            })
            .collect();

        assert!(names.contains(&"build".to_string()));
        assert!(names.contains(&"test".to_string()));
        assert!(names.contains(&"run".to_string()));
    }
}
