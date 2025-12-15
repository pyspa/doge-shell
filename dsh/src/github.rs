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
    pub review_count: usize,
    pub mention_count: usize,
    pub other_count: usize,
    pub has_error: bool,
}

impl GitHubStatus {
    pub fn total(&self) -> usize {
        self.review_count + self.mention_count + self.other_count
    }
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

    pub async fn fetch_notifications(&self, filter: Option<&str>) -> Result<GitHubStatus> {
        let url = "https://api.github.com/notifications";
        let request = self.client.request(Method::GET, url).bearer_auth(&self.pat);

        let response = request.send().await?;

        if !response.status().is_success() {
            anyhow::bail!("GitHub API error: {}", response.status());
        }

        let notifications: Vec<serde_json::Value> = response.json().await?;

        // Use the decoupled parsing function
        let status = parse_notifications(&notifications, filter);

        debug!(
            "GitHub notifications: review={}, mention={}, other={}",
            status.review_count, status.mention_count, status.other_count
        );

        Ok(status)
    }
}

fn parse_notifications(notifications: &[serde_json::Value], filter: Option<&str>) -> GitHubStatus {
    // Filter logic if needed (legacy filter string support)
    let allowed_reasons: Option<Vec<&str>> = if let Some(f) = filter
        && let Some(reason_val) = f.strip_prefix("reason:")
    {
        Some(reason_val.split(',').map(|s| s.trim()).collect())
    } else {
        None
    };

    let mut status = GitHubStatus::default();

    for n in notifications {
        let reason = n["reason"].as_str().unwrap_or("");

        // Debug logging for reasons is now inside here or could be passed a logger?
        // Keeping it simple for now, maybe reduce log spam or rely on caller?
        // The original code debug logged filtered stats.

        // Check legacy filter first
        if let Some(allowed) = &allowed_reasons
            && !allowed.contains(&reason)
        {
            continue;
        }

        match reason {
            "review_requested" => status.review_count += 1,
            "mention" | "assign" => status.mention_count += 1,
            _ => status.other_count += 1,
        }
    }

    // Original debug log was: "Filtered notifications (filter: '{}'): {} / {}"
    // We can't easily reproduce exact same side-effect logging here without passing more context,
    // but the core logic is preserved.

    status
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
            continue;
        }

        if let Some(pat_str) = pat {
            let client = GitHubClient::new(pat_str);
            debug!("Checking GitHub notifications...");
            match client.fetch_notifications(filter.as_deref()).await {
                Ok(new_status) => {
                    let mut status = github_status.write();
                    *status = new_status;
                }
                Err(e) => {
                    error!("Failed to fetch GitHub notifications: {}", e);
                    let mut status = github_status.write();
                    status.has_error = true;
                }
            }
        }
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
            Ok(new_status) => {
                let mut status = github_status.write();
                *status = new_status;
            }
            Err(e) => {
                error!("Failed to fetch GitHub notifications: {}", e);
                let mut status = github_status.write();
                status.has_error = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_parse_notifications_counts() {
        let data = vec![
            json!({ "reason": "review_requested" }),
            json!({ "reason": "review_requested" }),
            json!({ "reason": "mention" }),
            json!({ "reason": "assign" }),
            json!({ "reason": "subscribed" }),
            json!({ "reason": "comment" }),
        ];

        let status = parse_notifications(&data, None);

        assert_eq!(status.review_count, 2);
        assert_eq!(status.mention_count, 2); // mention + assign
        assert_eq!(status.other_count, 2); // subscribed + comment
        assert_eq!(status.total(), 6);
    }

    #[test]
    fn test_parse_notifications_filter() {
        let data = vec![
            json!({ "reason": "review_requested" }),
            json!({ "reason": "mention" }),
            json!({ "reason": "other" }),
        ];

        let filter = Some("reason:review_requested,mention");
        let status = parse_notifications(&data, filter);

        assert_eq!(status.review_count, 1);
        assert_eq!(status.mention_count, 1);
        assert_eq!(status.other_count, 0);
    }

    #[test]
    fn test_parse_notifications_empty() {
        let data = vec![];
        let status = parse_notifications(&data, None);
        assert_eq!(status.total(), 0);
    }
}
