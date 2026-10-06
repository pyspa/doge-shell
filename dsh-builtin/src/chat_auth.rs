//! User-triggered SIWC authentication; manual URLs omit private identity hints.
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
    let login = validate_args(argv)?;
    let config = load_openai_config(proxy);
    // Authentication management must remain usable when inference config is
    // invalid. Only catalog/inference validate the transport configuration.
    let store = AuthStore::new(config.auth_dir()?.to_path_buf());
    match argv.get(1).map(String::as_str) {
        Some("login") => {
            let login = login.expect("validated login arguments");
            let attempt = auth::run(LoginAttempt::start(
                store,
                login.new_account,
                Some(&|| proxy.is_canceled()),
            ))?;
            ctx.write_stdout(
                "Continue with ChatGPT in your system browser. Ctrl+C cancels sign-in.",
            )?;
            if let Some(reason) = manual_browser_reason(
                proxy,
                attempt.authorization_url(),
                login.no_browser,
                Duration::from_secs(10),
            )? {
                ctx.write_stdout(reason)?;
                ctx.write_stdout("Open this one-time URL in a browser on this machine; leave this login waiting. The 127.0.0.1 callback must reach this host. Sign-in expires after 5 minutes; Ctrl+C cancels.")?;
                ctx.write_stdout(&attempt.manual_authorization_url())?;
            }
            let account = auth::run(attempt.finish(Some(&|| proxy.is_canceled())))?;
            ctx.write_stdout(&format!(
                "Signed in. Account label: {}. Select chat_provider chatgpt_subscription, then use chat_auth models and chat_model <slug>.",
                account.label
            ))?;
            refresh(proxy);
        }
        Some("status") => {
            let active = store.status();
            let accounts = match store.accounts() {
                Ok(accounts) => accounts,
                Err(error) => {
                    ctx.write_stdout(&format!("Not ready: {error}"))?;
                    return Ok(());
                }
            };
            for account in accounts {
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
            config.validate()?;
            if config.provider() != dsh_openai::AiProvider::ChatGptSubscription {
                bail!(
                    "Select chat_provider chatgpt_subscription before listing subscription models."
                );
            }
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
        _ => unreachable!("validated authentication subcommand"),
    }
    Ok(())
}
#[derive(Default, Debug, PartialEq, Eq)]
struct LoginOptions {
    new_account: bool,
    no_browser: bool,
}

fn validate_args(argv: &[String]) -> Result<Option<LoginOptions>> {
    match argv.get(1).map(String::as_str) {
        Some("login") => {
            let mut options = LoginOptions::default();
            for arg in &argv[2..] {
                match arg.as_str() {
                    "--new" if !options.new_account => options.new_account = true,
                    "--no-browser" if !options.no_browser => options.no_browser = true,
                    _ => bail!("Usage: chat_auth login [--new] [--no-browser]"),
                }
            }
            Ok(Some(options))
        }
        Some("status" | "models" | "logout") if argv.len() == 2 => Ok(None),
        Some("account") if argv.len() == 3 && !argv[2].trim().is_empty() => Ok(None),
        _ => bail!(
            "Usage: chat_auth login [--new] [--no-browser] | status | models | logout | account <label>"
        ),
    }
}

// No opener failure aborts the already-bound callback listener. Diagnostics
// never interpolate URL or subprocess error details (which may contain it).
fn manual_browser_reason(
    proxy: &mut dyn ShellProxy,
    url: &str,
    no_browser: bool,
    timeout: Duration,
) -> Result<Option<&'static str>> {
    ensure_not_cancelled(&|| proxy.is_canceled())?;
    if no_browser {
        return Ok(Some("Manual browser sign-in requested."));
    }
    browser_result(open_browser(proxy, url, timeout))
}
fn browser_result(result: Result<()>) -> Result<Option<&'static str>> {
    match result {
        Ok(()) => Ok(None),
        Err(error) if error.is::<LoginCancelled>() => Err(error),
        Err(_) => Ok(Some(
            "Could not launch the system browser; continue sign-in manually.",
        )),
    }
}
#[derive(Debug)]
struct LoginCancelled;
impl std::fmt::Display for LoginCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Sign-in cancelled.")
    }
}
impl std::error::Error for LoginCancelled {}
fn ensure_not_cancelled(check: &dyn Fn() -> bool) -> Result<()> {
    if check() {
        return Err(LoginCancelled.into());
    }
    Ok(())
}
fn refresh(proxy: &mut dyn ShellProxy) {
    let provider = proxy
        .get_var(dsh_openai::PROVIDER_ENV)
        .unwrap_or_else(|| "api_key".into());
    proxy.set_var(dsh_openai::PROVIDER_ENV.to_owned(), provider);
}
fn open_browser(proxy: &mut dyn ShellProxy, url: &str, timeout: Duration) -> Result<()> {
    #[cfg(target_os = "linux")]
    let program = "xdg-open";
    #[cfg(target_os = "macos")]
    let program = "open";
    let mut child = crate::runtime_spawn::runtime_command(proxy, program)?
        .arg(url).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
        .spawn().map_err(|_| anyhow!("Could not open the system browser; install or configure the browser opener and retry."))?;
    wait_for_browser(&mut child, timeout, &|| proxy.is_canceled())
}
fn wait_for_browser(
    child: &mut std::process::Child,
    timeout: Duration,
    cancel: &dyn Fn() -> bool,
) -> Result<()> {
    let start = Instant::now();
    loop {
        if let Err(error) = ensure_not_cancelled(cancel) {
            child.kill().ok();
            child.wait().ok();
            return Err(error);
        }
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                bail!("System browser opener failed; retry chat_auth login.");
            }
            return Ok(());
        }
        if start.elapsed() >= timeout {
            child.kill().ok();
            child.wait().ok();
            bail!("System browser opener timed out; retry chat_auth login.");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(test)]
mod tests;
