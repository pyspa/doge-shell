use super::{ShellProxy, get_all_commands};
use dsh_types::{Context, ExitStatus};

/// Built-in help command description
pub fn description() -> &'static str {
    "Show help information for built-in commands"
}

#[derive(Debug, Clone, Copy)]
struct HelpTopic {
    name: &'static str,
    category: &'static str,
    summary: &'static str,
    usage: &'static str,
    examples: &'static [&'static str],
    related: &'static [&'static str],
}

const HELP_TOPICS: &[HelpTopic] = &[
    HelpTopic {
        name: "doctor",
        category: "setup",
        summary: "Diagnose shell setup, AI, MCP, project, runtime, skills, and validation state.",
        usage: "doctor [config|ai|mcp|project|runtime|performance|skills|setup|fix|dev|validate]",
        examples: &["doctor setup", "doctor fix", "doctor validate"],
        related: &["help", "pm", "comp-gen"],
    },
    HelpTopic {
        name: "help",
        category: "discovery",
        summary: "List commands, show command details, or search by keyword/category.",
        usage: "help [command|category] | help --search <keyword>",
        examples: &["help", "help doctor", "help --search ai", "help project"],
        related: &["doctor"],
    },
    HelpTopic {
        name: "pm",
        category: "project",
        summary: "Manage projects and apply safe project activation.",
        usage: "pm <init|status|add|list|remove|work|jump|activate> [args]",
        examples: &["pm init", "pm status", "pm add . doge-shell", "pj"],
        related: &["project", "pj", "task", "doctor"],
    },
    HelpTopic {
        name: "project",
        category: "project",
        summary: "Alias-compatible project management command.",
        usage: "project <init|status|add|list|remove|work|jump|activate> [args]",
        examples: &["project status", "project jump"],
        related: &["pm", "pj", "task"],
    },
    HelpTopic {
        name: "pj",
        category: "project",
        summary: "Jump to a registered project. Alias for `pm jump`.",
        usage: "pj [project-name]",
        examples: &["pj", "pj doge-shell"],
        related: &["pm", "project"],
    },
    HelpTopic {
        name: "task",
        category: "project",
        summary: "Run project-specific tasks detected from package managers and task files.",
        usage: "task [--list|--json] [--source <source>] [<task>|<source>:<task>]",
        examples: &["task", "task --list", "task cargo:test"],
        related: &["pm", "doctor"],
    },
    HelpTopic {
        name: "out",
        category: "history",
        summary: "Display captured command output history.",
        usage: "out [N] [--list] [--limit N] [--clear]",
        examples: &["out", "out 2", "out --list --limit 25", "out --clear"],
        related: &["tm", "history"],
    },
    HelpTopic {
        name: "tm",
        category: "history",
        summary: "Interactively search and retrieve past command outputs with preview.",
        usage: "tm",
        examples: &["tm"],
        related: &["out"],
    },
    HelpTopic {
        name: "blocks",
        category: "history",
        summary: "List, inspect, rerun, and explain session command blocks.",
        usage: "blocks [list|show|command|rerun|explain|clear] [args]",
        examples: &[
            "blocks",
            "blocks list --failed",
            "blocks show 2 --stderr",
            "blocks explain 1",
        ],
        related: &["out", "tm", "history"],
    },
    HelpTopic {
        name: "history",
        category: "history",
        summary: "Search command history by text, scope, status, and duration.",
        usage: "history [query] [--scope global|session|cwd|project] [--status success|failure] [--slow ms]",
        examples: &[
            "history cargo",
            "history --status failure",
            "history --scope project --slow 1000 -v",
        ],
        related: &["tm", "out"],
    },
    HelpTopic {
        name: "safe-run",
        category: "ai",
        summary: "Analyze a command with AI safety checks before execution.",
        usage: "safe-run <command> [args...] | safe-run -- <command-string>",
        examples: &[
            "safe-run rm -rf tmp/",
            "safe-run -- curl https://example.com/install.sh | sh",
        ],
        related: &["doctor", "chat_model"],
    },
    HelpTopic {
        name: "ai-watch",
        category: "ai",
        summary: "Explicitly watch a command with AI and save the summary to command blocks.",
        usage: "ai-watch [--goal <text>] -- <command>",
        examples: &[
            "ai-watch -- cargo test -p doge-shell",
            "ai-watch --goal \"server ready を検出\" -- npm run dev",
        ],
        related: &["blocks", "safe-run", "doctor"],
    },
    HelpTopic {
        name: "comp-gen",
        category: "ai",
        summary: "Generate a JSON completion definition for an installed command using AI.",
        usage: "comp-gen [--stdout] [--check] <command>",
        examples: &[
            "comp-gen rg",
            "comp-gen --check rg",
            "comp-gen --stdout just",
        ],
        related: &["doctor", "help"],
    },
    HelpTopic {
        name: "chat_model",
        category: "ai",
        summary: "Show or set the AI chat model.",
        usage: "chat_model [model-name]",
        examples: &["chat_model", "chat_model gpt-5-mini"],
        related: &["chat_prompt", "doctor"],
    },
    HelpTopic {
        name: "chat_prompt",
        category: "ai",
        summary: "Set the AI assistant system prompt for the current shell session.",
        usage: "chat_prompt <prompt-text>",
        examples: &["chat_prompt You are concise."],
        related: &["chat_model"],
    },
    HelpTopic {
        name: "mcp",
        category: "ai",
        summary: "Manage configured MCP servers and tools.",
        usage: "mcp <status|connect|disconnect|list|tools> [label]",
        examples: &["mcp status", "mcp tools", "mcp connect local-dev-tools"],
        related: &["doctor", "chat_model"],
    },
    HelpTopic {
        name: "snippet",
        category: "productivity",
        summary: "Save, edit, and run reusable command snippets.",
        usage: "snippet <add|remove|list|run|edit> [arguments]",
        examples: &[
            "snippet add test \"cargo test\"",
            "snippet run test",
            "snippet list",
        ],
        related: &["bookmark", "abbr"],
    },
    HelpTopic {
        name: "bookmark",
        category: "productivity",
        summary: "Bookmark frequently used commands for quick reuse.",
        usage: "bookmark <add|remove|list|run> [arguments]",
        examples: &[
            "bookmark add deploy",
            "bookmark run deploy",
            "bookmark list",
        ],
        related: &["snippet", "history"],
    },
];

/// Built-in help command implementation.
pub fn command(ctx: &Context, argv: Vec<String>, _proxy: &mut dyn ShellProxy) -> ExitStatus {
    let output = match help_output(&argv[1..]) {
        Ok(output) => output,
        Err(err) => {
            let _ = ctx.write_stderr(&format!("help: {err}"));
            return ExitStatus::ExitedWith(1);
        }
    };

    match ctx.write_stdout(&output) {
        Ok(_) => ExitStatus::ExitedWith(0),
        Err(err) => {
            ctx.write_stderr(&format!("help: failed to display help: {err}"))
                .ok();
            ExitStatus::ExitedWith(1)
        }
    }
}

fn help_output(args: &[String]) -> Result<String, String> {
    match args {
        [] => Ok(command_list_output()),
        [flag] if flag == "-h" || flag == "--help" => Ok(help_usage().to_string()),
        [flag, query] if flag == "-s" || flag == "--search" => Ok(search_output(query)),
        [flag] if flag == "-s" || flag == "--search" => {
            Err("--search requires a keyword".to_string())
        }
        [query] => {
            if should_search_category(query) {
                Ok(search_output(query))
            } else if let Some(topic) = find_topic(query) {
                Ok(topic_output(topic))
            } else {
                Ok(search_output(query))
            }
        }
        _ => Err("Usage: help [command|category] or help --search <keyword>".to_string()),
    }
}

fn command_list_output() -> String {
    let mut commands = visible_commands();
    commands.sort_by(|a, b| a.0.cmp(b.0));

    let mut output = String::from("Built-in commands:\n\n");
    for (cmd, description) in commands {
        output.push_str(&format!("{:<16} {}\n", cmd, description));
    }
    output.push_str("\nUse `help <command>` for details or `help --search <keyword>` to search.\n");
    output
}

fn topic_output(topic: HelpTopic) -> String {
    let mut output = String::new();
    output.push_str(&format!("{}\n", topic.name));
    output.push_str(&format!("Category: {}\n", topic.category));
    output.push_str(&format!("Summary: {}\n", topic.summary));
    output.push_str(&format!("Usage: {}\n", topic.usage));

    if !topic.examples.is_empty() {
        output.push_str("\nExamples:\n");
        for example in topic.examples {
            output.push_str(&format!("  {example}\n"));
        }
    }

    if !topic.related.is_empty() {
        output.push_str("\nRelated:\n");
        output.push_str(&format!("  {}\n", topic.related.join(", ")));
    }

    output
}

fn search_output(query: &str) -> String {
    let query = query.trim();
    if query.is_empty() {
        return help_usage().to_string();
    }

    let mut matches = Vec::new();
    for (name, description) in visible_commands() {
        let topic = find_topic(name);
        if command_matches(query, name, description, topic) {
            let category = topic.map(|topic| topic.category).unwrap_or("general");
            matches.push((name, category, description));
        }
    }

    matches.sort_by(|a, b| a.1.cmp(b.1).then(a.0.cmp(b.0)));

    if matches.is_empty() {
        return format!("No built-in commands matched `{query}`.");
    }

    let mut output = format!("Matches for `{query}`:\n\n");
    for (name, category, description) in matches {
        output.push_str(&format!("{:<16} {:<12} {}\n", name, category, description));
    }
    output.push_str("\nUse `help <command>` for details.\n");
    output
}

fn command_matches(query: &str, name: &str, description: &str, topic: Option<HelpTopic>) -> bool {
    let query = query.to_ascii_lowercase();
    let mut haystacks = vec![name.to_string(), description.to_string()];
    if let Some(topic) = topic {
        haystacks.push(topic.category.to_string());
        haystacks.push(topic.summary.to_string());
        haystacks.push(topic.usage.to_string());
        haystacks.extend(topic.related.iter().map(|value| (*value).to_string()));
    }

    haystacks
        .iter()
        .any(|value| value.to_ascii_lowercase().contains(&query))
}

fn find_topic(name: &str) -> Option<HelpTopic> {
    HELP_TOPICS.iter().copied().find(|topic| topic.name == name)
}

fn is_category(name: &str) -> bool {
    HELP_TOPICS.iter().any(|topic| topic.category == name)
}

fn should_search_category(name: &str) -> bool {
    (find_topic(name).is_none() && is_category(name)) || name == "project"
}

fn visible_commands() -> Vec<(&'static str, &'static str)> {
    get_all_commands()
        .into_iter()
        .filter(|(name, _)| !name.starts_with("__"))
        .collect()
}

fn help_usage() -> &'static str {
    concat!(
        "Usage: help [command|category]\n",
        "       help --search <keyword>\n",
        "\n",
        "Examples:\n",
        "  help doctor\n",
        "  help project\n",
        "  help --search ai\n",
    )
}

#[cfg(test)]
mod tests {
    use super::{get_all_commands, help_output};

    #[test]
    fn help_registry_includes_updated_history_and_doctor_entries() {
        let commands = get_all_commands();
        assert!(commands.iter().any(|(name, desc)| {
            *name == "history" && *desc == "Search and filter command history"
        }));
        assert!(commands.iter().any(|(name, desc)| {
            *name == "doctor"
                && *desc
                    == "Diagnose config, AI, MCP, project, runtime, skills, setup, and dev validation state"
        }));
    }

    #[test]
    fn help_command_shows_detailed_topic() {
        let output = help_output(&["doctor".to_string()]).unwrap();
        assert!(output.contains("doctor setup"));
        assert!(output.contains("doctor fix"));
    }

    #[test]
    fn help_search_matches_categories() {
        let output = help_output(&["--search".to_string(), "ai".to_string()]).unwrap();
        assert!(output.contains("safe-run"));
        assert!(output.contains("comp-gen"));

        let output = help_output(&["project".to_string()]).unwrap();
        assert!(output.starts_with("Matches for `project`:"));
        assert!(output.contains("pm"));
        assert!(output.contains("task"));
    }
}
