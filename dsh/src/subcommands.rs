//! Handlers for `dsh`'s clap subcommands (`import`, `completion`), run
//! instead of the normal shell startup path.
use std::process::ExitCode;
use std::sync::Arc;

pub async fn handle_completion_command(
    command: String,
    output: Option<String>,
    force: bool,
) -> ExitCode {
    use crate::ai_features::generate_completion_json;
    use crate::environment::Environment;
    use dsh_builtin::completion_generation::CompletionGenerationService;
    use dsh_openai::{ChatGptClient, OpenAiConfig};
    use std::path::PathBuf;
    use tracing::{debug, error, info};

    info!("Generating completion for command: {}", command);

    let help_text = {
        let env = Environment::new();
        let guard = env.read();
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        // Fresh startup environment (already imported from the process):
        // `man` and the target resolve through it, never the ambient PATH.
        let snapshot = guard.command_runtime_snapshot(cwd);
        match CompletionGenerationService::collect_help_text(&snapshot, &command) {
            Ok(help_text) => help_text,
            Err(e) => {
                error!("Failed to collect help for '{}': {:#}", command, e);
                eprintln!("Error: Failed to get help text for '{}': {}", command, e);
                return ExitCode::FAILURE;
            }
        }
    };

    debug!("Got help text ({} chars)", help_text.len());

    // Initialize AI service
    let env = Environment::new();
    let config = OpenAiConfig::from_getter(|key| {
        let guard = env.read();
        guard.get_var(key)
    });

    let _api_key = match config.api_key() {
        Some(key) => key,
        None => {
            error!("OpenAI-compatible API key is not configured");
            eprintln!(
                "Error: OpenAI-compatible API key is not configured. {}",
                dsh_openai::API_KEY_SETUP_HINT
            );
            return ExitCode::FAILURE;
        }
    };

    let client = match ChatGptClient::try_from_config(&config) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to create AI client: {}", e);
            eprintln!("Error: Failed to create AI client: {}", e);
            return ExitCode::FAILURE;
        }
    };

    let mcp_manager = env.read().integration_state.mcp_manager.clone();
    let safety_level = env.read().policy_state.safety_level.clone();
    let policy = crate::ai_features::AgentPolicyHandles {
        safety_level,
        safety_guard: Arc::new(crate::safety::SafetyGuard::new()),
        execute_allowlist: env.read().policy_state.execute_allowlist.clone(),
        agent_session_allowlist: env.read().policy_state.agent_session_allowlist.clone(),
    };

    let response_language = env.read().integration_state.response_language.clone();
    let chat_model = env.read().integration_state.chat_model.clone();
    let service = crate::ai_features::LiveAiService::new(
        client,
        mcp_manager,
        policy,
        None,
        response_language,
        chat_model,
    );

    // Generate completion JSON using AI
    let completion_json = match generate_completion_json(&service, &command, &help_text).await {
        Ok(json) => json,
        Err(e) => {
            error!("Failed to generate completion JSON: {}", e);
            eprintln!("Error: Failed to generate completion: {}", e);
            return ExitCode::FAILURE;
        }
    };

    if let Err(e) = CompletionGenerationService::validate_json(&completion_json, &command) {
        error!("Generated completion failed validation: {:#}", e);
        eprintln!("Error: Generated completion failed validation: {e:#}");
        return ExitCode::FAILURE;
    }

    let output_path = match output {
        Some(path) => PathBuf::from(path),
        None => match CompletionGenerationService::default_output_path(&command) {
            Ok(path) => path,
            Err(e) => {
                error!("Failed to resolve completion output path: {:#}", e);
                eprintln!("Error: Failed to resolve completion output path: {e:#}");
                return ExitCode::FAILURE;
            }
        },
    };

    match CompletionGenerationService::write_json_atomic(
        &output_path,
        &completion_json,
        &command,
        force,
    ) {
        Ok(()) => {}
        Err(e) => {
            error!("Failed to write completion file: {:#}", e);
            eprintln!("Error: Failed to write completion file: {e:#}");
            return ExitCode::FAILURE;
        }
    }

    info!("Completion written to: {}", output_path.display());
    println!(
        "Completion generated and saved to: {}",
        output_path.display()
    );
    ExitCode::SUCCESS
}

pub fn handle_import_command(shell_name: &str, custom_path: Option<&str>) -> ExitCode {
    use crate::history::History;
    use crate::history_import::create_importer;
    use tracing::{debug, error, info};

    debug!("Starting history import from {shell_name} shell");
    println!("Importing history from {shell_name} shell...");

    // Create a history importer for the specified shell
    let importer = match create_importer(shell_name, custom_path) {
        Ok(importer) => importer,
        Err(err) => {
            error!("Failed to create importer for {shell_name} shell: {err}");
            eprintln!("Error creating importer: {err}");
            return ExitCode::FAILURE;
        }
    };

    // Create or load the dsh command history
    let mut history = match History::from_file("dsh_cmd_history") {
        Ok(h) => h,
        Err(err) => {
            error!("Failed to open dsh command history database: {err}");
            eprintln!("Error opening dsh history database: {err}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(err) = history.load() {
        error!("Failed to load dsh command history: {err}");
        eprintln!("Error loading dsh history: {err}");
        return ExitCode::FAILURE;
    }

    // Import the history
    match importer.import(&mut history) {
        Ok(count) => {
            info!("Successfully imported {count} commands from {shell_name} shell");
            println!("Successfully imported {count} commands from {shell_name} shell.");
            ExitCode::SUCCESS
        }
        Err(err) => {
            error!("Failed to import history from {shell_name} shell: {err}");
            eprintln!("Error importing history: {err}");
            ExitCode::FAILURE
        }
    }
}
