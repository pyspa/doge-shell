use anyhow::Result;
use serde::Deserialize;
use skim::SkimItem;
use std::borrow::Cow;
use std::process::{Command, Stdio};

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct PrInfo {
    pub number: u64,
    pub title: String,
    pub author: Author,
    #[serde(rename = "headRefName")]
    pub head_ref_name: String,
    pub state: String,
    #[serde(rename = "isDraft")]
    pub is_draft: bool,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct Author {
    pub login: String,
}

impl PrInfo {
    pub fn display_text(&self) -> String {
        let state_icon = match self.state.as_str() {
            "OPEN" => {
                if self.is_draft {
                    "Draft"
                } else {
                    "Open"
                }
            }
            "MERGED" => "Merged",
            "CLOSED" => "Closed",
            _ => &self.state,
        };
        format!(
            "#{} {} ({}) [{}] @{}",
            self.number, self.title, self.head_ref_name, state_icon, self.author.login
        )
    }
}

// Skim adapter for PrInfo
impl SkimItem for PrInfo {
    fn text(&self) -> Cow<'_, str> {
        Cow::Owned(self.display_text())
    }

    fn output(&self) -> Cow<'_, str> {
        Cow::Owned(self.number.to_string())
    }
}

pub fn is_gh_installed() -> bool {
    Command::new("which")
        .arg("gh")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

pub fn get_prs() -> Result<Vec<PrInfo>, String> {
    let args = vec![
        "pr",
        "list",
        "--limit",
        "100",
        "--json",
        "number,title,author,headRefName,state,isDraft",
    ];

    let output = Command::new("gh")
        .args(&args)
        .output()
        .map_err(|e| format!("failed to execute gh: {e}"))?;

    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        return Err(error.trim().to_string());
    }

    let json_str = String::from_utf8_lossy(&output.stdout);
    parse_prs(&json_str).map_err(|e| format!("failed to parse gh output: {e}"))
}

// Helper for parsing JSON, separated for easier testing
fn parse_prs(json: &str) -> Result<Vec<PrInfo>> {
    let prs: Vec<PrInfo> = serde_json::from_str(json)?;
    Ok(prs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_prs_valid() {
        let json = r#"[
  {
    "number": 123,
    "title": "Fix bug",
    "author": { "login": "octocat" },
    "headRefName": "fix/123",
    "state": "OPEN",
    "isDraft": false
  },
  {
    "number": 124,
    "title": "WIP Feature",
    "author": { "login": "dev" },
    "headRefName": "feat/wip",
    "state": "OPEN",
    "isDraft": true
  }
]"#;
        let prs = parse_prs(json).unwrap();
        assert_eq!(prs.len(), 2);

        assert_eq!(prs[0].number, 123);
        assert_eq!(prs[0].title, "Fix bug");
        assert_eq!(prs[0].author.login, "octocat");
        assert_eq!(prs[0].head_ref_name, "fix/123");
        assert!(!prs[0].is_draft);
        assert_eq!(
            prs[0].display_text(),
            "#123 Fix bug (fix/123) [Open] @octocat"
        );

        assert_eq!(prs[1].number, 124);
        assert!(prs[1].is_draft);
        assert_eq!(
            prs[1].display_text(),
            "#124 WIP Feature (feat/wip) [Draft] @dev"
        );
    }

    #[test]
    fn test_parse_prs_empty() {
        let json = "[]";
        let prs = parse_prs(json).unwrap();
        assert!(prs.is_empty());
    }
}
