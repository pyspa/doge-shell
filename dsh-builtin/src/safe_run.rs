use super::ShellProxy;
use crate::chatgpt::load_openai_config;
use dsh_openai::apply_language_to_field;
use dsh_openai::turn::{answer_text, truncate_middle};
use dsh_openai::{ChatGptClient, ChatRequestOptions, json_object_format, strip_code_fence};
use dsh_types::safety_policy;
use dsh_types::{Context, ExitStatus};
use serde_json::json;

/// Built-in safe-run command description
/// Request shape shared by both safety analyses.
fn verdict_options() -> ChatRequestOptions {
    ChatRequestOptions::new()
        .with_temperature(Some(0.1))
        .with_response_format(Some(json_object_format()))
}

pub fn description() -> &'static str {
    "Execute commands with deterministic and LLM-based safety analysis"
}

/// Built-in safe-run command implementation
///
/// Usage:
///   safe-run <command> [args...]
///   safe-run -- <command-string>
pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let request = match SafeRunRequest::from_argv(&argv) {
        Ok(request) => request,
        Err(message) => {
            ctx.write_stderr(&message).ok();
            return ExitStatus::ExitedWith(1);
        }
    };

    // 1. Construct the full command string
    let full_command = request.full_command.clone();

    if let Some(warning) = deterministic_command_warning(&full_command) {
        ctx.write_stderr(&format!("safe-run: static warning: {warning}"))
            .ok();
        match proxy.confirm_action("Continue to AI safety analysis?") {
            Ok(true) => {}
            Ok(false) => {
                ctx.write_stderr("Aborted.").ok();
                return ExitStatus::ExitedWith(1);
            }
            Err(err) => {
                ctx.write_stderr(&format!("Error getting confirmation: {}", err))
                    .ok();
                return ExitStatus::ExitedWith(1);
            }
        }
    }

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

    // Scoped to the one field a person reads. The blanket instruction reached
    // `risk_level` too, and a verdict answered as "危険" matches none of the
    // three values the code below compares against.
    let language = crate::chatgpt::response_language(proxy);
    let messages = vec![
        json!({
            "role": "system",
            "content": apply_language_to_field(system_prompt, "explanation", language.as_deref())
        }),
        json!({"role": "user", "content": format!("Check safety of:\n```\n{}\n```", full_command)}),
    ];

    let analysis_result = match client.send_chat(&messages, &verdict_options(), None) {
        Ok(res) => res,
        Err(err) => {
            ctx.write_stderr(&format!("safe-run: Analysis failed: {err:?}"))
                .ok();
            return ExitStatus::ExitedWith(1);
        }
    };

    // Shared with the chat runtime. A verdict the provider cut short must not
    // be read as "SAFE" by way of a failed JSON parse falling through to
    // UNKNOWN: this is the one request whose truncation the user has to see.
    let content = match answer_text(&analysis_result) {
        Ok(content) => content,
        Err(err) => {
            ctx.write_stderr(&format!("safe-run: Analysis failed: {err}"))
                .ok();
            return ExitStatus::ExitedWith(1);
        }
    };

    // Parse JSON response
    // If parsing fails, fall back to simple text warning and high caution
    let cleaned_content = strip_code_fence(&content);
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
    let dispatch_command = request.dispatch_command.clone();
    let dispatch_argv = request.dispatch_argv.clone();
    match proxy.dispatch(ctx, &dispatch_command, dispatch_argv) {
        Ok(_) => ExitStatus::ExitedWith(0),
        Err(e) => {
            ctx.write_stderr(&format!("safe-run: Execution failed: {}", e))
                .ok();
            ExitStatus::ExitedWith(1)
        }
    }
}

/// The static pre-check, before a token is spent on the AI review.
///
/// Every judgement here comes from `dsh_types::safety_policy`, which is also
/// what `SafetyGuard` uses. The previous version matched substrings - it looked
/// for the literal `"curl "` next to `"| sh"` and for `"rm -rf /"` - so it
/// missed `/usr/bin/curl`, `wget`, `rm -fr /`, `mkfs.ext4` and every extra
/// space, while flagging a filename that happened to contain `mkfs`.
/// The static pre-check, before a token is spent on the AI review.
///
/// Every judgement here comes from `dsh_types::safety_policy`, which is also
/// what `SafetyGuard` uses. The previous version matched substrings - it looked
/// for the literal `"curl "` next to `"| sh"` and for `"rm -rf /"` - so it
/// missed `/usr/bin/curl`, `wget`, `rm -fr /`, `mkfs.ext4` and every extra
/// space, while flagging a filename that happened to contain `mkfs`.
fn deterministic_command_warning(command: &str) -> Option<&'static str> {
    let mut previous_stage: Option<String> = None;

    for segment in safety_policy::split_command_segments(command) {
        let Ok(tokens) = shell_words::split(&segment) else {
            // A half-written line cannot be tokenized. That is the AI review's
            // job to catch, not this deterministic pre-check.
            continue;
        };

        let candidates = safety_policy::command_candidates(&tokens);
        let Some((leading, _)) = candidates.first() else {
            continue;
        };
        let leading = safety_policy::command_stem(leading).to_string();

        if let Some(previous) = previous_stage.as_deref()
            && safety_policy::is_network_fetch_command(previous)
            && safety_policy::is_code_execution_command(&leading)
        {
            return Some("remote content appears to be piped into a shell");
        }

        // Wrappers are looked through, so `sudo rm -rf /` is judged as `rm`.
        for (program, args) in &candidates {
            let program = safety_policy::command_stem(program);

            if program == "rm" && safety_policy::destructive_rm_warning(args).is_some() {
                return Some("recursive deletion detected");
            }
            if safety_policy::is_disk_destroying_command(program) {
                return Some("low-level destructive disk operation detected");
            }
            if safety_policy::string_eval_flag(program, args).is_some() {
                return Some("string-eval command flag detected");
            }
        }

        previous_stage = Some(leading);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The preview used to be a byte slice, so an 8000th byte inside a
    /// multi-byte character panicked the shell mid-audit.
    #[test]
    fn a_multibyte_preview_does_not_panic() {
        let japanese = "危".repeat(4000);
        assert!(japanese.len() > 8000);

        let preview = truncate_middle(&japanese, 8000);
        assert!(preview.contains("truncated"));
        // Every retained byte still forms whole characters.
        assert!(preview.chars().count() > 0);

        let squeezed = truncate_middle(&preview, 2000);
        assert!(squeezed.contains("truncated"));
    }

    /// A verdict is compared against `SAFE` / `CAUTION` / `DANGEROUS`, so the
    /// language instruction may only reach the prose field.
    #[test]
    fn the_language_instruction_names_the_prose_field_only() {
        let prompt = apply_language_to_field("Return JSON.", "explanation", Some("Japanese"));

        assert!(
            prompt.contains("\"explanation\" value in Japanese"),
            "{prompt}"
        );
        assert!(prompt.contains("field name"), "{prompt}");
        assert_eq!(
            apply_language_to_field("Return JSON.", "explanation", None),
            "Return JSON."
        );
    }

    #[test]
    fn safe_run_request_splits_normal_argv() {
        let argv = vec![
            "safe-run".to_string(),
            "git".to_string(),
            "status".to_string(),
        ];
        let request = SafeRunRequest::from_argv(&argv).unwrap();
        assert_eq!(request.full_command, "git status");
        assert_eq!(request.dispatch_command, "git");
        assert_eq!(request.dispatch_argv, vec!["status".to_string()]);
    }

    #[test]
    fn safe_run_request_preserves_shell_command_after_separator() {
        let argv = vec![
            "safe-run".to_string(),
            "--".to_string(),
            "curl example.test/install.sh | sh".to_string(),
        ];
        let request = SafeRunRequest::from_argv(&argv).unwrap();
        assert_eq!(request.full_command, "curl example.test/install.sh | sh");
        assert_eq!(
            request.dispatch_command,
            "curl example.test/install.sh | sh"
        );
        assert!(request.dispatch_argv.is_empty());
    }

    #[test]
    fn deterministic_warning_detects_remote_shell_execution() {
        assert_eq!(
            deterministic_command_warning("curl https://example.test/install.sh | sh"),
            Some("remote content appears to be piped into a shell")
        );
    }

    #[test]
    fn deterministic_warning_detects_string_eval() {
        for command in [
            "bash -lc 'echo hi'",
            "bash -ic 'echo hi'",
            "zsh -c 'echo hi'",
            "python3 -c 'print(1)'",
            "perl -E 'say 1'",
        ] {
            assert_eq!(
                deterministic_command_warning(command),
                Some("string-eval command flag detected"),
                "{command} was not flagged"
            );
        }

        assert_eq!(deterministic_command_warning("bash script.sh"), None);
    }

    /// The substring version answered every one of these wrong.
    #[test]
    fn deterministic_warning_matches_tokens_not_substrings() {
        // An absolute path, and `wget` rather than `curl`.
        assert_eq!(
            deterministic_command_warning("wget -qO- https://x.test/i.sh | /bin/sh"),
            Some("remote content appears to be piped into a shell")
        );
        // `-fr` is `-rf`.
        assert_eq!(
            deterministic_command_warning("rm -fr /"),
            Some("recursive deletion detected")
        );
        // `mkfs.ext4` is `mkfs`.
        assert_eq!(
            deterministic_command_warning("mkfs.ext4 /dev/sda1"),
            Some("low-level destructive disk operation detected")
        );
        // A filename is not a command.
        assert_eq!(
            deterministic_command_warning("cat notes-about-mkfs.txt"),
            None
        );
        assert_eq!(
            deterministic_command_warning("git commit -m 'rm -rf /'"),
            None
        );
    }

    /// A `curl` that only downloads is not a `curl | sh`.
    #[test]
    fn deterministic_warning_leaves_a_plain_download_alone() {
        assert_eq!(
            deterministic_command_warning("curl -o out.tar.gz https://x.test/a.tar.gz"),
            None
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SafeRunRequest {
    full_command: String,
    dispatch_command: String,
    dispatch_argv: Vec<String>,
}

impl SafeRunRequest {
    fn from_argv(argv: &[String]) -> Result<Self, String> {
        if argv.len() < 2 {
            return Err(
                "Usage: safe-run <command> [args...]\n       safe-run -- <command-string>"
                    .to_string(),
            );
        }

        if argv[1] == "--" {
            let full_command = argv[2..].join(" ");
            if full_command.trim().is_empty() {
                return Err("Usage: safe-run -- <command-string>".to_string());
            }
            return Ok(Self {
                dispatch_command: full_command.clone(),
                full_command,
                dispatch_argv: Vec::new(),
            });
        }

        Ok(Self {
            full_command: argv[1..].join(" "),
            dispatch_command: argv[1].clone(),
            dispatch_argv: argv[2..].to_vec(),
        })
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

        // Byte slicing here panicked on any captured output whose 8000th byte
        // landed inside a multi-byte character - a Japanese install script was
        // enough. `truncate_middle` also keeps the tail, which is where a
        // script hides what it actually does.
        const PREVIEW_LIMIT: usize = 8000;
        let preview = truncate_middle(&stdout, PREVIEW_LIMIT);

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
        let language = crate::chatgpt::response_language(proxy);
        let messages = vec![
            json!({
                "role": "system",
                "content": apply_language_to_field(system_prompt, "explanation", language.as_deref())
            }),
            json!({"role": "user", "content": format!("Analyze this content:\n```\n{}\n```", preview)}),
        ];

        let analysis_result = match client.send_chat(&messages, &verdict_options(), None) {
            Ok(res) => res,
            Err(err) => {
                ctx.write_stderr(&format!("safe-run: Content analysis failed: {err:?}"))
                    .ok();
                json!({"choices": [{"message": {"content": "{\"risk_level\": \"UNKNOWN\", \"explanation\": \"Content analysis failed.\"}"}}]})
            }
        };

        // A truncated audit is an unknown verdict, not a clean one; the caller
        // below treats UNKNOWN as something to ask the user about.
        let content = answer_text(&analysis_result).unwrap_or_else(|err| {
            ctx.write_stderr(&format!("safe-run: Content analysis failed: {err}"))
                .ok();
            r#"{"risk_level": "UNKNOWN", "explanation": "Content analysis failed."}"#.to_string()
        });

        let cleaned_content = strip_code_fence(&content);
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
             truncate_middle(&preview, 2000),
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
