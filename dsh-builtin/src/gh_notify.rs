use super::ShellProxy;
use dsh_types::{Context, ExitStatus};
use reqwest::blocking::Client;
use reqwest::header::AUTHORIZATION;
use serde::Deserialize;
use skim::prelude::*;
use std::borrow::Cow;
use std::collections::HashMap;
use std::io::IsTerminal;
use std::process::Command;
use std::sync::Arc;

pub fn description() -> &'static str {
    "View and open GitHub notifications interactively"
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
struct Notification {
    id: String,
    subject: Subject,
    repository: Repository,
    reason: String,
    updated_at: String,
    #[serde(default = "default_unread")]
    unread: bool,
}

fn default_unread() -> bool {
    true
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
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
    // Check if running in a terminal
    if !std::io::stdout().is_terminal() {
        ctx.write_stderr("gh-notify: Standard output is not a terminal")
            .ok();
        return ExitStatus::ExitedWith(1);
    }

    // Get PAT
    let pat = if let Some(token) = proxy.get_var("*github-pat*") {
        token
    } else if let Some(token) = proxy.get_lisp_var("*github-pat*") {
        token
    } else if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        token
    } else {
        ctx.write_stderr("gh-notify: *github-pat* variable or GITHUB_TOKEN env not set")
            .ok();
        return ExitStatus::ExitedWith(1);
    };

    // Configure client with timeout
    let client_builder = Client::builder()
        .user_agent("doge-shell")
        .timeout(std::time::Duration::from_secs(10));

    // Fetch notifications in a separate thread
    let pat_clone = pat.clone();
    let handle = std::thread::spawn(move || {
        let client = client_builder.build().map_err(|e| e.to_string())?;
        fetch_notifications(
            &client,
            &pat_clone,
            "https://api.github.com/notifications?all=false",
        )
    });

    let notifications = match handle.join() {
        Ok(Ok(n)) => n,
        Ok(Err(e)) => {
            ctx.write_stderr(&format!("gh-notify: {}", e)).ok();
            return ExitStatus::ExitedWith(1);
        }
        Err(e) => {
            ctx.write_stderr(&format!("gh-notify: Thread panic: {:?}", e))
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
    let options = match SkimOptionsBuilder::default()
        .multi(false)
        .prompt("GitHub> ".to_string())
        .bind(vec!["Enter:accept".to_string(), "Esc:abort".to_string()])
        .build()
    {
        Ok(opt) => opt,
        Err(e) => {
            ctx.write_stderr(&format!("gh-notify: Failed to build options: {}", e))
                .ok();
            return ExitStatus::ExitedWith(1);
        }
    };

    let output = match Skim::run_with(&options, Some(rx_item)) {
        Some(out) => out,
        None => return ExitStatus::ExitedWith(0),
    };

    if output.is_abort {
        return ExitStatus::ExitedWith(0);
    }

    let selected_items = output.selected_items;

    if selected_items.is_empty() {
        return ExitStatus::ExitedWith(0);
    }

    let selected_text = selected_items[0].output();
    let selected_key = selected_text.as_ref();

    if let Some(n) = notification_map.get(selected_key) {
        let pat_clone = pat.clone();
        let api_url_opt = n.subject.url.clone();
        let fallback_url = n.repository.html_url.clone();

        // Resolve URL in a separate thread
        let handle = std::thread::spawn(move || {
            let client = Client::builder()
                .user_agent("doge-shell")
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .ok()?;
            resolve_url(&client, &pat_clone, api_url_opt)
        });

        let target_url = match handle.join() {
            Ok(Some(url)) => url,
            _ => fallback_url,
        };

        let open_cmd = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };

        if let Err(e) = Command::new(open_cmd).arg(&target_url).spawn() {
            ctx.write_stderr(&format!("gh-notify: Failed to open browser: {}", e))
                .ok();
            return ExitStatus::ExitedWith(1);
        }
    }

    ExitStatus::ExitedWith(0)
}

fn fetch_notifications(client: &Client, pat: &str, url: &str) -> Result<Vec<Notification>, String> {
    let response = client
        .get(url)
        .header(AUTHORIZATION, format!("Bearer {}", pat))
        .send()
        .map_err(|e| format!("HTTP request failed: {}", e))?;

    if !response.status().is_success() {
        return Err(format!("API error: {}", response.status()));
    }

    let notifications: Vec<Notification> = response
        .json::<Vec<Notification>>()
        .map_err(|e| format!("Failed to parse JSON: {}", e))?;

    // Client-side filtering to strictly ensure only unread notifications are returned
    Ok(notifications.into_iter().filter(|n| n.unread).collect())
}

fn resolve_url(client: &Client, pat: &str, api_url_opt: Option<String>) -> Option<String> {
    if let Some(api_url) = api_url_opt {
        match client
            .get(&api_url)
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
    }
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

    use std::time::Duration;

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
            unread: true,
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
            unread: true,
        };

        let display = format_notification_display(&n);
        assert_eq!(display, "🔔 [owner/repo] Issue Title (mention)");
    }

    #[test]
    fn test_fetch_notifications_connection_error() {
        // Bind a random port and immediately drop it or let it close
        // Actually picking a free port and NOT listening on it is the best way to get Connection Refused
        // But finding a free port safely is tricky.
        // Instead, let's use a timeout.
        let client = Client::builder()
            .timeout(Duration::from_millis(100))
            .build()
            .unwrap();

        // This IP is reserved for documentation and should not be reachable (TEST-NET-1)
        // It will timeout.
        let result = fetch_notifications(&client, "dummy_token", "http://192.0.2.1");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("HTTP request failed"));
    }

    #[test]
    fn test_resolve_url_none() {
        let client = Client::new();
        assert_eq!(resolve_url(&client, "token", None), None);
    }
}
