use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub instruction: String,
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

        Ok(Self {
            name,
            instruction: content,
        })
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

        Ok(Self {
            name,
            instruction: content,
        })
    }

    pub fn summary(&self) -> String {
        // Extract first non-empty line as summary if it's not a header
        self.instruction
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty() && !l.starts_with('#'))
            .unwrap_or("No description available.")
            .to_string()
    }
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
            "\n\n## Agent Skills\nYou have access to the following specialized skills. Each skill is defined in a folder under `~/.config/dsh/skills/`.\n",
        );

        fragment.push_str("\n### Available Skills:\n");
        for skill in &skills {
            fragment.push_str(&format!("- **{}**: {}\n", skill.name, skill.summary()));
        }

        fragment.push_str("\n### Progressive Disclosure:\n");
        fragment.push_str("For detailed instructions on a specific skill, use the `read_file` tool to read its `SKILL.md` file.\n");
        fragment.push_str(
            "Example: `read_file(path=\"~/.config/dsh/skills/<skill_name>/SKILL.md\")`\n",
        );
        fragment.push_str(
            "Skill resources (scripts, data) are also located in their respective folders.\n",
        );

        fragment
    }
}
