//! Shared "write via a temp file, then persist" helper.
//!
//! `comp-gen` (`completion_generation.rs`) and `output-gen` (`output_gen.rs`)
//! each write a generated JSON file into a config directory that may not
//! exist yet, and both want the same atomicity and the same `--force` vs.
//! "refuse to clobber" choice. This is that one implementation, parameterised
//! only by the noun used in error messages (`"completion"`, `"output-schema"`,
//! ...) so each caller's errors still read naturally.

use anyhow::{Context as _, Result};
use std::fs;
use std::io::Write;
use std::path::Path;

/// Writes `content` to `output_path` atomically: a temp file in the same
/// directory, fsynced, then renamed into place. Without `force`, refuses to
/// overwrite an existing file at `output_path`.
pub fn write_atomic(output_path: &Path, content: &str, force: bool, kind: &str) -> Result<()> {
    let parent = output_path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)
        .with_context(|| format!("Failed to create {kind} directory '{}'", parent.display()))?;

    let mut temporary = tempfile::NamedTempFile::new_in(parent).with_context(|| {
        format!(
            "Failed to create a temporary {kind} file in '{}'",
            parent.display()
        )
    })?;
    temporary
        .write_all(content.as_bytes())
        .with_context(|| format!("Failed to write temporary {kind} file"))?;
    temporary
        .as_file()
        .sync_all()
        .with_context(|| format!("Failed to flush temporary {kind} file"))?;

    if force {
        temporary
            .persist(output_path)
            .map_err(|error| error.error)?;
    } else {
        temporary
            .persist_noclobber(output_path)
            .map_err(|error| error.error)
            .with_context(|| {
                format!(
                    "{kind} file already exists: {}. Use --force to overwrite",
                    output_path.display()
                )
            })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_force_a_second_write_is_refused_and_the_first_survives() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("foo.txt");

        write_atomic(&path, "first", false, "test").unwrap();
        assert!(write_atomic(&path, "second", false, "test").is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "first");

        write_atomic(&path, "second", true, "test").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "second");
    }

    #[test]
    fn creates_missing_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("foo.txt");

        write_atomic(&path, "content", false, "test").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "content");
    }
}
