use super::ShellProxy;
use crate::chatgpt::load_openai_config;
use crate::shell_capabilities::{AgentCommandVerdict, ApprovalDecision};
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

/// A risk verdict from an LLM safety review.
///
/// The JSON shape both the command-intention check and the captured-output
/// audit ask for; `recommend_inspection` is unused by the latter (its prompt
/// never asks for it), and defaults to `false` there.
struct Verdict {
    risk: String,
    explanation: String,
    recommend_inspection: bool,
}

/// Ask the model to review `subject` against `system_prompt` and parse the
/// verdict.
///
/// Shared between `command()`'s command-intention check and
/// `inspect_and_run()`'s captured-content audit: same request shape
/// (`verdict_options`), same `risk_level`/`explanation` JSON parse, same
/// fallback. A transport failure, a stalled/truncated answer, or malformed
/// JSON never silently reads as `SAFE` - each becomes an explicit `UNKNOWN`
/// verdict whose explanation names the failure, which the caller's confirm
/// prompt surfaces to the user before anything executes.
fn request_verdict(
    proxy: &mut dyn ShellProxy,
    client: &ChatGptClient,
    system_prompt: &str,
    subject: &str,
) -> Verdict {
    // Scoped to the one field a person reads. The blanket instruction reached
    // `risk_level` too, and a verdict answered as "危険" matches none of the
    // three values callers compare against.
    let language = crate::chatgpt::response_language(proxy);
    let messages = vec![
        json!({
            "role": "system",
            "content": apply_language_to_field(system_prompt, "explanation", language.as_deref())
        }),
        json!({"role": "user", "content": subject}),
    ];

    let content = match client.send_chat(&messages, &verdict_options(), None) {
        Ok(res) => match answer_text(&res) {
            Ok(content) => content,
            Err(err) => {
                return Verdict {
                    risk: "UNKNOWN".to_string(),
                    explanation: format!("Analysis failed: {err}"),
                    recommend_inspection: true,
                };
            }
        },
        Err(err) => {
            return Verdict {
                risk: "UNKNOWN".to_string(),
                explanation: format!("Analysis failed: {err:?}"),
                recommend_inspection: true,
            };
        }
    };

    let cleaned_content = strip_code_fence(&content);
    match serde_json::from_str::<serde_json::Value>(&cleaned_content) {
        Ok(json) => Verdict {
            risk: json
                .get("risk_level")
                .and_then(|s| s.as_str())
                .unwrap_or("UNKNOWN")
                .to_string(),
            explanation: json
                .get("explanation")
                .and_then(|s| s.as_str())
                .unwrap_or("No explanation provided")
                .to_string(),
            recommend_inspection: json
                .get("recommend_inspection")
                .and_then(|b| b.as_bool())
                .unwrap_or(false),
        },
        Err(_) => Verdict {
            risk: "UNKNOWN".to_string(),
            explanation: format!("Failed to parse AI response: {content}"),
            recommend_inspection: true,
        },
    }
}

/// Whether `full_command` may run, judged by the same `SafetyGuard` that a
/// typed command goes through - `Ok(true)` to proceed, `Ok(false)` if the
/// user (or the guard) refused.
///
/// Before this, everything past this point (`ShellProxy::dispatch` /
/// `capture_command`) ran the command directly with no `SafetyGuard`
/// involvement at all: `SAFETY_LEVEL=strict`, the configured allowlist, and
/// the substitution/compound-statement/unconsumed-tail refusals in
/// `evaluate_agent_command` were all silently skipped for anything spelled
/// `safe-run <cmd>` instead of typed directly. safe-run's own LLM verdict is
/// a *different* judgement (an AI opinion on intent) and does not replace
/// this one (a deterministic policy decision) - both apply, same as a
/// skill-script command already gets both an allowlist check and a guard
/// verdict in the chat `execute` tool.
fn confirm_with_safety_guard(
    ctx: &Context,
    proxy: &mut dyn ShellProxy,
    full_command: &str,
) -> bool {
    match proxy.evaluate_agent_command(full_command) {
        AgentCommandVerdict::Allowed => true,
        AgentCommandVerdict::Denied(reason) => {
            ctx.write_stderr(&format!("safe-run: refused by the safety guard: {reason}"))
                .ok();
            false
        }
        AgentCommandVerdict::Confirm(reason) => {
            match proxy.request_agent_approval(&format!("safe-run: {full_command}: {reason}")) {
                Ok(ApprovalDecision::Allow) => true,
                Ok(ApprovalDecision::AllowAlways) => {
                    proxy.remember_agent_approval(full_command);
                    true
                }
                Ok(ApprovalDecision::Deny) => {
                    ctx.write_stderr("Aborted.").ok();
                    false
                }
                Err(e) => {
                    ctx.write_stderr(&format!("Error getting confirmation: {}", e))
                        .ok();
                    false
                }
            }
        }
    }
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
        ctx.write_stderr(&format!(
            "safe-run: AI service is not configured. {}",
            dsh_openai::API_KEY_SETUP_HINT
        ))
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

    let verdict = request_verdict(
        proxy,
        &client,
        system_prompt,
        &format!("Check safety of:\n```\n{}\n```", full_command),
    );
    let risk = verdict.risk;
    let explanation = verdict.explanation;
    let recommend_inspection = verdict.recommend_inspection;

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
        "\n{bold}Safety Analysis:{reset}\nRate: {}{}{reset}\nExplanation: {}",
        risk_color, risk, explanation
    ))
    .ok();

    if recommend_inspection {
        ctx.write_stderr(&format!(
            "\n{}[!] Remote content execution detected or specific risk identified.{}",
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
    if !confirm_with_safety_guard(ctx, proxy, &full_command) {
        return ExitStatus::ExitedWith(1);
    }
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
    use crate::shell_capabilities::AgentCommandPolicy;
    use crate::test_support::TestShellProxy;

    fn ctx() -> Context {
        let pid = nix::unistd::getpid();
        Context::new_safe(pid, pid, false)
    }

    /// A command the guard denies outright (e.g. a compound statement or a
    /// substitution it cannot judge before it runs) must never reach
    /// `dispatch`/`capture_command`, no matter what safe-run's own LLM
    /// verdict said.
    #[test]
    fn a_denied_verdict_blocks_execution() {
        let ctx = ctx();
        let mut proxy = TestShellProxy {
            agent_verdict: AgentCommandVerdict::Denied("refused by policy".to_string()),
            ..TestShellProxy::default()
        };

        assert!(!confirm_with_safety_guard(&ctx, &mut proxy, "rm -rf ~"));
        // Denied is a hard refusal - the user is never even asked.
        assert_eq!(proxy.confirm_calls, 0);
    }

    /// A command the guard is unsure about goes through the same
    /// confirm-with-Always flow as a directly typed command; declining stops
    /// execution.
    #[test]
    fn declining_a_guard_confirmation_blocks_execution() {
        let ctx = ctx();
        let mut proxy = TestShellProxy {
            agent_verdict: AgentCommandVerdict::Confirm("looks risky".to_string()),
            confirm_result: false,
            ..TestShellProxy::default()
        };

        assert!(!confirm_with_safety_guard(&ctx, &mut proxy, "rm -rf ~"));
        assert_eq!(proxy.confirm_calls, 1);
    }

    /// Accepting a guard confirmation with "always" both proceeds and
    /// remembers the command for the rest of the session, same as a typed
    /// command would.
    #[test]
    fn always_approving_a_guard_confirmation_proceeds_and_remembers() {
        let ctx = ctx();
        let mut proxy = TestShellProxy {
            agent_verdict: AgentCommandVerdict::Confirm("looks risky".to_string()),
            approval_decision: Some(ApprovalDecision::AllowAlways),
            ..TestShellProxy::default()
        };

        assert!(confirm_with_safety_guard(&ctx, &mut proxy, "rm -rf /tmp/x"));
        assert_eq!(proxy.agent_session_approvals(), vec!["rm -rf /tmp/x"]);
    }

    /// A command the guard is happy with proceeds without ever asking.
    #[test]
    fn an_allowed_verdict_proceeds_without_asking() {
        let ctx = ctx();
        let mut proxy = TestShellProxy {
            agent_verdict: AgentCommandVerdict::Allowed,
            ..TestShellProxy::default()
        };

        assert!(confirm_with_safety_guard(&ctx, &mut proxy, "echo hi"));
        assert_eq!(proxy.confirm_calls, 0);
    }

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

    // `capture_command` actually runs `full_command` (via `sh -c`) to collect
    // its output - the same real execution as the dispatch below, just with
    // stdout held back for review. It needs the same gate.
    if !confirm_with_safety_guard(ctx, proxy, full_command) {
        return ExitStatus::ExitedWith(1);
    }

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
        ctx.write_stderr(&format!("\n--- STDERR ---\n{}", stderr))
            .ok();
    }

    if stdout.is_empty() {
        ctx.write_stderr(&format!("\n{yellow}--- No STDOUT captured ---{reset}"))
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
                 "\n{yellow}[!] Static Analysis Warning: Potential dangerous patterns detected in content:{reset}",
                 yellow=yellow, reset=reset
             )).ok();
            for warn in &static_warnings {
                ctx.write_stderr(&format!(" - {}", warn)).ok();
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
        let verdict = request_verdict(
            proxy,
            client,
            system_prompt,
            &format!("Analyze this content:\n```\n{}\n```", preview),
        );
        let risk = verdict.risk;
        let explanation = verdict.explanation;

        let risk_color = match risk.to_uppercase().as_str() {
            "SAFE" => green,
            "DANGEROUS" => red,
            "CAUTION" => yellow,
            _ => reset,
        };

        ctx.write_stderr(&format!(
            "\n{cyan}--- Content Preview ({} chars) ---{reset}\n{}\n{cyan}--- End Preview ---{reset}",
             preview.len(),
             truncate_middle(&preview, 2000),
             cyan=cyan, reset=reset
        )).ok();

        ctx.write_stderr(&format!(
            "\n{bold}Content Analysis:{reset}\nRate: {}{}{reset}\nExplanation: {}",
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
                    // Through `ctx`, not the real stdout directly: a raw
                    // `print!` here skipped the output observer and any
                    // redirect/pipe target, so a captured, approved script's
                    // output never reached `out`/`tm`, `OutputHistory`, or
                    // `safe-run ... > file`. `write_stdout` appends its own
                    // `\n`, so trim what `sh -c` already left on the end.
                    ctx.write_stdout(stdout.trim_end_matches('\n')).ok();
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
