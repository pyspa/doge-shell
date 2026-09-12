use super::*;
use std::fs;
use tempfile::tempdir;

fn user_root(path: &Path) -> SkillRoot {
    SkillRoot {
        scope: SkillScope::User,
        origin: SkillOrigin::Dsh,
        path: path.to_path_buf(),
    }
}

fn project_root(path: &Path) -> SkillRoot {
    SkillRoot {
        scope: SkillScope::Project,
        origin: SkillOrigin::Dsh,
        path: path.to_path_buf(),
    }
}

fn project_agents_root(path: &Path) -> SkillRoot {
    SkillRoot {
        scope: SkillScope::Project,
        origin: SkillOrigin::Agents,
        path: path.to_path_buf(),
    }
}

fn write_skill(root: &Path, name: &str, description: &str) -> PathBuf {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\n# {name}\n"),
    )
    .unwrap();
    dir
}

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
        Path::new("/tmp/skills/demo/SKILL.md"),
        PathBuf::from("/tmp/skills/demo"),
        &user_root(Path::new("/tmp/skills")),
    );

    assert_eq!(skill.summary(), "Short runtime summary");
}

#[test]
fn summary_falls_back_to_body_without_frontmatter() {
    let skill = Skill::from_content(
        "demo".to_string(),
        "# Demo\n\nUse this to inspect prompts.\n".to_string(),
        Path::new("/tmp/skills/demo/SKILL.md"),
        PathBuf::from("/tmp/skills/demo"),
        &user_root(Path::new("/tmp/skills")),
    );

    assert_eq!(skill.summary(), "Use this to inspect prompts.");
}

#[test]
fn summary_truncates_long_descriptions() {
    let repeated = "a".repeat(MAX_SKILL_SUMMARY_CHARS + 10);
    let skill = Skill::from_content(
        "demo".to_string(),
        format!("---\ndescription: \"{repeated}\"\n---\n"),
        Path::new("/tmp/skills/demo/SKILL.md"),
        PathBuf::from("/tmp/skills/demo"),
        &user_root(Path::new("/tmp/skills")),
    );

    assert!(skill.summary().ends_with("..."));
    assert!(skill.summary().chars().count() <= MAX_SKILL_SUMMARY_CHARS + 3);
}

#[test]
fn a_description_up_to_the_new_budget_survives_into_the_prompt() {
    let at_budget = "b".repeat(MAX_SKILL_SUMMARY_CHARS);
    let skill = Skill::from_content(
        "demo".to_string(),
        format!("---\ndescription: \"{at_budget}\"\n---\n"),
        Path::new("/tmp/skills/demo/SKILL.md"),
        PathBuf::from("/tmp/skills/demo"),
        &user_root(Path::new("/tmp/skills")),
    );

    assert_eq!(skill.summary(), at_budget);
    assert!(!skill.summary().ends_with("..."));
}

/// The trust digest has to tell these two skills apart even though the
/// prompt shows them identically - it is what the user is agreeing to,
/// not what happened to fit on screen.
#[test]
fn the_trust_digest_does_not_change_when_only_the_summary_budget_changes() {
    let shared_prefix = "d".repeat(MAX_SKILL_SUMMARY_CHARS + 10);
    let a = Skill::from_content(
        "demo".to_string(),
        format!("---\ndescription: \"{shared_prefix}AAAA\"\n---\n"),
        Path::new("/tmp/skills/demo/SKILL.md"),
        PathBuf::from("/tmp/skills/demo"),
        &user_root(Path::new("/tmp/skills")),
    );
    let b = Skill::from_content(
        "demo".to_string(),
        format!("---\ndescription: \"{shared_prefix}BBBB\"\n---\n"),
        Path::new("/tmp/skills/demo/SKILL.md"),
        PathBuf::from("/tmp/skills/demo"),
        &user_root(Path::new("/tmp/skills")),
    );

    // Same prompt line: both descriptions agree up to the display budget.
    assert_eq!(a.summary(), b.summary());
    // Different descriptions: the digest must not agree too.
    assert_ne!(a.raw_summary(), b.raw_summary());
    assert_ne!(trust::digest(&[a]), trust::digest(&[b]));
}

/// A block scalar used to fall through to the body, so the model saw the
/// first heading-less line of prose instead of the author's summary.
#[test]
fn frontmatter_reads_a_block_scalar_description() {
    let skill = Skill::from_content(
        "demo".to_string(),
        "---\ndescription: >\n  Use for deploys\n  and rollbacks\n---\n# Demo\n\nbody line\n"
            .to_string(),
        Path::new("/tmp/skills/demo/SKILL.md"),
        PathBuf::from("/tmp/skills/demo"),
        &user_root(Path::new("/tmp/skills")),
    );

    assert_eq!(skill.summary(), "Use for deploys and rollbacks");
}

/// An indented key belongs to whatever mapping encloses it, not to the skill.
#[test]
fn frontmatter_ignores_a_nested_description_key() {
    let skill = Skill::from_content(
        "demo".to_string(),
        "---\nmetadata:\n  description: nested and wrong\n---\n# Demo\n\nthe real summary\n"
            .to_string(),
        Path::new("/tmp/skills/demo/SKILL.md"),
        PathBuf::from("/tmp/skills/demo"),
        &user_root(Path::new("/tmp/skills")),
    );

    assert_eq!(skill.summary(), "the real summary");
}

#[test]
fn system_prompt_fragment_uses_compact_summary() {
    clear_skills_fragment_cache();
    let dir = tempdir().unwrap();
    let skills_dir = dir.path().join("skills");
    write_skill(&skills_dir, "demo-skill", "compact summary");

    let manager = SkillsManager::with_roots(vec![user_root(&skills_dir)]);
    let fragment = manager.get_system_prompt_fragment();

    assert!(fragment.contains("- `demo-skill`"));
    assert!(fragment.contains("compact summary"));
    // The path is the one actually read, not a hard-coded `~/.config`.
    assert!(fragment.contains(&skills_dir.join("demo-skill/SKILL.md").display().to_string()));
    assert!(fragment.contains(&skills_dir.display().to_string()));
    assert!(!fragment.contains("### Progressive Disclosure"));
}

/// A bare `*.md` skill has no `SKILL.md`, so pointing the model at
/// `<name>/SKILL.md` sent it after a file that does not exist.
#[test]
fn system_prompt_fragment_points_a_file_skill_at_the_file() {
    clear_skills_fragment_cache();
    let dir = tempdir().unwrap();
    let skills_dir = dir.path().join("skills");
    fs::create_dir_all(&skills_dir).unwrap();
    fs::write(
        skills_dir.join("loose-note.md"),
        "---\ndescription: a bare file skill\n---\n",
    )
    .unwrap();

    let manager = SkillsManager::with_roots(vec![user_root(&skills_dir)]);
    let fragment = manager.get_system_prompt_fragment();

    assert!(fragment.contains(&skills_dir.join("loose-note.md").display().to_string()));
    assert!(!fragment.contains("loose-note/SKILL.md"));
}

#[test]
fn system_prompt_fragment_cache_invalidates_when_skills_change() {
    clear_skills_fragment_cache();
    let dir = tempdir().unwrap();
    let skills_dir = dir.path().join("skills");
    write_skill(&skills_dir, "demo-skill", "first summary");

    let manager = SkillsManager::with_roots(vec![user_root(&skills_dir)]);
    let first = manager.get_system_prompt_fragment();
    assert!(first.contains("first summary"));

    write_skill(&skills_dir, "second-skill", "second summary");

    let second = manager.get_system_prompt_fragment();
    assert!(second.contains("first summary"));
    assert!(second.contains("second summary"));
}

/// The project block comes first because that is the order the roots are
/// searched in, and the model reads the list top-down.
#[test]
fn project_skills_are_listed_before_user_skills() {
    clear_skills_fragment_cache();
    let dir = tempdir().unwrap();
    let project = dir.path().join("proj/.dsh/skills");
    let user = dir.path().join("home/skills");
    write_skill(&project, "deploy", "repo deploy steps");
    write_skill(&user, "bisect", "personal bisect notes");

    let manager = SkillsManager::with_roots(vec![project_root(&project), user_root(&user)]);
    let fragment = manager.get_system_prompt_fragment();

    let project_at = fragment.find("Project skills").expect("project block");
    let user_at = fragment.find("Personal skills").expect("user block");
    assert!(project_at < user_at);
    assert!(fragment.contains("provided by this repository"));
    assert!(fragment.contains("- `deploy`"));
    assert!(fragment.contains("- `bisect`"));
}

/// Listing the same name twice would leave the model to guess which file to
/// open. The more specific root wins.
#[test]
fn a_project_skill_shadows_a_user_skill_with_the_same_name() {
    clear_skills_fragment_cache();
    let dir = tempdir().unwrap();
    let project = dir.path().join("proj/.dsh/skills");
    let user = dir.path().join("home/skills");
    write_skill(&project, "deploy", "repo version");
    write_skill(&user, "deploy", "personal version");

    let manager = SkillsManager::with_roots(vec![project_root(&project), user_root(&user)]);
    let skills = manager.load_skills();

    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].scope, SkillScope::Project);
    let fragment = manager.get_system_prompt_fragment();
    assert!(fragment.contains("repo version"));
    assert!(!fragment.contains("personal version"));
}

/// With nothing installed the fragment used to be empty, so a model that had
/// never seen a skill was never told it could write one.
#[test]
fn the_fragment_is_emitted_with_no_skills_so_creating_one_is_discoverable() {
    clear_skills_fragment_cache();
    let dir = tempdir().unwrap();
    let skills_dir = dir.path().join("skills");

    let manager = SkillsManager::with_roots(vec![user_root(&skills_dir)]);
    let fragment = manager.get_system_prompt_fragment();

    assert!(fragment.contains("## Agent Skills"));
    assert!(fragment.contains("skill_manage"));
    assert!(fragment.contains(&skills_dir.display().to_string()));
}

/// `build_system_prompt` passes no roots in tests that do not care about
/// skills; that has to stay a no-op on the prompt.
#[test]
fn no_roots_renders_nothing() {
    clear_skills_fragment_cache();
    let manager = SkillsManager::with_roots(Vec::new());
    assert!(manager.get_system_prompt_fragment().is_empty());
}

/// The cache key covers every root, so entering a project with its own
/// skills does not keep serving the previous project's list.
#[test]
fn the_cache_invalidates_when_only_the_project_root_changes() {
    clear_skills_fragment_cache();
    let dir = tempdir().unwrap();
    let user = dir.path().join("home/skills");
    write_skill(&user, "shared", "always here");
    let first_project = dir.path().join("a/.dsh/skills");
    write_skill(&first_project, "alpha", "project a");
    let second_project = dir.path().join("b/.dsh/skills");
    write_skill(&second_project, "beta", "project b");

    let first = SkillsManager::with_roots(vec![project_root(&first_project), user_root(&user)])
        .get_system_prompt_fragment();
    let second = SkillsManager::with_roots(vec![project_root(&second_project), user_root(&user)])
        .get_system_prompt_fragment();

    assert!(first.contains("project a") && !first.contains("project b"));
    assert!(second.contains("project b") && !second.contains("project a"));
}

/// The prompt used to advertise a symlinked skill whose canonical path is
/// outside the root - which every tool then refused to read.
#[cfg(unix)]
#[test]
fn a_symlinked_skill_that_escapes_its_root_is_reported_not_listed() {
    use std::os::unix::fs::symlink;

    clear_skills_fragment_cache();
    let dir = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let root = dir.path().join("skills");
    fs::create_dir_all(&root).unwrap();
    write_skill(outside.path(), "elsewhere", "somewhere else");
    symlink(outside.path().join("elsewhere"), root.join("elsewhere")).unwrap();
    write_skill(&root, "local", "right here");

    let (skills, problems) = SkillsManager::with_roots(vec![user_root(&root)]).load_reporting();

    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "local");
    assert!(
        problems.iter().any(|p| p.problem.contains("links outside")),
        "{problems:?}"
    );
}

/// Deleting a `SKILL.md` and leaving the directory changed neither the
/// entry count nor the newest mtime, so the old fragment kept being served.
#[test]
fn the_cache_notices_a_deleted_skill_md() {
    clear_skills_fragment_cache();
    let dir = tempdir().unwrap();
    let root = dir.path().join("skills");
    let doomed = write_skill(&root, "doomed", "about to go");
    write_skill(&root, "keeper", "stays put");

    let manager = SkillsManager::with_roots(vec![user_root(&root)]);
    assert!(manager.get_system_prompt_fragment().contains("about to go"));

    fs::remove_file(doomed.join("SKILL.md")).unwrap();

    let after = manager.get_system_prompt_fragment();
    assert!(!after.contains("about to go"), "{after}");
    assert!(after.contains("stays put"));
}

/// `foo/` and `foo.md` in one root is two skills with one name. Which one
/// won used to be `read_dir` order.
#[test]
fn a_directory_skill_beats_a_file_skill_of_the_same_name() {
    clear_skills_fragment_cache();
    let dir = tempdir().unwrap();
    let root = dir.path().join("skills");
    write_skill(&root, "deploy", "the directory one");
    fs::write(
        root.join("deploy.md"),
        "---\ndescription: the file one\n---\n",
    )
    .unwrap();

    let (skills, problems) = SkillsManager::with_roots(vec![user_root(&root)]).load_reporting();

    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].summary(), "the directory one");
    assert!(
        problems.iter().any(|p| p.problem.contains("shadowed")),
        "{problems:?}"
    );
}

/// "Why is the skill I wrote not in the prompt?" had no answer at all:
/// every one of these went to `debug!`.
#[test]
fn load_problems_are_reported_rather_than_swallowed() {
    clear_skills_fragment_cache();
    let dir = tempdir().unwrap();
    let root = dir.path().join("skills");
    fs::create_dir_all(root.join("no-skill-md")).unwrap();
    let mismatched = root.join("on-disk");
    fs::create_dir_all(&mismatched).unwrap();
    fs::write(
        mismatched.join("SKILL.md"),
        "---\nname: in-frontmatter\ndescription: d\n---\n",
    )
    .unwrap();
    let bare = root.join("undescribed");
    fs::create_dir_all(&bare).unwrap();
    fs::write(bare.join("SKILL.md"), "# just a body\n\nsome prose\n").unwrap();

    let (_skills, problems) = SkillsManager::with_roots(vec![user_root(&root)]).load_reporting();

    let joined = problems
        .iter()
        .map(|p| p.problem.as_str())
        .collect::<Vec<_>>()
        .join(" | ");
    assert!(joined.contains("SKILL.md not found"), "{joined}");
    assert!(joined.contains("does not match the directory"), "{joined}");
    assert!(joined.contains("no frontmatter `description`"), "{joined}");
}

/// `doctor` called this "missing", which sends the user looking in the
/// wrong place.
#[test]
fn a_skills_path_that_is_a_file_is_reported_as_such() {
    clear_skills_fragment_cache();
    let dir = tempdir().unwrap();
    let root = dir.path().join("skills");
    fs::write(&root, "not a directory\n").unwrap();

    let (skills, problems) = SkillsManager::with_roots(vec![user_root(&root)]).load_reporting();

    assert!(skills.is_empty());
    assert!(
        problems
            .iter()
            .any(|p| p.problem.contains("not a directory")),
        "{problems:?}"
    );
}

/// The precedence is intended; going quiet about it is not.
#[test]
fn a_shadowed_personal_skill_is_reported() {
    clear_skills_fragment_cache();
    let dir = tempdir().unwrap();
    let project = dir.path().join("proj/.dsh/skills");
    let user = dir.path().join("home/skills");
    write_skill(&project, "deploy", "repo version");
    write_skill(&user, "deploy", "personal version");

    let (skills, problems) =
        SkillsManager::with_roots(vec![project_root(&project), user_root(&user)]).load_reporting();

    assert_eq!(skills.len(), 1);
    assert!(
        problems
            .iter()
            .any(|p| p.problem.contains("shadowed by the project skill")),
        "{problems:?}"
    );
}

/// Parsing has to stop at the first token that is not a skill, or an email
/// address at the start of a message becomes a failed lookup.
#[test]
fn leading_at_tokens_load_skills_and_stop_at_the_first_other_word() {
    let known = |name: &str| matches!(name, "deploy" | "bisect");

    let (names, rest) = split_leading_mentions("@deploy @bisect fix the build", &known);
    assert_eq!(names, vec!["deploy", "bisect"]);
    assert_eq!(rest, "fix the build");

    // Stops at the first unknown name, and leaves it in the text.
    let (names, rest) = split_leading_mentions("@deploy @nope do it", &known);
    assert_eq!(names, vec!["deploy"]);
    assert_eq!(rest, "@nope do it");

    // Not a mention at all.
    let (names, rest) = split_leading_mentions("@user@host mail them", &known);
    assert!(names.is_empty());
    assert_eq!(rest, "@user@host mail them");

    let (names, rest) = split_leading_mentions("just a question", &known);
    assert!(names.is_empty());
    assert_eq!(rest, "just a question");

    // A repeat is a typo, not a second load.
    let (names, _) = split_leading_mentions("@deploy @deploy go", &known);
    assert_eq!(names, vec!["deploy"]);

    // A bare `@` is not a name.
    let (names, rest) = split_leading_mentions("@ deploy", &known);
    assert!(names.is_empty());
    assert_eq!(rest, "@ deploy");
}

/// Naming the bundled files is what makes the third tier discoverable; the
/// model was otherwise left to guess that an `ls` might be worth a turn.
#[test]
fn an_invoked_skill_carries_its_body_and_names_its_bundled_files() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("skills");
    let skill = write_skill(&root, "deploy", "repo deploy steps");
    fs::create_dir_all(skill.join("references")).unwrap();
    fs::write(skill.join("references/api.md"), "detail\n").unwrap();
    fs::create_dir_all(skill.join("scripts")).unwrap();
    fs::write(skill.join("scripts/run.sh"), "echo\n").unwrap();

    let loaded = SkillsManager::with_roots(vec![user_root(&root)]).load_skills();
    let rendered = render_mention(&loaded[0]).expect("body");

    assert!(rendered.contains("# deploy"), "{rendered}");
    assert!(rendered.contains("references/api.md"), "{rendered}");
    assert!(rendered.contains("scripts/run.sh"), "{rendered}");
    assert!(
        rendered.contains("only if the instructions above call for it"),
        "{rendered}"
    );
}

#[test]
fn a_skill_with_no_bundled_files_lists_none() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("skills");
    write_skill(&root, "plain", "nothing extra");

    let loaded = SkillsManager::with_roots(vec![user_root(&root)]).load_skills();
    let rendered = render_mention(&loaded[0]).expect("body");

    assert!(!rendered.contains("Files bundled"), "{rendered}");
}

#[test]
fn a_reference_file_is_attributed_to_its_skill_directory() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("skills");
    let skill = write_skill(&root, "demo", "d");
    let roots = vec![user_root(&root)];

    let (owner, scope) =
        containing_skill_in(&roots, &skill.join("references/deep.md")).expect("owner");

    assert_eq!(owner, skill);
    assert_eq!(scope, SkillScope::User);
    assert!(containing_skill_in(&roots, Path::new("/etc/hosts")).is_none());
}

/// A project root with no project marker must not be invented: `.dsh/skills`
/// under an arbitrary directory is not a project skill root.
#[test]
fn project_root_is_skipped_without_a_project_marker() {
    let dir = tempdir().unwrap();
    let plain = dir.path().join("not-a-project");
    std::fs::create_dir_all(&plain).unwrap();

    assert!(project_skills_root(&plain).is_none());
}

#[test]
fn a_project_marker_makes_a_project_skills_root() {
    let dir = tempdir().unwrap();
    let project = dir.path().join("proj");
    std::fs::create_dir_all(project.join(".git")).unwrap();

    assert_eq!(
        project_skills_root(&project),
        Some(project.join(".dsh").join("skills"))
    );
}

/// One switch, both project roots. A cloned repository can put text in
/// front of the model from either directory.
#[test]
fn turning_project_skills_off_drops_both_project_roots() {
    let dir = tempdir().unwrap();
    let project = dir.path().join("proj");
    std::fs::create_dir_all(project.join(".git")).unwrap();

    let roots = skill_roots(Some(&project), false);

    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].scope, SkillScope::User);
}

#[test]
fn the_agents_root_needs_a_project_marker_like_the_dsh_one() {
    let dir = tempdir().unwrap();
    let bare = dir.path().join("bare");
    std::fs::create_dir_all(&bare).unwrap();
    assert_eq!(project_agents_skills_root(&bare), None);

    let project = dir.path().join("proj");
    std::fs::create_dir_all(project.join(".git")).unwrap();
    assert_eq!(
        project_agents_skills_root(&project),
        Some(project.join(".agents").join("skills"))
    );
}

/// `.dsh` is this shell's own answer, so it beats the shared one, which in
/// turn beats the personal root.
#[test]
fn skill_root_precedence_is_dsh_then_agents_then_user() {
    let dir = tempdir().unwrap();
    let project = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::create_dir_all(project.join(".git")).unwrap();

    let roots = skill_roots(Some(&project), true);
    let paths: Vec<&Path> = roots.iter().map(|root| root.path.as_path()).collect();
    assert_eq!(
        paths[..2],
        [
            project.join(".dsh").join("skills").as_path(),
            project.join(".agents").join("skills").as_path()
        ]
    );
    assert_eq!(roots[0].origin, SkillOrigin::Dsh);
    assert_eq!(roots[1].origin, SkillOrigin::Agents);
    assert_eq!(roots[2].scope, SkillScope::User);

    // Same name in all three: the most specific one is what loads, and the
    // others are reported as shadowed rather than silently gone.
    let dsh = project.join(".dsh/skills");
    let agents = project.join(".agents/skills");
    let personal = project.join("personal");
    for root in [&dsh, &agents, &personal] {
        std::fs::create_dir_all(root).unwrap();
    }
    write_skill(&dsh, "deploy", "the dsh one");
    write_skill(&agents, "deploy", "the shared one");
    write_skill(&personal, "deploy", "the personal one");

    let manager = SkillsManager::with_roots(vec![
        project_root(&dsh),
        project_agents_root(&agents),
        user_root(&personal),
    ]);
    let (skills, problems) = manager.load_reporting();
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].summary(), "the dsh one");
    assert_eq!(
        problems
            .iter()
            .filter(|problem| problem.problem.contains("shadowed"))
            .count(),
        2
    );
}

/// Grouping by scope rendered a project's skills once per project root.
#[test]
fn an_agents_skill_is_listed_in_its_own_block() {
    let dir = tempdir().unwrap();
    let dsh = dir.path().join(".dsh/skills");
    let agents = dir.path().join(".agents/skills");
    std::fs::create_dir_all(&dsh).unwrap();
    std::fs::create_dir_all(&agents).unwrap();
    write_skill(&dsh, "deploy", "the dsh one");
    write_skill(&agents, "review", "the shared one");

    clear_skills_fragment_cache();
    let fragment =
        SkillsManager::with_roots(vec![project_root(&dsh), project_agents_root(&agents)])
            .get_system_prompt_fragment();

    assert_eq!(fragment.matches("the dsh one").count(), 1, "{fragment}");
    assert_eq!(fragment.matches("the shared one").count(), 1, "{fragment}");
    // One heading per root, and each root's skills under only its own.
    assert_eq!(
        fragment.matches("\nProject skills (").count(),
        1,
        "{fragment}"
    );
    assert_eq!(
        fragment.matches("\nShared project skills (").count(),
        1,
        "{fragment}"
    );
}

/// A repository is free to symlink one at the other; that is one set of
/// files, so it must not be listed - or asked about - twice.
#[test]
fn an_agents_root_symlinked_to_the_dsh_root_is_deduplicated() {
    let dir = tempdir().unwrap();
    let project = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::create_dir_all(project.join(".git")).unwrap();
    std::fs::create_dir_all(project.join(".dsh/skills")).unwrap();
    std::fs::create_dir_all(project.join(".agents")).unwrap();
    std::os::unix::fs::symlink(project.join(".dsh/skills"), project.join(".agents/skills"))
        .unwrap();

    let roots = skill_roots(Some(&project), true);
    let project_roots = roots
        .iter()
        .filter(|root| root.scope == SkillScope::Project)
        .count();
    assert_eq!(project_roots, 1, "{roots:?}");
}

/// The reason `~/.agents/skills` is not a fourth root: pointing the
/// personal one at it already works, and keeps the one trust story.
#[test]
fn a_personal_root_that_is_a_symlink_loads_the_skills_behind_it() {
    let dir = tempdir().unwrap();
    let shared = dir.path().join("shared");
    std::fs::create_dir_all(&shared).unwrap();
    write_skill(&shared, "portable", "works in any agent");
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&shared, &link).unwrap();

    let skills = SkillsManager::with_roots(vec![user_root(&link)]).load_skills();
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "portable");
}

/// Another tool's frontmatter carries keys this parser does not read.
#[test]
fn an_unknown_frontmatter_key_does_not_stop_a_skill_loading() {
    let dir = tempdir().unwrap();
    let agents = dir.path().join(".agents/skills/review");
    std::fs::create_dir_all(&agents).unwrap();
    fs::write(
            agents.join("SKILL.md"),
            "---\nname: review\ndescription: Use when reviewing.\nallowed-tools: [Bash, Read]\nlicense: MIT\n---\n# Review\n",
        )
        .unwrap();

    let root = dir.path().join(".agents/skills");
    let (skills, problems) =
        SkillsManager::with_roots(vec![project_agents_root(&root)]).load_reporting();
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].summary(), "Use when reviewing.");
    assert!(problems.is_empty(), "{problems:?}");
}
