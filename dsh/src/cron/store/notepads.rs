//! Keep each note attached to its job across renames and name reuse.

use super::{DirBuilder, DirBuilderExt, HashMap, Path, PathBuf, Result, slug};
use rusqlite::Connection;
use std::{fs, io::ErrorKind, os::unix::fs::MetadataExt};

pub(super) fn path(root: &Path, id: i64) -> PathBuf {
    root.join("notepad").join("by-id").join(format!("{id}.md"))
}

pub(super) fn migrate(connection: &Connection, root: &Path) -> Result<()> {
    let directory = root.join("notepad");
    if !directory.try_exists()? {
        return Ok(());
    }
    // Filesystem identity also detects aliases on case-insensitive volumes.
    let mut owners = HashMap::<_, Vec<_>>::new();
    let mut statement = connection.prepare("SELECT id, name FROM jobs")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let name: String = row.get(1)?;
        let old = directory.join(format!("{}.md", slug(&name)));
        match fs::symlink_metadata(&old) {
            Ok(meta) if meta.is_file() => {
                owners
                    .entry((meta.dev(), meta.ino()))
                    .or_default()
                    .push((id, old));
            }
            Ok(_) => {}
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            // A valid job name can exceed the old single-component filename limit.
            Err(err) if err.raw_os_error() == Some(nix::libc::ENAMETOOLONG) => {}
            Err(err) => return Err(err.into()),
        }
    }

    for entries in owners.into_values() {
        let [(id, source)] = entries.as_slice() else {
            tracing::warn!(
                "cron: multiple jobs share legacy notepad {}; keeping it in place",
                entries[0].1.display()
            );
            continue;
        };
        let target = path(root, *id);
        DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(directory.join("by-id"))?;
        // A completed rename is safe to retry after an interrupted DB upgrade.
        // Preserve both files if an ID-based note already exists.
        match fs::symlink_metadata(&target) {
            Ok(_) => tracing::warn!(
                "cron: {} already exists; keeping {}",
                target.display(),
                source.display()
            ),
            Err(err) if err.kind() == ErrorKind::NotFound => fs::rename(source, target)?,
            Err(err) => return Err(err.into()),
        }
    }
    Ok(())
}
