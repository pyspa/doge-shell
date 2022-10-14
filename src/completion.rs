use anyhow::Result;
use std::fs::read_dir;
use std::path::PathBuf;

pub enum Candidate {
    Path(String),
}

pub fn path_completion_first(input: &str) -> Result<Option<String>> {
    let name = if let Some(f) = PathBuf::from(input).file_name() {
        f.to_string_lossy().to_string()
    } else {
        input.to_string()
    };

    let paths = path_completion()?;

    for cand in paths.iter() {
        match cand {
            Candidate::Path(ref path) if path.starts_with(&name) && input.starts_with("./") => {
                return Ok(Some("./".to_owned() + &path.clone()));
            }
            Candidate::Path(ref path) if path.starts_with(&name) && input.starts_with("/") => {
                return Ok(Some("/".to_owned() + &path.clone()));
            }
            Candidate::Path(ref path) if path.starts_with(&name) => {
                return Ok(Some(path.clone()));
            }
            _ => {
                //
            }
        }
    }
    Ok(None)
}

pub fn path_completion() -> Result<Vec<Candidate>> {
    let current_dir = std::env::current_dir()?;
    path_completion_path(current_dir)
}

pub fn path_completion_path(path: PathBuf) -> Result<Vec<Candidate>> {
    let dir = read_dir(path)?;
    let mut files: Vec<Candidate> = Vec::new();

    for entry in dir.into_iter() {
        if let Ok(entry) = entry {
            let dir = if let Ok(metadata) = entry.metadata() {
                metadata.is_dir()
            } else {
                false
            };

            let mut file = entry.file_name().to_string_lossy().to_string();
            if dir {
                file = file + "/";
            }

            files.push(Candidate::Path(file));
        }
    }

    Ok(files)
}
