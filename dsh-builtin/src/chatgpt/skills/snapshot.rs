//! Turn-owned skill metadata: admission and prompt rendering use the same bytes.
use super::*;

struct RootSnapshot {
    root: SkillRoot,
    skills: Vec<Skill>,
}

#[derive(Default)]
pub(crate) struct SkillsSnapshot {
    roots: Vec<RootSnapshot>,
}

impl SkillsSnapshot {
    /// Capture each root before asking about any project descriptions. Neither
    /// approvals nor rendering may discover additional metadata in this turn.
    pub(crate) fn capture(roots: Vec<SkillRoot>) -> Self {
        Self {
            roots: roots
                .into_iter()
                .map(|root| RootSnapshot {
                    skills: load_root(&root),
                    root,
                })
                .collect(),
        }
    }

    pub(crate) fn roots(&self) -> impl Iterator<Item = &SkillRoot> {
        self.roots.iter().map(|snapshot| &snapshot.root)
    }

    pub(crate) fn retain_roots(&mut self, mut keep: impl FnMut(&SkillRoot) -> bool) {
        self.roots.retain(|snapshot| keep(&snapshot.root));
    }

    pub(crate) fn project_decisions(&self) -> Vec<ProjectSkillDecision> {
        self.roots
            .iter()
            .filter(|snapshot| {
                snapshot.root.scope == SkillScope::Project && !snapshot.skills.is_empty()
            })
            .map(|snapshot| ProjectSkillDecision {
                root: snapshot.root.path.clone(),
                names: snapshot
                    .skills
                    .iter()
                    .map(|skill| skill.name.clone())
                    .collect(),
                digest: trust::digest(&snapshot.skills),
            })
            .collect()
    }

    /// Keep the manager's root precedence and name ordering without touching disk.
    pub(crate) fn skills(&self) -> Vec<Skill> {
        let mut skills = BTreeMap::new();
        for snapshot in &self.roots {
            for skill in &snapshot.skills {
                skills
                    .entry(skill.name.clone())
                    .or_insert_with(|| skill.clone());
            }
        }
        skills.into_values().collect()
    }

    pub(crate) fn render_fragment(&self) -> String {
        self.render_fragment_with_writes(true)
    }

    pub(crate) fn render_fragment_with_writes(&self, allow_writes: bool) -> String {
        if self.roots.is_empty() {
            return String::new();
        }
        let roots: Vec<_> = self.roots().cloned().collect();
        SkillsManager::render_snapshot_fragment(&roots, self.skills(), allow_writes)
    }
}
