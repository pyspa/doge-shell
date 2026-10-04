//! User-triggered official SIWC authentication; never prints authorization URLs or tokens.
use crate::{ShellProxy, chatgpt::load_openai_config};
use anyhow::{Result, anyhow, bail};
use dsh_openai::auth::{self, AuthStore, LoginAttempt};
use dsh_types::{Context, ExitStatus};
use std::process::Stdio;
use std::time::{Duration, Instant};

pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let result = execute(ctx, &argv, proxy);
    match result {
        Ok(()) => ExitStatus::ExitedWith(0),
        Err(error) => {
            ctx.write_stderr(&format!("chat_auth: {error}")).ok();
            ExitStatus::ExitedWith(1)
        }
    }
}
fn execute(ctx: &Context, argv: &[String], proxy: &mut dyn ShellProxy) -> Result<()> {
    let config = load_openai_config(proxy);
    config.validate()?;
    let store = AuthStore::new(config.auth_dir()?.to_path_buf());
    match argv.get(1).map(String::as_str) {
        Some("login") => {
            if argv.len() > 3 || argv.get(2).is_some_and(|v| v != "--new") {
                bail!("Usage: chat_auth login [--new]");
            }
            let attempt = auth::run(LoginAttempt::start(store, argv.get(2).is_some(), None))?;
            ctx.write_stdout(
                "Continue with ChatGPT in your system browser. Ctrl+C cancels sign-in.",
            )?;
            open_browser(proxy, attempt.authorization_url())?;
            let account = auth::run(attempt.finish(None))?;
            ctx.write_stdout(&format!(
                "Signed in. Account label: {}. Use chat_auth models, then chat_model <slug>.",
                account.label
            ))?;
            refresh(proxy);
        }
        Some("status") => {
            let active = store.status();
            for account in store.accounts()? {
                let marker = if active.as_ref().is_ok_and(|a| a.label == account.label) {
                    "active"
                } else {
                    "saved"
                };
                ctx.write_stdout(&format!(
                    "{marker} {} (registration {})",
                    account.label, account.client_id
                ))?;
            }
            if let Err(error) = active {
                ctx.write_stdout(&format!("Not ready: {error}"))?;
            }
        }
        Some("models") => {
            let client = dsh_openai::ChatGptClient::try_from_config(&config)?;
            for model in client.subscription_models(None)? {
                ctx.write_stdout(&format!(
                    "{}\t{}",
                    model["slug"].as_str().unwrap_or_default(),
                    model["display_name"].as_str().unwrap_or_default()
                ))?;
            }
            ctx.write_stdout("Catalog listing does not guarantee model entitlement. Select a slug with chat_model <slug>.")?;
        }
        Some("logout") => {
            let result = auth::run(store.logout(None));
            refresh(proxy);
            result?;
            ctx.write_stdout("Signed out; remote revocation confirmed. Registration retained.")?;
        }
        Some("account") => {
            let label = argv
                .get(2)
                .ok_or_else(|| anyhow!("Usage: chat_auth account <label>"))?;
            store.select(label)?;
            refresh(proxy);
            ctx.write_stdout("Selected saved account. Run chat_reset before continuing an existing conversation.")?;
        }
        _ => bail!("Usage: chat_auth login [--new] | status | models | logout | account <label>"),
    }
    Ok(())
}
fn refresh(proxy: &mut dyn ShellProxy) {
    let provider = proxy
        .get_var(dsh_openai::PROVIDER_ENV)
        .unwrap_or_else(|| "api_key".into());
    proxy.set_var(dsh_openai::PROVIDER_ENV.to_owned(), provider);
}
fn open_browser(proxy: &mut dyn ShellProxy, url: &str) -> Result<()> {
    #[cfg(target_os = "linux")]
    let program = "xdg-open";
    #[cfg(target_os = "macos")]
    let program = "open";
    let mut child = crate::runtime_spawn::runtime_command(proxy, program)?
        .arg(url).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
        .spawn().map_err(|_| anyhow!("Could not open the system browser; install or configure the browser opener and retry."))?;
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                bail!("System browser opener failed; retry chat_auth login.");
            }
            return Ok(());
        }
        if start.elapsed() >= Duration::from_secs(10) {
            child.kill().ok();
            child.wait().ok();
            bail!("System browser opener timed out; retry chat_auth login.");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}
