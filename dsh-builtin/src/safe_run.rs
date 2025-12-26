use super::ShellProxy;
use crate::chatgpt::load_openai_config;
use dsh_openai::ChatGptClient;
use dsh_types::{Context, ExitStatus};
use serde_json::json;

/// Built-in safe-run command description
pub fn description() -> &'static str {
    "Execute commands with LLM-based safety analysis"
}

/// Built-in safe-run command implementation
///
/// Usage:
///   safe-run <command> [args...]
pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    if argv.len() < 2 {
        ctx.write_stderr("Usage: safe-run <command> [args...]").ok();
        return ExitStatus::ExitedWith(1);
    }

    // 1. Construct the full command string
    let cmd_args = &argv[1..];
    let full_command = cmd_args.join(" ");

    // 2. Initialize LLM client
    let config = load_openai_config(proxy);
    if config.api_key().is_none() {
        ctx.write_stderr("safe-run: AI_CHAT_API_KEY not found. Cannot perform safety check.")
            .ok();
        return ExitStatus::ExitedWith(1);
    }

    let client = match ChatGptClient::try_from_config(&config) {
        Ok(client) => client,
        Err(err) => {
            ctx.write_stderr(&format!("safe-run: Failed to initialize AI client: {err}"))
                .ok();
            return ExitStatus::ExitedWith(1);
        }
    };

    // 3. Phase 1: Command Intention Check
    // Analyze command handling "curl | sh" patterns specifically
    ctx.write_stderr("Analyzing command safety...").ok();

    let system_prompt = r#"You are a security-conscious shell expert. Analyze the given command for potential risks.
Check for:
- Destructive operations (rm -rf, mkfs, etc.)
- Data loss risks
- Security vulnerabilities
- Remote script execution (e.g. executing fetched content mostly via pipes like `curl ... | sh`)

If the command involves fetching and executing remote content (like `curl | sh`), you MUST recommend Output Inspection.

Format your response as valid JSON:
{
  "risk_level": "SAFE" | "CAUTION" | "DANGEROUS",
  "explanation": "Concise explanation of the risk",
  "recommend_inspection": true | false
}
"#;

    let messages = vec![
        json!({"role": "system", "content": system_prompt}),
        json!({"role": "user", "content": format!("Check safety of:\n```\n{}\n```", full_command)}),
    ];

    let analysis_result = match client.send_chat_request(&messages, Some(0.1), None, None, None) {
        Ok(res) => res,
        Err(err) => {
            ctx.write_stderr(&format!("safe-run: Analysis failed: {err:?}"))
                .ok();
            return ExitStatus::ExitedWith(1);
        }
    };

    let content = analysis_result
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");

    // Parse JSON response
    // If parsing fails, fall back to simple text warning and high caution
    fn clean_json(s: &str) -> String {
        let s = s.trim();
        if s.starts_with("```json") {
            s.strip_prefix("```json")
                .unwrap_or(s)
                .strip_suffix("```")
                .unwrap_or(s)
                .trim()
                .to_string()
        } else if s.starts_with("```") {
            s.strip_prefix("```")
                .unwrap_or(s)
                .strip_suffix("```")
                .unwrap_or(s)
                .trim()
                .to_string()
        } else {
            s.to_string()
        }
    }

    let cleaned_content = clean_json(content);
    let (risk, explanation, recommend_inspection) =
        match serde_json::from_str::<serde_json::Value>(&cleaned_content) {
            Ok(json) => (
                json.get("risk_level")
                    .and_then(|s| s.as_str())
                    .unwrap_or("UNKNOWN")
                    .to_string(),
                json.get("explanation")
                    .and_then(|s| s.as_str())
                    .unwrap_or("No explanation provided")
                    .to_string(),
                json.get("recommend_inspection")
                    .and_then(|b| b.as_bool())
                    .unwrap_or(false),
            ),
            Err(_) => (
                "UNKNOWN".to_string(),
                format!("Failed to parse AI response: {}", content),
                true, // Default to inspection on error
            ),
        };

    // Styling helpers
    let bold = "\x1b[1m";
    let reset = "\x1b[0m";
    let green = "\x1b[32m";
    let red = "\x1b[31m";
    let yellow = "\x1b[33m";
    let cyan = "\x1b[36m";

    let risk_color = match risk.to_uppercase().as_str() {
        "SAFE" => green,
        "DANGEROUS" => red,
        "CAUTION" => yellow,
        _ => reset,
    };

    ctx.write_stderr(&format!(
        "\n{bold}Safety Analysis:{reset}\nRate: {}{}{reset}\nExplanation: {}\n",
        risk_color, risk, explanation
    ))
    .ok();

    if recommend_inspection {
        ctx.write_stderr(&format!(
            "\n{}[!] Remote content execution detected or specific risk identified.{}\n",
            yellow, reset
        ))
        .ok();
        match proxy.confirm_action(&format!(
            "Do you want to {}INSPECT{} the output (content) before execution?",
            cyan, reset
        )) {
            Ok(true) => {
                // Proceed to Phase 2: Output Inspection
                return inspect_and_run(ctx, proxy, &client, &full_command);
            }
            Ok(false) => {
                // User declined inspection. Ask for immediate execution.
                match proxy.confirm_action(&format!(
                    "Execute {}IMMEDIATELY{} without inspection?",
                    red, reset
                )) {
                    Ok(true) => {
                        // Fall out to dispatch below
                    }
                    Ok(false) => {
                        ctx.write_stderr("Aborted.").ok();
                        return ExitStatus::ExitedWith(1);
                    }
                    Err(e) => {
                        ctx.write_stderr(&format!("Error getting confirmation: {}", e))
                            .ok();
                        return ExitStatus::ExitedWith(1);
                    }
                }
            }
            Err(e) => {
                ctx.write_stderr(&format!("Error getting confirmation: {}", e))
                    .ok();
                return ExitStatus::ExitedWith(1);
            }
        }
    } else {
        // Even if SAFE, asking for confirmation to let user read the analysis
        let confirmation_msg = if risk != "SAFE" {
            format!(
                "Risk detected: {}. {}Execute anyway?{}",
                explanation, red, reset
            )
        } else {
            format!("{}Execute?{}", green, reset)
        };

        match proxy.confirm_action(&confirmation_msg) {
            Ok(true) => {
                // Fall out to dispatch below
            }
            Ok(false) => {
                ctx.write_stderr("Aborted.").ok();
                return ExitStatus::ExitedWith(1);
            }
            Err(e) => {
                ctx.write_stderr(&format!("Error getting confirmation: {}", e))
                    .ok();
                return ExitStatus::ExitedWith(1);
            }
        }
    }

    // 4. Execution (if approved)
    match proxy.dispatch(ctx, &argv[1], argv[2..].to_vec()) {
        Ok(_) => ExitStatus::ExitedWith(0),
        Err(e) => {
            ctx.write_stderr(&format!("safe-run: Execution failed: {}", e))
                .ok();
            ExitStatus::ExitedWith(1)
        }
    }
}

fn inspect_and_run(
    ctx: &Context,
    proxy: &mut dyn ShellProxy,
    client: &ChatGptClient,
    full_command: &str,
) -> ExitStatus {
    let bold = "\x1b[1m";
    let reset = "\x1b[0m";
    let green = "\x1b[32m";
    let red = "\x1b[31m";
    let yellow = "\x1b[33m";
    let cyan = "\x1b[36m";

    ctx.write_stderr("Capturing output for inspection...").ok();

    // Capture the output
    let (exit_code, stdout, stderr) = match proxy.capture_command(ctx, full_command) {
        Ok(res) => res,
        Err(e) => {
            ctx.write_stderr(&format!("safe-run: Failed to capture output: {}", e))
                .ok();
            return ExitStatus::ExitedWith(1);
        }
    };

    if !stderr.is_empty() {
        ctx.write_stderr(&format!("\n--- STDERR ---\n{}\n", stderr))
            .ok();
    }

    if stdout.is_empty() {
        ctx.write_stderr(&format!("\n{yellow}--- No STDOUT captured ---{reset}\n"))
            .ok();
        ExitStatus::ExitedWith(exit_code)
    } else {
        // Initialize robust static analysis
        let dangerous_patterns = [
            ("rm -rf", "Recursive deletion"),
            ("mkfs", "Filesystem formatting"),
            ("dd if=", "Low-level disk access"),
            (":(){ :|:& };:", "Fork bomb"),
            ("chmod 777", "Insecure permissions"),
            ("wget ", "Remote download"),
            ("curl ", "Remote download"),
            ("| sh", "Pipe to shell"),
            ("| bash", "Pipe to shell"),
            ("> /dev/sd", "Device overwriting"),
            ("> /dev/nvme", "Device overwriting"),
            ("mv /", "Root directory modification"),
        ];

        let mut static_warnings = Vec::new();
        for (pattern, desc) in dangerous_patterns.iter() {
            if stdout.contains(pattern) {
                static_warnings.push(format!("Found '{}' ({})", pattern, desc));
            }
        }

        let preview_limit = 8000;
        let preview = if stdout.len() > preview_limit {
            format!(
                "{}... (truncated, total length: {})",
                &stdout[..preview_limit],
                stdout.len()
            )
        } else {
            stdout.clone()
        };

        if !static_warnings.is_empty() {
            ctx.write_stderr(&format!(
                 "\n{yellow}[!] Static Analysis Warning: Potential dangerous patterns detected in content:{reset}\n",
                 yellow=yellow, reset=reset
             )).ok();
            for warn in &static_warnings {
                ctx.write_stderr(&format!(" - {}\n", warn)).ok();
            }
        }

        ctx.write_stderr("\nAnalyzing captured content...").ok();

        let system_prompt = r#"You are a code auditor. Analyze the following captured output (which might be a script intended for execution).
Check for malicious code, backdoors, or dangerous operations.
Format your response as valid JSON:
{
  "risk_level": "SAFE" | "CAUTION" | "DANGEROUS",
  "explanation": "Concise analysis of the content"
}
"#;
        let messages = vec![
            json!({"role": "system", "content": system_prompt}),
            json!({"role": "user", "content": format!("Analyze this content:\n```\n{}\n```", preview)}),
        ];

        let analysis_result = match client.send_chat_request(&messages, Some(0.1), None, None, None)
        {
            Ok(res) => res,
            Err(err) => {
                ctx.write_stderr(&format!("safe-run: Content analysis failed: {err:?}"))
                    .ok();
                json!({"choices": [{"message": {"content": "{\"risk_level\": \"UNKNOWN\", \"explanation\": \"Content analysis failed.\"}"}}]})
            }
        };

        let content = analysis_result
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .unwrap_or("");

        fn clean_json(s: &str) -> String {
            let s = s.trim();
            if s.starts_with("```json") {
                s.strip_prefix("```json")
                    .unwrap_or(s)
                    .strip_suffix("```")
                    .unwrap_or(s)
                    .trim()
                    .to_string()
            } else if s.starts_with("```") {
                s.strip_prefix("```")
                    .unwrap_or(s)
                    .strip_suffix("```")
                    .unwrap_or(s)
                    .trim()
                    .to_string()
            } else {
                s.to_string()
            }
        }

        let cleaned_content = clean_json(content);
        let (risk, explanation) = match serde_json::from_str::<serde_json::Value>(&cleaned_content)
        {
            Ok(json) => (
                json.get("risk_level")
                    .and_then(|s| s.as_str())
                    .unwrap_or("UNKNOWN")
                    .to_string(),
                json.get("explanation")
                    .and_then(|s| s.as_str())
                    .unwrap_or("No explanation")
                    .to_string(),
            ),
            Err(_) => (
                "UNKNOWN".to_string(),
                format!("Analysis failed: {}", content),
            ),
        };

        let risk_color = match risk.to_uppercase().as_str() {
            "SAFE" => green,
            "DANGEROUS" => red,
            "CAUTION" => yellow,
            _ => reset,
        };

        ctx.write_stderr(&format!(
            "\n{cyan}--- Content Preview ({} chars) ---{reset}\n{}\n{cyan}--- End Preview ---{reset}\n",
             preview.len(),
             if preview.len() > 2000 { format!("{}... (preview truncated to 2kb)", &preview[..2000]) } else { preview.clone() },
             cyan=cyan, reset=reset
        )).ok();

        ctx.write_stderr(&format!(
            "\n{bold}Content Analysis:{reset}\nRate: {}{}{reset}\nExplanation: {}\n",
            risk_color, risk, explanation
        ))
        .ok();

        let prompt_msg = if risk != "SAFE" {
            format!(
                "Content Risk: {}!!!!\nExecute {}release output to stdout{}?",
                risk, cyan, reset
            )
        } else {
            format!(
                "Content Risk: SAFE.\nExecute {}release output to stdout{}?",
                cyan, reset
            )
        };

        match proxy.confirm_action(&prompt_msg) {
            Ok(true) => {
                if !stdout.is_empty() {
                    print!("{}", stdout);
                    use std::io::Write;
                    std::io::stdout().flush().ok();
                }
                ExitStatus::ExitedWith(exit_code)
            }
            Ok(false) => {
                ctx.write_stderr("Aborted (output discarded).").ok();
                ExitStatus::ExitedWith(1)
            }
            Err(e) => {
                ctx.write_stderr(&format!("Error: {}", e)).ok();
                ExitStatus::ExitedWith(1)
            }
        }
    }
}
