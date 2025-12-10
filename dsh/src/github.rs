use crate::environment::Environment;
use crate::prompt::Prompt;
use anyhow::Result;
use parking_lot::RwLock;
use reqwest::{Client, Method};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, error};

#[derive(Debug, Clone, Default)]
pub struct GitHubStatus {
    pub notification_count: usize,
    pub has_error: bool,
}

#[derive(Debug, Clone, Default)]
pub struct GitHubConfig {
    pub pat: Option<String>,
    pub interval: u64,
    pub filter: Option<String>,
}

#[derive(Debug, Clone)]
pub struct GitHubClient {
    client: Client,
    pat: String,
}

impl GitHubClient {
    pub fn new(pat: String) -> Self {
        let client = Client::builder()
            .user_agent("doge-shell")
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_default();
        Self { client, pat }
    }

    pub async fn fetch_notifications(&self, filter: Option<&str>) -> Result<usize> {
        let url = "https://api.github.com/notifications";
        let request = self.client.request(Method::GET, url).bearer_auth(&self.pat);

        // We don't use query param filters effectively here because the API is limited.
        // We fetch unread notifications and filter client-side.

        let response = request.send().await?;

        if !response.status().is_success() {
            anyhow::bail!("GitHub API error: {}", response.status());
        }

        let notifications: Vec<serde_json::Value> = response.json().await?;

        if let Some(f) = filter
            && let Some(reason_val) = f.strip_prefix("reason:")
        {
            let allowed_reasons: Vec<&str> = reason_val.split(',').map(|s| s.trim()).collect();
            let filtered_count = notifications
                .iter()
                .filter(|n| {
                    n["reason"]
                        .as_str()
                        .is_some_and(|r| allowed_reasons.contains(&r))
                })
                .count();
            debug!(
                "Filtered notifications (filter: '{}'): {} / {}",
                f,
                filtered_count,
                notifications.len()
            );
            return Ok(filtered_count);
        }

        Ok(notifications.len())
    }
}

pub async fn background_github_task(
    config: Arc<RwLock<GitHubConfig>>,
    prompt: Arc<RwLock<Prompt>>,
    github_status: Arc<RwLock<GitHubStatus>>,
) {
    // Initial delay
    tokio::time::sleep(Duration::from_secs(2)).await;

    debug!("GitHub background task started");

    loop {
        // Read config to get interval
        let (pat, interval_secs, filter) = {
            let cfg = config.read();
            (cfg.pat.clone(), cfg.interval, cfg.filter.clone())
        };

        // Sleep for the configured interval
        let sleep_duration = if interval_secs > 0 { interval_secs } else { 60 };
        tokio::time::sleep(Duration::from_secs(sleep_duration)).await;

        // Check constraints
        let should_check = {
            let p = prompt.read();
            p.under_git()
        };

        if pat.is_none() {
            debug!("GitHub PAT not set, skipping check");
            continue;
        }

        if !should_check {
            debug!("Not in Git repository, skipping GitHub check");
            // Clear status if not in git dir? Or keep previous?
            // User said "Check ... only ... is fine".
            // Display also checks under_git.
            // If we don't check, maybe we shouldn't clear, just preserve old state?
            // But if we move out of git dir, prompt won't show it anyway.
            // If we move back, we might see old state until next tick.
            // Given 60s interval, "old state" might be stale.
            // Maybe we should trigger a check on directory change?
            // But `chpwd` is synchronous hook.
            // For now, simple polling is fine.
            continue;
        }

        // Update timer interval if needed (heuristic: if config changed significantly)
        // For simplicity, we just use the timer. If user changes interval, it applies next run.
        // But `tokio::time::interval` doesn't change period easily.
        // We could reconstruct `timer` if we really wanted dynamic interval,
        // but for now let's assume 60s or just sleep.
        // Actually, let's use `tokio::time::sleep` for dynamic interval support.

        if let Some(pat_str) = pat {
            let client = GitHubClient::new(pat_str);
            debug!("Checking GitHub notifications...");
            match client.fetch_notifications(filter.as_deref()).await {
                Ok(count) => {
                    let mut status = github_status.write();
                    status.notification_count = count;
                    status.has_error = false;
                    debug!("GitHub notifications: {}", count);
                }
                Err(e) => {
                    error!("Failed to fetch GitHub notifications: {}", e);
                    let mut status = github_status.write();
                    status.has_error = true;
                }
            }
        }

        // If using sleep loop instead of interval:
        // tokio::time::sleep(Duration::from_secs(interval_secs)).await;
        // But the `timer` above is `interval` which is robust.
        // Mixing them is bad. Let's just stick to the loop but maybe reset timer?
        // Let's just respect the loop tick for now (60s default).
        // If we want to support custom interval, we should change the loop structure.

        // Improved loop for dynamic interval:
        /*
        loop {
            let interval_secs = ... get from env ...
            tokio::time::sleep(Duration::from_secs(interval_secs)).await;
            ... work ...
        }
        */
        // But strict "1 minute" was requested ("1分ごとなど").
        // "config.lispで設定します" ("configure in config.lisp").
        // So dynamic interval IS a requirement.
    }
}

pub async fn background_github_task_dynamic(
    environment: Arc<RwLock<Environment>>,
    prompt: Arc<RwLock<Prompt>>,
    github_status: Arc<RwLock<GitHubStatus>>,
) {
    loop {
        // Read interval first
        let interval_secs = {
            let env = environment.read();
            env.get_var("*github-notify-interval*")
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(60)
        };

        tokio::time::sleep(Duration::from_secs(interval_secs)).await;

        // duplicate logic from above... helper function?
        check_github(environment.clone(), prompt.clone(), github_status.clone()).await;
    }
}

async fn check_github(
    environment: Arc<RwLock<Environment>>,
    prompt: Arc<RwLock<Prompt>>,
    github_status: Arc<RwLock<GitHubStatus>>,
) {
    let should_check = {
        let p = prompt.read();
        p.under_git()
    };

    if !should_check {
        return;
    }

    let (pat, filter) = {
        let env = environment.read();
        let pat = env.get_var("*github-pat*");
        let filter = env.get_var("*github-notifications-filter*");
        (pat, filter)
    };

    if let Some(pat_str) = pat {
        let client = GitHubClient::new(pat_str);
        match client.fetch_notifications(filter.as_deref()).await {
            Ok(count) => {
                let mut status = github_status.write();
                status.notification_count = count;
                status.has_error = false;
            }
            Err(e) => {
                error!("Failed to fetch GitHub notifications: {}", e);
                let mut status = github_status.write();
                status.has_error = true;
            }
        }
    }
}
