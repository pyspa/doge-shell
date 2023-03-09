use super::ShellProxy;
use dsh_chatgpt::ChatGptClient;
use dsh_types::{Context, ExitStatus};

const PROMPT_KEY: &str = "CHAT_PROMPT";

pub fn chat(_ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    if let Some(key) = proxy.get_var("OPEN_AI_API_KEY") {
        match ChatGptClient::new(key) {
            Ok(client) => {
                let content = &argv[1];
                let prompt = proxy.get_var(PROMPT_KEY);
                match client.send_message(content, prompt, None) {
                    Ok(res) => {
                        println!("\r{}", res.trim());
                    }
                    Err(err) => {
                        eprintln!("\r{:?}", err);
                    }
                }
            }
            Err(err) => {
                eprintln!("\r{:?}", err)
            }
        }
    } else {
        eprintln!("OPEN_AI_API_KEY not found");
    }
    ExitStatus::ExitedWith(0)
}

pub fn chat_prompt(_ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    if argv.len() < 2 {
        println!("chat_prompt variable");
    } else {
        let prompt = &argv[1];
        proxy.save_var(PROMPT_KEY.to_string(), prompt.to_string());
    }
    ExitStatus::ExitedWith(0)
}
