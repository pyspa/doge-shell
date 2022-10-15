use anyhow::Result;
use log::debug;
use std::fs::read_dir;
use std::path::PathBuf;

#[derive(Debug, PartialEq, PartialOrd, Eq, Ord)]
pub enum Candidate {
    Path(PathBuf),
}

pub fn path_completion_first(input: &str) -> Result<Option<String>> {
    let pbuf = PathBuf::from(input);
    let absolute = pbuf.is_absolute();
    let file_name = pbuf.file_name();
    if file_name.is_none() {
        return Ok(None);
    }
    let parent = pbuf.parent();
    let search = input.to_string();

    let paths = if absolute {
        let dir = if let Some(f) = parent {
            f.to_string_lossy().to_string()
        } else {
            input.to_string()
        };
        path_completion_path(PathBuf::from(dir))?
    } else {
        if let Some(dir) = parent {
            if dir.display().to_string().is_empty() {
                // current dir
                path_completion_path(PathBuf::from("."))?
            } else {
                path_completion_path(PathBuf::from(dir))?
            }
        } else {
            path_completion()?
        }
    };

    for cand in paths.iter() {
        match cand {
            Candidate::Path(ref path) => {
                if path.display().to_string().starts_with(&search) {
                    let is_dir = is_dir(path)?;
                    let mut file = path.display().to_string();
                    if is_dir {
                        file = file + "/";
                    }
                    return Ok(Some(file));
                }
                match path.strip_prefix("./") {
                    Ok(ref path) => {
                        if path.display().to_string().starts_with(&search) {
                            let is_dir = is_dir(&path.to_path_buf())?;
                            let mut file = path.display().to_string();
                            if is_dir {
                                file = file + "/";
                            }
                            return Ok(Some(file));
                        }
                    }
                    Err(_) => {}
                }
            }
            _ => {}
        }
    }
    Ok(None)
}

fn is_dir(path: &PathBuf) -> Result<bool> {
    if let Ok(mut metadata) = path.metadata() {
        if metadata.is_symlink() {
            let link = std::fs::read_link(path)?;
            let relative = link.is_relative();
            if relative {
                metadata = path.join(&link).metadata()?;
            }
        }
        Ok(metadata.is_dir())
    } else {
        Ok(false)
    }
}

pub fn path_completion() -> Result<Vec<Candidate>> {
    let current_dir = std::env::current_dir()?;
    path_completion_path(current_dir)
}

pub fn path_completion_path(path: PathBuf) -> Result<Vec<Candidate>> {
    let dir = read_dir(&path)?;
    let mut files: Vec<Candidate> = Vec::new();

    for entry in dir.into_iter() {
        if let Ok(entry) = entry {
            files.push(Candidate::Path(entry.path()));
        }
    }
    files.sort();
    Ok(files)
}

#[cfg(test)]
mod test {

    use super::*;

    #[test]
    fn completion() -> Result<()> {
        let _ = env_logger::try_init();

        let p = path_completion_first(".")?;
        assert_eq!(None, p);

        let p = path_completion_first("./")?;
        assert_eq!(None, p);

        let p = path_completion_first("./sr")?;
        assert_eq!(Some("./src/".to_string()), p);

        let p = path_completion_first("sr")?;
        assert_eq!(Some("src/".to_string()), p);

        let p = path_completion_first("./sr")?;
        assert_eq!(Some("./src/".to_string()), p);

        let p = path_completion_first("src/b")?;
        assert_eq!(Some("src/builtin/".to_string()), p);

        let p = path_completion_first("/")?;
        assert_eq!(None, p);

        let p = path_completion_first("/s")?;
        assert_eq!(Some("/sbin/".to_string()), p);

        let p = path_completion_first("/usr/b")?;
        assert_eq!(Some("/usr/bin/".to_string()), p);

        Ok(())
    }
}
