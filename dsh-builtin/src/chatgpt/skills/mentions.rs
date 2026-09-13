//! `@name` mentions in a chat message: parsing them off the front (`split_leading_mentions`), and rendering the full skill body plus its bundled file listing when one is invoked (`render_mention`).
use super::*;

/// How many `@name` mentions one message may carry.
const MAX_MENTIONS: usize = 5;
/// How many bundled files to list when a skill is invoked by name.
const MAX_LISTED_RESOURCES: usize = 20;
/// Split leading `@name` mentions off the front of a chat message.
///
/// A skill's summary is in the prompt, but whether the model acts on it is its
/// own judgement, and a shell conversation is often over in one turn - there is
/// no second chance for it to notice. `@name` is the user saying so outright.
///
/// Parsing stops at the first token that is not a known skill, so `@user@host`,
/// an email address, or a message that merely starts with `@` are left alone.
/// `@` was chosen over `/` and `$`: one is a path, the other a variable.
pub(crate) fn split_leading_mentions<'a>(
    input: &'a str,
    is_skill: &dyn Fn(&str) -> bool,
) -> (Vec<String>, &'a str) {
    let mut names = Vec::new();
    let mut rest = input.trim_start();

    while names.len() < MAX_MENTIONS {
        let Some(candidate) = rest.strip_prefix('@') else {
            break;
        };
        let end = candidate
            .find(char::is_whitespace)
            .unwrap_or(candidate.len());
        let name = &candidate[..end];
        if name.is_empty() || !is_skill(name) || names.iter().any(|seen| seen == name) {
            break;
        }
        names.push(name.to_string());
        rest = candidate[end..].trim_start();
    }

    (names, rest)
}
/// The full text of a skill, with its bundled files named but not read.
///
/// Listing `references/`, `scripts/` and `assets/` is the difference between
/// the model knowing they exist and having to guess that an `ls` might be worth
/// a turn. They are named, never loaded: that is the whole point of the tier.
pub(crate) fn render_mention(skill: &Skill) -> Option<String> {
    let path = skill.instruction_file();
    let body = std::fs::read_to_string(&path).ok()?;

    let mut rendered = format!(
        "Skill `{}` ({}), loaded because the user asked for it by name:

{}",
        skill.name,
        skill.instruction_path(),
        body.trim_end()
    );

    let resources = bundled_resources(skill.dir());
    if !resources.is_empty() {
        rendered.push_str(&format!(
            "

Files bundled with this skill, relative to `{}` - read one with `read_file` only if the instructions above call for it:
",
            crate::config_paths::display_path(skill.dir())
        ));
        for resource in resources {
            rendered.push_str(&format!(
                "- {resource}
"
            ));
        }
    }

    Some(rendered)
}
fn bundled_resources(dir: &Path) -> Vec<String> {
    if !dir.is_dir() {
        return Vec::new();
    }

    let mut found = Vec::new();
    for section in ["references", "scripts", "assets"] {
        let Ok(entries) = std::fs::read_dir(dir.join(section)) else {
            continue;
        };
        for entry in entries.flatten() {
            if !entry.path().is_file() {
                continue;
            }
            if let Some(name) = entry.file_name().to_str() {
                found.push(format!("{section}/{name}"));
            }
            if found.len() >= MAX_LISTED_RESOURCES {
                found.push("... (more not listed)".to_string());
                return found;
            }
        }
    }

    found.sort();
    found
}
/// Attribute a successful `read_file` to the skill that owns the path.
///
/// A no-op for every path outside a skill root, which is almost all of them.
pub(crate) fn note_skill_read(path: &Path, current_dir: &Path) {
    if let Some((dir, scope)) = containing_skill(path, current_dir) {
        usage::note_read(&dir, scope);
    }
}
