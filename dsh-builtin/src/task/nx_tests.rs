//! Nx traversal contracts exercised through discovery and the process-local cache.
use super::*;
use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::symlink;
use tempfile::tempdir;

fn write_project(directory: &Path, target: &str) {
    fs::create_dir_all(directory).unwrap();
    fs::write(
        directory.join("project.json"),
        format!(r#"{{"name":"api","targets":{{"{target}":{{}}}}}}"#),
    )
    .unwrap();
}

fn names(root: &Path, runtime: &TaskDiscoveryRuntime) -> Vec<String> {
    list_tasks_in_dir(root, runtime)
        .unwrap()
        .into_iter()
        .map(|task| task.name)
        .collect()
}

#[test]
fn nx_cache_refreshes_at_every_discovered_depth() {
    for depth in 0..=4 {
        let dir = tempdir().unwrap();
        let runtime = TaskDiscoveryRuntime::new(Vec::new(), HashMap::new());
        fs::write(dir.path().join("nx.json"), "{}").unwrap();
        let project = (0..depth).fold(dir.path().to_path_buf(), |path, _| path.join("child"));
        write_project(&project, "build");
        assert_eq!(names(dir.path(), &runtime), ["api:build"]);
        // A different length guarantees a metadata change without sleeping or
        // depending on the filesystem timestamp resolution. Reuse the cache.
        write_project(&project, "newtask");
        assert_eq!(
            names(dir.path(), &runtime),
            ["api:newtask"],
            "depth {depth}"
        );
        fs::remove_file(project.join("project.json")).unwrap();
        assert!(names(dir.path(), &runtime).is_empty(), "removed at {depth}");
        write_project(&project, "added");
        assert_eq!(
            names(dir.path(), &runtime),
            ["api:added"],
            "added at {depth}"
        );
    }
}

#[test]
fn nx_traversal_ignores_directory_symlinks_and_cycles() {
    let dir = tempdir().unwrap();
    let external = tempdir().unwrap();
    let runtime = TaskDiscoveryRuntime::new(Vec::new(), HashMap::new());
    fs::write(dir.path().join("nx.json"), "{}").unwrap();
    write_project(external.path(), "build");
    symlink(external.path(), dir.path().join("external")).unwrap();
    assert!(names(dir.path(), &runtime).is_empty());
    let before = discovery_signature(dir.path(), None, &runtime);
    write_project(external.path(), "newtask");
    assert_eq!(before, discovery_signature(dir.path(), None, &runtime));

    write_project(&dir.path().join("apps/api"), "test");
    symlink(dir.path(), dir.path().join("apps/cycle")).unwrap();
    assert_eq!(names(dir.path(), &runtime), ["api:test"]);
    let definitions = project_context::detect_task_names_in_dir(dir.path()).unwrap();
    assert_eq!(definitions.len(), 1, "cycle must not duplicate projects");
}

#[test]
fn nx_traversal_shares_depth_and_directory_exclusions() {
    for excluded in [
        "a/b/c/d/e",
        ".hidden",
        ".git",
        "node_modules",
        "target",
        "dist",
        "build",
        "apps/.hidden",
    ] {
        let dir = tempdir().unwrap();
        let runtime = TaskDiscoveryRuntime::new(Vec::new(), HashMap::new());
        fs::write(dir.path().join("nx.json"), "{}").unwrap();
        write_project(&dir.path().join("apps/api"), "build");
        let ignored = dir.path().join(excluded);
        write_project(&ignored, "ignored");
        assert_eq!(names(dir.path(), &runtime), ["api:build"], "{excluded}");
        let before = discovery_signature(dir.path(), None, &runtime);
        write_project(&ignored, "still_ignored");
        assert_eq!(
            before,
            discovery_signature(dir.path(), None, &runtime),
            "{excluded}"
        );
    }
}

#[test]
fn nx_cycle_does_not_create_an_alias_for_an_unnamed_root_project() {
    let dir = tempdir().unwrap();
    let runtime = TaskDiscoveryRuntime::new(Vec::new(), HashMap::new());
    fs::write(
        dir.path().join("project.json"),
        r#"{"targets":{"test":{}}}"#,
    )
    .unwrap();
    symlink(dir.path(), dir.path().join("cycle")).unwrap();
    let expected = format!("{}:test", dir.path().file_name().unwrap().to_str().unwrap());
    assert_eq!(names(dir.path(), &runtime), [expected]);
}

#[test]
fn nx_linked_marker_file_remains_discoverable_and_refreshes() {
    let dir = tempdir().unwrap();
    let external = tempdir().unwrap();
    let runtime = TaskDiscoveryRuntime::new(Vec::new(), HashMap::new());
    fs::write(dir.path().join("nx.json"), "{}").unwrap();
    let app = dir.path().join("apps/api");
    fs::create_dir_all(&app).unwrap();
    write_project(external.path(), "build");
    symlink(
        external.path().join("project.json"),
        app.join("project.json"),
    )
    .unwrap();
    assert_eq!(names(dir.path(), &runtime), ["api:build"]);
    write_project(external.path(), "newtask");
    assert_eq!(names(dir.path(), &runtime), ["api:newtask"]);
}
