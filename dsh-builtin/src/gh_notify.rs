use super::ShellProxy;
use dsh_types::{Context, ExitStatus};
use reqwest::blocking::Client;
use reqwest::header::{AUTHORIZATION, USER_AGENT};
use serde::Deserialize;
use skim::prelude::*;
use std::borrow::Cow;
use std::collections::HashMap;
use std::process::Command;
use std::sync::Arc;

pub fn description() -> &'static str {
    "View and open GitHub notifications interactively"
}

#[derive(Debug, Deserialize, Clone)]
struct Notification {
    id: String,
    subject: Subject,
    repository: Repository,
    reason: String,
    updated_at: String,
}

#[derive(Debug, Deserialize, Clone)]
struct Subject {
    title: String,
    #[serde(rename = "type")]
    subject_type: String,
    url: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
struct Repository {
    full_name: String,
    html_url: String,
}

struct NotificationItem {
    // We don't really need the notification field in item if we use a map,
    // but useful for debugging if needed.
    // For now we just store display_text to be safe.
    display_text: String,
}

impl SkimItem for NotificationItem {
    fn text(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.display_text)
    }

    fn output(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.display_text)
    }
}

pub fn command(ctx: &Context, _argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    // Get PAT
    let pat = match proxy.get_var("*github-pat*") {
        Some(token) => token,
        None => {
            if let Ok(token) = std::env::var("GITHUB_TOKEN") {
                token
            } else {
                ctx.write_stderr("gh-notify: *github-pat* variable or GITHUB_TOKEN env not set")
                    .ok();
                return ExitStatus::ExitedWith(1);
            }
        }
    };

    // Fetch notifications
    let client = Client::new();
    let url = "https://api.github.com/notifications";

    let response = match client
        .get(url)
        .header(USER_AGENT, "doge-shell")
        .header(AUTHORIZATION, format!("Bearer {}", pat))
        .send()
    {
        Ok(res) => res,
        Err(e) => {
            ctx.write_stderr(&format!("gh-notify: HTTP request failed: {}", e))
                .ok();
            return ExitStatus::ExitedWith(1);
        }
    };

    if !response.status().is_success() {
        ctx.write_stderr(&format!("gh-notify: API error: {}", response.status()))
            .ok();
        return ExitStatus::ExitedWith(1);
    }

    let notifications = match response.json::<Vec<Notification>>() {
        Ok(n) => n,
        Err(e) => {
            ctx.write_stderr(&format!("gh-notify: Failed to parse JSON: {}", e))
                .ok();
            return ExitStatus::ExitedWith(1);
        }
    };

    if notifications.is_empty() {
        ctx.write_stdout("No unread notifications.").ok();
        return ExitStatus::ExitedWith(0);
    }

    // Store notifications in a map for lookup
    let mut notification_map = HashMap::new();
    let (tx_item, rx_item): (SkimItemSender, SkimItemReceiver) = unbounded();

    for n in notifications {
        let display_text = format_notification_display(&n);

        notification_map.insert(display_text.clone(), n.clone());

        let item = NotificationItem {
            display_text: display_text.clone(),
        };

        let _ = tx_item.send(Arc::new(item));
    }
    drop(tx_item);

    // Run Skim
    let options = SkimOptionsBuilder::default()
        .height("50%".to_string())
        .multi(false)
        .prompt("GitHub> ".to_string())
        .bind(vec!["Enter:accept".to_string(), "Esc:abort".to_string()])
        .build()
        .unwrap();

    let selected_items = Skim::run_with(&options, Some(rx_item))
        .map(|out| out.selected_items)
        .unwrap_or_default();

    if selected_items.is_empty() {
        return ExitStatus::ExitedWith(0);
    }

    let selected_text = selected_items[0].output();
    let selected_key = selected_text.as_ref();

    if let Some(n) = notification_map.get(selected_key) {
        // Construct URL to open
        let target_url = if let Some(api_url) = &n.subject.url {
            match client
                .get(api_url)
                .header(USER_AGENT, "doge-shell")
                .header(AUTHORIZATION, format!("Bearer {}", pat))
                .send()
            {
                Ok(res) => match res.json::<serde_json::Value>() {
                    Ok(json) => json
                        .get("html_url")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    Err(_) => None,
                },
                Err(_) => None,
            }
        } else {
            None
        };

        let final_url = target_url.unwrap_or_else(|| n.repository.html_url.clone());
        let open_cmd = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };

        if let Err(e) = Command::new(open_cmd).arg(&final_url).spawn() {
            ctx.write_stderr(&format!("gh-notify: Failed to open browser: {}", e))
                .ok();
            return ExitStatus::ExitedWith(1);
        }
    }

    ExitStatus::ExitedWith(0)
}

fn format_notification_display(n: &Notification) -> String {
    let icon = match n.reason.as_str() {
        "review_requested" => "🔍",
        "mention" | "assign" => "🔔",
        "ci_activity" => "🚦",
        _ => "📬",
    };

    format!(
        "{} [{}] {} ({})",
        icon, n.repository.full_name, n.subject.title, n.reason
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_notification_display() {
        let n = Notification {
            id: "1".to_string(),
            subject: Subject {
                title: "Pull Request Title".to_string(),
                subject_type: "PullRequest".to_string(),
                url: None,
            },
            repository: Repository {
                full_name: "owner/repo".to_string(),
                html_url: "https://github.com/owner/repo".to_string(),
            },
            reason: "review_requested".to_string(),
            updated_at: "2023-01-01T00:00:00Z".to_string(),
        };

        let display = format_notification_display(&n);
        assert_eq!(
            display,
            "🔍 [owner/repo] Pull Request Title (review_requested)"
        );
    }

    #[test]
    fn test_format_notification_display_mention() {
        let n = Notification {
            id: "2".to_string(),
            subject: Subject {
                title: "Issue Title".to_string(),
                subject_type: "Issue".to_string(),
                url: None,
            },
            repository: Repository {
                full_name: "owner/repo".to_string(),
                html_url: "https://github.com/owner/repo".to_string(),
            },
            reason: "mention".to_string(),
            updated_at: "2023-01-01T00:00:00Z".to_string(),
        };

        let display = format_notification_display(&n);
        assert_eq!(display, "🔔 [owner/repo] Issue Title (mention)");
    }
}
