use super::ShellProxy;
use dsh_chatgpt::ChatGptClient;
use dsh_types::{Context, ExitStatus};
use termimad::crossterm::style::Color;
use termimad::MadSkin;

const PROMPT_KEY: &str = "CHAT_PROMPT";

pub fn chat(_ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    if let Some(key) = proxy.get_var("OPEN_AI_API_KEY") {
        match ChatGptClient::new(key) {
            Ok(client) => {
                let content = &argv[1];
                let prompt = proxy.get_var(PROMPT_KEY);
                match client.send_message(content, prompt, Some(0.1)) {
                    Ok(res) => {
                        let mut skin = MadSkin::default();
                        skin.bold.set_fg(Color::Yellow);
                        skin.italic.set_fg(Color::Magenta);
                        skin.code_block.set_fg(Color::White);
                        skin.inline_code.set_fg(Color::Yellow);
                        skin.print_text(res.trim());
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
        proxy.set_var(PROMPT_KEY.to_string(), prompt.to_string());
    }
    ExitStatus::ExitedWith(0)
}
