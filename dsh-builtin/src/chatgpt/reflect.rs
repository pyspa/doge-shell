//! An optional turn-end reviewer that *proposes* a skill from what a long
//! turn just did. It never writes one itself.
//!
//! Off by default (`AI_CHAT_SKILL_REFLECT`). When it runs after a turn that
//! used at least `AI_CHAT_SKILL_REFLECT_MIN_TOOLS` tool calls, it sends one
//! `tools`-free request and reads the answer with `turn::answer_text` - the
//! same shape a single-shot request like `perform_summary` or `safe_run`
//! already uses. This is deliberately not a third agent loop (see §2 of
//! `ai-architecture.md`, which names the four properties that keep it out of
//! that rule):
//!
//! 1. No `tools` are sent, so no `tool_calls` can come back and nothing here
//!    dispatches one.
//! 2. `send_chat` is called exactly once - no retry loop, no iteration cap
//!    of its own.
//! 3. The answer is read with `turn::answer_text`, the same function every
//!    single-shot request in this crate uses; `interpret_response` (which
//!    exists to read `tool_calls`) is never called.
//! 4. It does not touch the conversation: no `ConversationManager` is
//!    created, nothing is appended to `manager.buffer`, and `session::store`
//!    never sees this request. Its only side effect is at most one file
//!    under `skills::pending` - and until a person runs `skill approve`,
//!    nothing reads that file at all.

use super::skills::{self, MAX_DESCRIPTION_CHARS, MAX_SKILL_SUMMARY_CHARS, SkillScope, pending};
use super::tool::skill::render_skill_md;
use super::{ConversationManager, flatten_conversation};
use crate::shell_capabilities::ChatToolHost;
use dsh_openai::turn::{answer_text, truncate_middle};
use dsh_openai::{
    ChatGptClient, ChatRequestOptions, apply_language_to_field, json_object_format,
    strip_code_fence,
};
use serde::Deserialize;
use serde_json::json;

/// Per-tool-result budget inside the transcript handed to the reviewer.
/// Narrower than `perform_summary`'s: the reviewer only needs enough to
/// recognise a pattern, not to reconstruct exact output.
const REFLECT_TOOL_CHARS: usize = 300;
/// Ceiling on the whole flattened transcript.
const REFLECT_TRANSCRIPT_CHARS: usize = 12_000;
/// Per-skill budget when including a body the turn actually read.
const REFLECT_BODY_CHARS: usize = 4_000;
/// At most this many skills the turn read are sent in full - an index entry
/// (name + description) is cheap; a full body is not.
const REFLECT_MAX_OPENED: usize = 2;
/// How much of the reviewer's own one-line reason is kept.
const REFLECT_NOTE_CHARS: usize = 200;

const REFLECT_SYSTEM_PROMPT: &str = r#"You just watched one turn of an autonomous coding agent. Decide whether the
approach it used is worth saving as a reusable skill for a future run - a
non-trivial multi-step procedure, a working path found after an error or a
dead end, or something a user corrected during the turn.

Do not propose a skill for routine, single-step, or already-documented work.
When in doubt, decline.

Respond with a single JSON object, no prose outside it:
{
  "save": boolean,
  "action": "create" | "replace",
  "name": "lowercase-hyphenated-name (required if save)",
  "scope": "user" | "project",
  "description": "one line, 'Use when ...' - the only thing shown until it is read",
  "body": "the skill's SKILL.md body in Markdown - reproducible steps, assumptions, the pitfall. Never a transcript or a copy of file contents.",
  "replaces": "name of an existing skill this should overwrite instead of creating a new one - only allowed if that skill's body was shown to you below",
  "reason": "one short line saying why, for the person who reviews this before it is written"
}

"replace" is only valid when "replaces" names a skill whose body appears
below under "skills_read_this_turn" - never replace a skill by name alone.
"project" scope is only valid when project_scope_available is true. If none
of this turn is worth saving, respond {"save": false}."#;

/// A skill this turn actually read, in full - name, scope and body.
///
/// Scope travels with it (not just name + body) so a "replace" can target
/// the skill's *actual* scope: a project skill replaced through this must
/// still land in `.dsh/skills`, not silently in the personal root.
struct OpenedSkill {
    name: String,
    scope: SkillScope,
    body: String,
}

/// Everything about this turn a reflection call might need.
struct Snapshot {
    transcript: String,
    index: Vec<(String, String)>,
    opened: Vec<OpenedSkill>,
    project_scope_available: bool,
}

/// Whether this turn qualifies for a reflection request.
///
/// A pure function on purpose: every condition that decides whether an API
/// request gets sent has a test of its own, with no client or proxy needed.
#[allow(clippy::too_many_arguments)]
fn should_reflect(
    enabled: bool,
    min_iterations: usize,
    iterations: usize,
    turn_succeeded: bool,
    wrote_skill_this_turn: bool,
    queue_full: bool,
    budget_exhausted: bool,
) -> bool {
    enabled
        && turn_succeeded
        && !wrote_skill_this_turn
        && !queue_full
        && !budget_exhausted
        && iterations >= min_iterations
}

/// Called once, at the very end of a successful turn - see the call site in
/// `chatgpt.rs` for exactly where and why. Never changes the turn's outcome:
/// every failure here (a disabled setting, a full queue, a request that
/// fails or comes back unparsable, a lint rejection) is swallowed and
/// reported as a single dim line, the same as a skipped hook.
#[allow(clippy::too_many_arguments)]
pub(crate) fn maybe_reflect(
    client: &ChatGptClient,
    proxy: &mut dyn ChatToolHost,
    manager: &mut ConversationManager,
    iterations: usize,
    turn_succeeded: bool,
    turn_token_budget: Option<u64>,
    model_override: Option<String>,
) {
    // Cheapest check first: an env-var read, before anything that touches
    // the filesystem. Off by default costs nothing beyond this.
    if !resolve_enabled(proxy) {
        return;
    }
    if let Some(runtime) = proxy.agent_runtime()
        && runtime.lock().stopped()
    {
        return;
    }

    let min_iterations = resolve_min_iterations(proxy);
    let queue_full = !pending::has_room();
    let budget_exhausted =
        turn_token_budget.is_some_and(|budget| manager.turn_usage.total_tokens() >= budget);

    if !should_reflect(
        true,
        min_iterations,
        iterations,
        turn_succeeded,
        skills::usage::wrote_this_turn(),
        queue_full,
        budget_exhausted,
    ) {
        return;
    }

    match propose(client, proxy, manager, model_override) {
        Ok(Some(id)) => {
            eprintln!(
                "\x1b[2mskills: proposal `{id}` staged; review with `skill diff {id}`\x1b[0m"
            );
        }
        Ok(None) => {}
        Err(reason) => {
            eprintln!("\x1b[2mskills: reflection skipped ({reason})\x1b[0m");
        }
    }
}

const REFLECT_KEY: &str = super::SKILL_REFLECT_KEY;
const REFLECT_MIN_TOOLS_KEY: &str = super::SKILL_REFLECT_MIN_TOOLS_KEY;
const REFLECT_MODEL_KEY: &str = super::SKILL_REFLECT_MODEL_KEY;
/// Hermes Agent's own threshold for "took many steps": five tool calls.
const DEFAULT_MIN_ITERATIONS: usize = 5;

fn resolve_enabled(proxy: &mut dyn ChatToolHost) -> bool {
    match super::resolve_setting(proxy, REFLECT_KEY) {
        None => false,
        Some(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "on" | "yes"
        ),
    }
}

fn resolve_min_iterations(proxy: &mut dyn ChatToolHost) -> usize {
    super::resolve_setting(proxy, REFLECT_MIN_TOOLS_KEY)
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_MIN_ITERATIONS)
}

fn resolve_model(proxy: &mut dyn ChatToolHost, model_override: Option<String>) -> Option<String> {
    super::resolve_setting(proxy, REFLECT_MODEL_KEY)
        .or_else(|| super::resolve_setting(proxy, super::SUMMARY_MODEL_KEY))
        .or(model_override)
}

fn snapshot(proxy: &mut dyn ChatToolHost, manager: &ConversationManager) -> Snapshot {
    let current_dir = proxy.get_current_dir().ok();
    let allow_project = super::resolve_project_skills_enabled(proxy);
    let project_scope_available = current_dir
        .as_deref()
        .is_some_and(|cwd| skills::project_skills_root(cwd).is_some())
        && allow_project;

    let roots = skills::skill_roots(current_dir.as_deref(), allow_project);
    let all_skills = skills::SkillsManager::with_roots(roots).load_skills();

    let index = all_skills
        .iter()
        .map(|skill| (skill.name.clone(), skill.raw_summary().to_string()))
        .collect();

    let opened_dirs = skills::usage::read_this_turn();
    let opened = all_skills
        .iter()
        .filter(|skill| opened_dirs.iter().any(|dir| dir == skill.dir()))
        .take(REFLECT_MAX_OPENED)
        .filter_map(|skill| {
            let body = std::fs::read_to_string(skill.instruction_file()).ok()?;
            Some(OpenedSkill {
                name: skill.name.clone(),
                scope: skill.scope,
                body: skills::truncate_chars(&body, REFLECT_BODY_CHARS),
            })
        })
        .collect();

    let raw_transcript = flatten_conversation(&manager.buffer, REFLECT_TOOL_CHARS);
    let raw_transcript = match &manager.summary {
        Some(summary) => format!("Earlier summary:\n{summary}\n\nThis turn:\n{raw_transcript}"),
        None => raw_transcript,
    };
    let transcript = truncate_middle(&raw_transcript, REFLECT_TRANSCRIPT_CHARS);

    Snapshot {
        transcript,
        index,
        opened,
        project_scope_available,
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ReflectionAnswer {
    save: bool,
    action: String,
    name: String,
    scope: String,
    description: String,
    body: String,
    replaces: Option<String>,
    reason: Option<String>,
}

fn propose(
    client: &ChatGptClient,
    proxy: &mut dyn ChatToolHost,
    manager: &mut ConversationManager,
    model_override: Option<String>,
) -> Result<Option<String>, String> {
    let snap = snapshot(proxy, manager);
    let model = resolve_model(proxy, model_override);
    let language = super::response_language(proxy);
    // Never `apply_language` on a JSON-shaped request (§6 of
    // `ai-architecture.md`): it would translate the field names and the
    // `action`/`scope` enum values along with the prose, and a `"replace"`
    // that comes back as something else matches nothing this reads. Only
    // the one field a person actually reads gets a language instruction.
    let system_prompt =
        apply_language_to_field(REFLECT_SYSTEM_PROMPT, "reason", language.as_deref());

    let user_content = json!({
        "transcript": snap.transcript,
        "existing_skills": snap.index.iter().map(|(name, description)| {
            json!({"name": name, "description": description})
        }).collect::<Vec<_>>(),
        "skills_read_this_turn": snap.opened.iter().map(|opened| {
            json!({"name": opened.name, "body": opened.body})
        }).collect::<Vec<_>>(),
        "project_scope_available": snap.project_scope_available,
    })
    .to_string();

    let messages = vec![
        json!({"role": "system", "content": system_prompt}),
        json!({"role": "user", "content": user_content}),
    ];

    let options = ChatRequestOptions::new()
        .with_temperature(Some(0.2))
        .with_model(model)
        .with_response_format(Some(json_object_format()));

    let response = client
        .send_chat(&messages, &options, Some(&|| super::task_cancelled(proxy)))
        .map_err(|err| format!("request failed: {err}"))?;
    // Counted against this turn's budget like everything else it spent -
    // this is not a request the user gets for free just because it is
    // optional.
    manager.turn_usage.add_response(&response);

    let content = answer_text(&response).map_err(|err| format!("no answer: {err}"))?;
    let cleaned = strip_code_fence(&content);
    let answer: ReflectionAnswer = serde_json::from_str(&cleaned)
        .map_err(|err| format!("answer was not the expected JSON: {err}"))?;

    if !answer.save {
        return Ok(None);
    }

    stage_from_answer(proxy, &snap, answer)
}

fn stage_from_answer(
    proxy: &mut dyn ChatToolHost,
    snap: &Snapshot,
    answer: ReflectionAnswer,
) -> Result<Option<String>, String> {
    // A "replace" is only honoured against a skill whose body this turn
    // actually saw - never by name alone, or the reviewer could silently
    // overwrite a skill it never read.
    let replace_target = match answer.action.as_str() {
        "replace" => {
            let target_name = answer
                .replaces
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .ok_or_else(|| "said \"replace\" without naming which skill".to_string())?;
            let Some(existing) = snap.opened.iter().find(|opened| opened.name == target_name)
            else {
                return Err(format!(
                    "wants to replace `{target_name}`, which this turn never read"
                ));
            };
            Some(existing)
        }
        _ => None,
    };

    let name = match replace_target {
        Some(existing) => existing.name.clone(),
        None => {
            let name = answer.name.trim();
            if name.is_empty() {
                return Err("omitted a skill name".to_string());
            }
            name.to_string()
        }
    };

    let scope = if let Some(existing) = replace_target {
        // A replace targets what is already there - its own scope, read
        // back from the skill this turn actually opened, never a fresh
        // choice the model could get wrong.
        existing.scope
    } else if answer.scope == "project" && snap.project_scope_available {
        SkillScope::Project
    } else {
        SkillScope::User
    };

    let description = answer.description.trim();
    if description.is_empty() {
        return Err("omitted a description".to_string());
    }
    // Truncated to the prompt's display budget, not the (wider) hard write
    // limit: a reviewer proposal that needs the extra room to avoid the
    // hard limit is proposing a description too long to work as a trigger
    // anyway.
    let description = skills::truncate_chars(description, MAX_SKILL_SUMMARY_CHARS);
    debug_assert!(description.chars().count() <= MAX_DESCRIPTION_CHARS);

    let contents = render_skill_md(&name, &description, answer.body.trim());
    let findings = skills::lint::lint_skill_md(&name, &contents);
    if let Some(reason) = skills::lint::has_rejection(&findings) {
        return Err(format!("failed lint: {reason}"));
    }

    let current_dir = proxy
        .get_current_dir()
        .map_err(|err| format!("could not read the current directory: {err}"))?;
    let args = json!({
        "action": if replace_target.is_some() { "write_file" } else { "create" },
        "name": name,
        "scope": scope.as_str(),
    });
    let request = super::tool::skill::validate_in(&args, &current_dir)?;

    let base_digest = std::fs::read_to_string(&request.target)
        .ok()
        .map(|existing| pending::content_digest(&existing));
    let project_root = if request.scope == SkillScope::Project {
        request.skill_dir.parent().map(std::path::Path::to_path_buf)
    } else {
        None
    };
    let note = answer
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
        .map(|reason| skills::truncate_chars(reason, REFLECT_NOTE_CHARS));

    let proposal = pending::Proposal {
        version: 0, // overwritten by `pending::stage`
        id: pending::proposal_id(request.scope, &request.name, &request.relative_file),
        scope: request.scope.as_str().to_string(),
        name: request.name.clone(),
        file: request.relative_file.clone(),
        action: if replace_target.is_some() {
            "write_file".to_string()
        } else {
            "create".to_string()
        },
        project_root,
        contents,
        base_digest,
        created_ms: skills::usage::now_ms(),
        origin: "reflection".to_string(),
        note,
    };

    pending::stage(proposal).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reflection_is_off_by_default() {
        assert!(!should_reflect(false, 5, 100, true, false, false, false));
    }

    #[test]
    fn a_short_turn_does_not_reflect() {
        assert!(!should_reflect(true, 5, 4, true, false, false, false));
        assert!(should_reflect(true, 5, 5, true, false, false, false));
    }

    #[test]
    fn a_failed_turn_does_not_reflect() {
        assert!(!should_reflect(true, 5, 10, false, false, false, false));
    }

    #[test]
    fn a_turn_that_already_wrote_a_skill_does_not_reflect() {
        assert!(!should_reflect(true, 5, 10, true, true, false, false));
    }

    #[test]
    fn a_full_queue_does_not_reflect() {
        assert!(!should_reflect(true, 5, 10, true, false, true, false));
    }

    #[test]
    fn an_exhausted_turn_budget_does_not_reflect() {
        assert!(!should_reflect(true, 5, 10, true, false, false, true));
    }

    #[test]
    fn a_replace_naming_a_skill_the_turn_never_read_is_refused() {
        let snap = Snapshot {
            transcript: String::new(),
            index: Vec::new(),
            opened: vec![OpenedSkill {
                name: "known".to_string(),
                scope: SkillScope::User,
                body: "body".to_string(),
            }],
            project_scope_available: false,
        };
        let mut proxy = crate::test_support::TestShellProxy {
            current_dir: std::env::temp_dir(),
            ..crate::test_support::TestShellProxy::default()
        };
        let answer = ReflectionAnswer {
            save: true,
            action: "replace".to_string(),
            replaces: Some("unread".to_string()),
            description: "Use when x".to_string(),
            body: "step".to_string(),
            ..ReflectionAnswer::default()
        };
        let err = stage_from_answer(&mut proxy, &snap, answer).unwrap_err();
        assert!(err.contains("never read"), "{err}");
    }

    #[test]
    fn a_missing_name_on_create_is_refused() {
        let snap = Snapshot {
            transcript: String::new(),
            index: Vec::new(),
            opened: Vec::new(),
            project_scope_available: false,
        };
        let mut proxy = crate::test_support::TestShellProxy {
            current_dir: std::env::temp_dir(),
            ..crate::test_support::TestShellProxy::default()
        };
        let answer = ReflectionAnswer {
            save: true,
            action: "create".to_string(),
            description: "Use when x".to_string(),
            body: "step".to_string(),
            ..ReflectionAnswer::default()
        };
        let err = stage_from_answer(&mut proxy, &snap, answer).unwrap_err();
        assert!(err.contains("name"), "{err}");
    }

    fn with_state_home<R>(dir: &std::path::Path, f: impl FnOnce() -> R) -> R {
        let _lock = crate::chatgpt::tool::execute::tests::env_lock();
        let previous = std::env::var_os("XDG_STATE_HOME");
        // SAFETY: single-threaded under `env_lock`.
        unsafe { std::env::set_var("XDG_STATE_HOME", dir) };
        let result = f();
        match previous {
            Some(value) => unsafe { std::env::set_var("XDG_STATE_HOME", value) },
            None => unsafe { std::env::remove_var("XDG_STATE_HOME") },
        }
        result
    }

    /// A "replace" against a project-scope skill must stage into
    /// `.dsh/skills`, not silently into the personal root - the scope comes
    /// from the skill this turn actually opened, never a fresh guess.
    #[test]
    fn a_replace_of_a_project_skill_keeps_its_project_scope() {
        let project = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(project.path()).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let state = tempfile::tempdir().unwrap();

        with_state_home(state.path(), || {
            let snap = Snapshot {
                transcript: String::new(),
                index: Vec::new(),
                opened: vec![OpenedSkill {
                    name: "deploy".to_string(),
                    scope: SkillScope::Project,
                    body: "old body".to_string(),
                }],
                project_scope_available: true,
            };
            let mut proxy = crate::test_support::TestShellProxy {
                current_dir: root.clone(),
                ..crate::test_support::TestShellProxy::default()
            };
            let answer = ReflectionAnswer {
                save: true,
                action: "replace".to_string(),
                replaces: Some("deploy".to_string()),
                // A wrong `scope` from the model must not matter either -
                // the replace target's own scope wins.
                scope: "user".to_string(),
                description: "Use when deploying".to_string(),
                body: "new body".to_string(),
                ..ReflectionAnswer::default()
            };

            let id = stage_from_answer(&mut proxy, &snap, answer)
                .unwrap()
                .expect("a valid replace stages a proposal");
            let proposal = pending::find(&id).unwrap();
            assert_eq!(proposal.scope, "project");
            assert_eq!(
                proposal.project_root,
                Some(root.join(".dsh/skills")),
                "{:?}",
                proposal.project_root
            );
        });
    }
}
