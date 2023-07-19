use super::ShellProxy;
use dsh_chatgpt::ChatGptClient;
use dsh_types::{Context, ExitStatus};

const PROMPT_KEY: &str = "CHAT_PROMPT";

pub fn chat(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    if let Some(key) = proxy.get_var("OPEN_AI_API_KEY") {
        match ChatGptClient::new(key) {
            Ok(client) => {
                let content = &argv[1];
                let prompt = proxy.get_var(PROMPT_KEY);
                match client.send_message(content, prompt, Some(0.1)) {
                    Ok(res) => {
                        ctx.write_stdout(res.trim()).ok();
                        ExitStatus::ExitedWith(0)
                    }
                    Err(err) => {
                        ctx.write_stderr(&format!("\r{:?}", err)).ok();
                        ExitStatus::ExitedWith(1)
                    }
                }
            }
            Err(err) => {
                ctx.write_stderr(&format!("\r{:?}", err)).ok();
                ExitStatus::ExitedWith(1)
            }
        }
    } else {
        ctx.write_stderr("OPEN_AI_API_KEY not found").ok();
        ExitStatus::ExitedWith(1)
    }
}

pub fn chat_prompt(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    if argv.len() < 2 {
        ctx.write_stderr("chat_prompt variable").ok();
    } else {
        let prompt = &argv[1];
        proxy.set_var(PROMPT_KEY.to_string(), prompt.to_string());
    }
    ExitStatus::ExitedWith(0)
}
