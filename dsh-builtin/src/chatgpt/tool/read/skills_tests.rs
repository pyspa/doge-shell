use super::*;
use crate::test_support::TestShellProxy;
use tempfile::tempdir;

fn setup() -> (tempfile::TempDir, TestShellProxy, std::path::PathBuf) {
    let dir = tempdir().unwrap();
    fs::create_dir(dir.path().join(".git")).unwrap();
    let skill = dir.path().join(".dogesh/skills/demo");
    fs::create_dir_all(skill.join("references")).unwrap();
    fs::write(
        skill.join("SKILL.md"),
        "---\nname: demo\ndescription: Use for the fixture\n---\nRead references/detail.md.\n",
    )
    .unwrap();
    fs::write(skill.join("references/detail.md"), "needed detail").unwrap();
    let proxy = TestShellProxy {
        current_dir: dir.path().to_path_buf(),
        ..Default::default()
    };
    (dir, proxy, skill)
}

#[test]
fn model_can_read_the_listed_body_and_relative_resource_with_existing_tool() {
    let (_dir, mut proxy, skill) = setup();
    let instruction = skill.join("SKILL.md");
    let body = run(&json!({"path":instruction}).to_string(), &mut proxy).unwrap();
    assert!(body.contains("Read references/detail.md"));
    let resource = run(
        &json!({"path":"references/detail.md","skill_path":instruction}).to_string(),
        &mut proxy,
    )
    .unwrap();
    assert!(resource.contains("needed detail"));
    assert!(crate::chatgpt::skills::usage::read_this_turn().contains(&skill));
}

#[test]
fn relative_resource_never_escapes_its_bundle_or_expands_shell_variables() {
    let (dir, mut proxy, skill) = setup();
    fs::write(dir.path().join("outside.md"), "must not escape").unwrap();
    for path in ["../../../outside.md", "/etc/hosts", "~/file", "$HOME/file"] {
        let result = run(
            &json!({"path":path,"skill_path":skill.join("SKILL.md")}).to_string(),
            &mut proxy,
        );
        assert!(result.is_err(), "{path}: {result:?}");
    }
    std::os::unix::fs::symlink(
        dir.path().join("outside.md"),
        skill.join("references/escape.md"),
    )
    .unwrap();
    let result = run(
        &json!({"path":"references/escape.md","skill_path":skill.join("SKILL.md")}).to_string(),
        &mut proxy,
    );
    assert!(result.unwrap_err().contains("escapes"));
}

#[test]
fn missing_resource_and_invalid_bases_are_explicit_errors() {
    let (dir, mut proxy, skill) = setup();
    let missing = run(
        &json!({"path":"references/missing.md","skill_path":skill.join("SKILL.md")}).to_string(),
        &mut proxy,
    );
    assert!(missing.unwrap_err().contains("failed to read"));
    for base in [
        dir.path().join("README.md"),
        skill.join("references/detail.md"),
    ] {
        fs::write(&base, "not an instruction base").unwrap();
        assert!(
            run(
                &json!({"path":"x","skill_path":base}).to_string(),
                &mut proxy
            )
            .is_err()
        );
    }
}

#[test]
fn paged_relative_resource_continues_with_an_absolute_path() {
    let (_dir, mut proxy, skill) = setup();
    fs::write(skill.join("references/detail.md"), "x\n".repeat(500)).unwrap();
    let first = run(
        &json!({"path":"references/detail.md","skill_path":skill.join("SKILL.md")}).to_string(),
        &mut proxy,
    )
    .unwrap();
    let canonical = fs::canonicalize(skill.join("references/detail.md")).unwrap();
    assert!(first.contains(&format!(
        "read_file(path=\"{}\", offset=401)",
        canonical.display()
    )));
    let next = run(
        &json!({"path":canonical,"offset":401}).to_string(),
        &mut proxy,
    )
    .unwrap();
    assert!(next.contains("lines 401-500 of 500"));
}

#[test]
fn in_bundle_instruction_symlinks_work_but_cross_bundle_links_do_not() {
    let (_dir, mut proxy, skill) = setup();
    fs::rename(skill.join("SKILL.md"), skill.join("instructions.md")).unwrap();
    std::os::unix::fs::symlink("instructions.md", skill.join("SKILL.md")).unwrap();
    let result = run(
        &json!({"path":"references/detail.md","skill_path":skill.join("SKILL.md")}).to_string(),
        &mut proxy,
    )
    .unwrap();
    assert!(result.contains("needed detail"));
    let sibling = skill.parent().unwrap().join("other");
    fs::create_dir(&sibling).unwrap();
    fs::write(sibling.join("SKILL.md"), "other").unwrap();
    fs::remove_file(skill.join("SKILL.md")).unwrap();
    std::os::unix::fs::symlink(sibling.join("SKILL.md"), skill.join("SKILL.md")).unwrap();
    assert!(
        run(
            &json!({"path":"references/detail.md","skill_path":skill.join("SKILL.md")}).to_string(),
            &mut proxy
        )
        .unwrap_err()
        .contains("escapes")
    );
}
