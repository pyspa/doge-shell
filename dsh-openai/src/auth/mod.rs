//! Official Sign in with ChatGPT public-client OAuth. Tokens are never imported.
use anyhow::{Result, anyhow, bail};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
mod oauth;
mod store;
#[cfg(test)]
pub(crate) mod tests;
pub use oauth::LoginAttempt;
use store::Credentials;
pub use store::{Account, AuthStore};
pub const AUTH_ISSUER: &str = "https://auth.openai.com";
pub const RESOURCE: &str = "https://api.openai.com/v1";
pub const TOKEN_ENDPOINT: &str = "https://auth.openai.com/api/accounts/oauth/token";
pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub(crate) fn http_client() -> Result<Client> {
    Ok(Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()?)
}
pub(crate) async fn wait<T>(
    future: impl Future<Output = Result<T>>,
    cancel: Option<&dyn Fn() -> bool>,
    timeout: Duration,
) -> Result<T> {
    tokio::pin!(future);
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if cancel.is_some_and(|check| check()) {
            return Err(crate::client::RequestCancelled.into());
        }
        tokio::select! {
            result = &mut future => return result,
            signal = &mut ctrl_c => {
                signal.map_err(|_| anyhow!("Cannot listen for cancellation."))?;
                return Err(crate::client::RequestCancelled.into());
            },
            _ = tokio::time::sleep(Duration::from_millis(50)) => {
                if tokio::time::Instant::now() >= deadline { bail!("ChatGPT request timed out."); }
            }
        }
    }
}
pub fn run<T>(future: impl Future<Output = Result<T>>) -> Result<T> {
    let runtime = crate::client::shared_runtime()?;
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::task::block_in_place(|| runtime.block_on(future))
    } else {
        runtime.block_on(future)
    }
}
// Server strings are untrusted and could contain credentials: expose only short identifiers.
pub(crate) fn identifier(value: Option<&str>) -> String {
    value
        .filter(|s| {
            s.len() <= 128
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        })
        .unwrap_or("unknown")
        .to_owned()
}
pub(crate) async fn checked_json(response: reqwest::Response) -> Result<serde_json::Value> {
    let status = response.status();
    let request_id = identifier(
        response
            .headers()
            .get("x-request-id")
            .and_then(|s| s.to_str().ok()),
    );
    let data: serde_json::Value = response
        .json()
        .await
        .map_err(|_| anyhow!("Invalid OpenAI response (HTTP {}).", status.as_u16()))?;
    if !status.is_success() {
        let code = identifier(
            data.pointer("/error/code")
                .and_then(|v| v.as_str())
                .or_else(|| data.get("error").and_then(|v| v.as_str())),
        );
        let param = identifier(data.pointer("/error/param").and_then(|v| v.as_str()));
        let hint = match code.as_str() {
            "usage_limit" | "usage_limit_reached" => {
                " ChatGPT plan limit reached. See https://chatgpt.com/settings/usage. No API-key fallback."
            }
            "user_not_eligible" => {
                " This account is not eligible; signing in repeatedly will not fix this."
            }
            "invalid_grant" | "invalid_token" => " Run chat_auth login to reauthenticate.",
            _ => "",
        };
        bail!(
            "OpenAI HTTP {} code={code} param={param} request_id={request_id}.{hint}",
            status.as_u16()
        );
    }
    Ok(data)
}
