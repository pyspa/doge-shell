use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    summary: String,
}

impl Skill {
    pub fn from_folder(path: &Path) -> Result<Self> {
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        let skill_md_path = path.join("SKILL.md");
        if !skill_md_path.exists() {
            anyhow::bail!("SKILL.md not found in folder: {:?}", path);
        }

        let content = std::fs::read_to_string(&skill_md_path)
            .with_context(|| format!("Failed to read SKILL.md: {:?}", skill_md_path))?;

        debug!("Loaded folder skill: {}", name);

        Ok(Self::from_content(name, content))
    }

    pub fn from_file(path: &Path) -> Result<Self> {
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read skill file: {:?}", path))?;

        debug!("Loaded file skill: {}", name);

        Ok(Self::from_content(name, content))
    }

    fn from_content(name: String, instruction: String) -> Self {
        let summary = extract_skill_summary(&instruction);

        Self { name, summary }
    }

    pub fn summary(&self) -> &str {
        &self.summary
    }
}

const MAX_SKILL_SUMMARY_CHARS: usize = 140;

fn extract_skill_summary(instruction: &str) -> String {
    let (frontmatter, body) = split_frontmatter(instruction);
    if let Some(description) = frontmatter_field(frontmatter, "description") {
        return truncate_chars(&collapse_whitespace(&description), MAX_SKILL_SUMMARY_CHARS);
    }

    let body_summary = body
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .unwrap_or("No description available.");

    truncate_chars(&collapse_whitespace(body_summary), MAX_SKILL_SUMMARY_CHARS)
}

fn split_frontmatter(content: &str) -> (Option<&str>, &str) {
    let mut offset = 0usize;
    let mut lines = content.split_inclusive('\n');

    let Some(first) = lines.next() else {
        return (None, content);
    };
    offset += first.len();

    if first.trim() != "---" {
        return (None, content);
    }

    for line in lines {
        offset += line.len();
        if line.trim() == "---" {
            let frontmatter = &content[first.len()..offset - line.len()];
            let body = &content[offset..];
            return (Some(frontmatter), body);
        }
    }

    (None, content)
}

fn frontmatter_field(frontmatter: Option<&str>, key: &str) -> Option<String> {
    let frontmatter = frontmatter?;

    for line in frontmatter.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let Some((field, value)) = trimmed.split_once(':') else {
            continue;
        };
        if field.trim() == key {
            let value = value.trim();
            if value.is_empty() {
                return None;
            }

            return Some(strip_matching_quotes(value).to_string());
        }
    }

    None
}

fn strip_matching_quotes(value: &str) -> &str {
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        let first = bytes[0];
        let last = bytes[value.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return &value[1..value.len() - 1];
        }
    }

    value
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let char_count = text.chars().count();
    if char_count <= max_chars {
        return text.to_string();
    }

    let end = text
        .char_indices()
        .nth(max_chars)
        .map(|(idx, _)| idx)
        .unwrap_or(text.len());
    format!("{}...", &text[..end])
}

pub struct SkillsManager {
    skills_dir: PathBuf,
}

impl SkillsManager {
    pub fn new() -> Self {
        // ~/.config/dsh/skills/
        let config_dir = dirs::config_dir()
            .map(|p| p.join("dsh/skills"))
            .unwrap_or_else(|| PathBuf::from(".config/dsh/skills"));

        Self {
            skills_dir: config_dir,
        }
    }

    pub fn load_skills(&self) -> Vec<Skill> {
        let mut skills = Vec::new();

        if !self.skills_dir.exists() {
            debug!("Skills directory does not exist: {:?}", self.skills_dir);
            return skills;
        }

        match std::fs::read_dir(&self.skills_dir) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        match Skill::from_folder(&path) {
                            Ok(skill) => skills.push(skill),
                            Err(e) => debug!("Skipping directory {:?}: {}", path, e),
                        }
                    } else if path.extension().is_some_and(|ext| ext == "md") {
                        match Skill::from_file(&path) {
                            Ok(skill) => skills.push(skill),
                            Err(e) => warn!("Error loading skill from {:?}: {}", path, e),
                        }
                    }
                }
            }
            Err(e) => {
                warn!("Failed to read skills directory: {}", e);
            }
        }

        skills.sort_by(|a, b| a.name.cmp(&b.name));
        skills
    }

    pub fn get_system_prompt_fragment(&self) -> String {
        let skills = self.load_skills();
        if skills.is_empty() {
            return String::new();
        }

        let mut fragment = String::from(
            "\n\n## Agent Skills\nAvailable runtime skills from `~/.config/dsh/skills/`:\n",
        );

        for skill in &skills {
            fragment.push_str(&format!("- `{}`: {}\n", skill.name, skill.summary()));
        }

        fragment.push_str("\nRead a skill only when needed with `read_file(path=\"~/.config/dsh/skills/<skill>/SKILL.md\")`.\n");
        fragment.push_str(
            "Use files in that skill directory only after you know the skill is relevant.\n",
        );

        fragment
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn summary_prefers_frontmatter_description() {
        let skill = Skill::from_content(
            "demo".to_string(),
            r#"---
name: demo
description: "Short runtime summary"
---

# Demo

Longer explanation.
"#
            .to_string(),
        );

        assert_eq!(skill.summary(), "Short runtime summary");
    }

    #[test]
    fn summary_falls_back_to_body_without_frontmatter() {
        let skill = Skill::from_content(
            "demo".to_string(),
            "# Demo\n\nUse this to inspect prompts.\n".to_string(),
        );

        assert_eq!(skill.summary(), "Use this to inspect prompts.");
    }

    #[test]
    fn summary_truncates_long_descriptions() {
        let repeated = "a".repeat(MAX_SKILL_SUMMARY_CHARS + 10);
        let skill = Skill::from_content(
            "demo".to_string(),
            format!("---\ndescription: \"{repeated}\"\n---\n"),
        );

        assert!(skill.summary().ends_with("..."));
        assert!(skill.summary().chars().count() <= MAX_SKILL_SUMMARY_CHARS + 3);
    }

    #[test]
    fn system_prompt_fragment_uses_compact_summary() {
        let dir = tempdir().unwrap();
        let skills_dir = dir.path().join("skills");
        let skill_dir = skills_dir.join("demo-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: compact summary\n---\n# Demo\n",
        )
        .unwrap();

        let manager = SkillsManager { skills_dir };
        let fragment = manager.get_system_prompt_fragment();

        assert!(fragment.contains("- `demo-skill`: compact summary"));
        assert!(fragment.contains("read_file(path=\"~/.config/dsh/skills/<skill>/SKILL.md\")"));
        assert!(!fragment.contains("### Progressive Disclosure"));
    }
}
