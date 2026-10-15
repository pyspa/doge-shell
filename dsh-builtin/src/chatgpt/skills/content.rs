//! Bounded skill metadata and explicit-body reads with canonical root checks.
use super::*;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;

pub(super) const INDEX_BYTES: usize = 8 * 1024;
pub(super) const MENTION_BYTES: usize = 8 * 1024;

fn open_in_root(path: &Path, root: &Path) -> Result<std::fs::File> {
    let root = std::fs::canonicalize(root)?;
    let path = std::fs::canonicalize(path)?;
    if !path.starts_with(&root) {
        anyhow::bail!("skill instruction links outside the skills directory");
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(&path)?;
    if !file.metadata()?.is_file() {
        anyhow::bail!("skill instruction is not a regular file");
    }
    Ok(file)
}

/// Listing never reads an entire body. Retain the header and enough body for
/// legacy no-description fallback; later reads go through the paged file tool.
pub(super) fn index(path: &Path, root: &Path) -> Result<String> {
    let mut bytes = Vec::new();
    open_in_root(path, root)?
        .take((INDEX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > INDEX_BYTES {
        bytes.truncate(INDEX_BYTES);
        match std::str::from_utf8(&bytes) {
            Ok(_) => (),
            Err(err) if err.error_len().is_none() => {
                bytes.truncate(err.valid_up_to());
            }
            Err(err) => return Err(err.into()),
        }
    }
    let text = String::from_utf8(bytes)?;
    if text.lines().next().is_some_and(|line| line.trim() == "---")
        && split_frontmatter(&text).0.is_none()
    {
        anyhow::bail!("missing closing frontmatter delimiter within the 8 KiB metadata limit");
    }
    Ok(text)
}

pub(super) fn body(skill: &Skill) -> Result<String> {
    let mut body = Vec::new();
    open_in_root(&skill.instruction_file(), skill.root())?
        .take((MENTION_BYTES + 1) as u64)
        .read_to_end(&mut body)?;
    if body.len() > MENTION_BYTES {
        anyhow::bail!(
            "body exceeds the 8 KiB inline limit; use paged read_file on `{}`",
            skill.instruction_path()
        );
    }
    Ok(String::from_utf8(body)?)
}
