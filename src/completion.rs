use crate::frecency::ItemStats;
use anyhow::Result;
use log::debug;
use std::fs::read_dir;
use std::path::PathBuf;

#[derive(Debug)]
pub struct Completion {
    pub input: Option<String>,
    completions: Vec<ItemStats>,
    current_index: usize,
}

impl Completion {
    pub fn new() -> Self {
        Completion {
            input: None,
            current_index: 0,
            completions: Vec::new(),
        }
    }

    pub fn is_changed(&self, word: &str) -> bool {
        if let Some(input) = &self.input {
            input != word
        } else {
            !word.is_empty()
        }
    }

    pub fn clear(&mut self) {
        self.input = None;
        self.current_index = 0;
        self.completions = Vec::new();
    }

    pub fn completion_mode(&self) -> bool {
        !self.completions.is_empty()
    }

    pub fn set_completions(&mut self, input: &str, comps: Vec<ItemStats>) {
        let item = ItemStats::new(&input.to_string(), 0.0, 0.0);

        self.input = if input == "" {
            None
        } else {
            Some(input.to_string())
        };
        self.completions = comps;
        self.completions.insert(0, item);
        self.current_index = 0;
    }

    pub fn backward(&mut self) -> Option<ItemStats> {
        if self.completions.is_empty() {
            return None;
        }

        if self.completions.len() - 1 > self.current_index {
            self.current_index += 1;
            Some(self.completions[self.current_index as usize].clone())
        } else {
            None
        }
    }

    pub fn forward(&mut self) -> Option<ItemStats> {
        if self.current_index > 0 {
            self.current_index -= 1;
            Some(self.completions[self.current_index as usize].clone())
        } else {
            None
        }
    }
}

#[derive(Debug, PartialEq, PartialOrd, Eq, Ord)]
pub enum Candidate {
    Path(String),
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
                let path_str = path.to_string();
                if path.starts_with(&search) {
                    return Ok(Some(path_str));
                }

                match PathBuf::from(path).strip_prefix("./") {
                    Ok(ref striped) => {
                        let striped_str = striped.display().to_string();
                        if striped_str.starts_with(&search) {
                            return Ok(Some(path_str[2..].to_string()));
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
    let path_str = path.display().to_string();
    let exp_str = shellexpand::tilde(&path_str).to_string();
    let expand = path_str != exp_str;

    let home = dirs::home_dir().unwrap();
    let path = PathBuf::from(exp_str);

    let dir = read_dir(&path)?;
    let mut files: Vec<Candidate> = Vec::new();

    for entry in dir.into_iter() {
        if let Ok(entry) = entry {
            let entry_path = entry.path();
            let is_dir = is_dir(&entry_path)?;
            if expand {
                if let Ok(part) = entry_path.strip_prefix(&home) {
                    let mut pb = PathBuf::new();
                    pb.push("~/");
                    pb.push(part);
                    let mut path = pb.display().to_string();
                    if is_dir {
                        path = path + "/";
                    }
                    files.push(Candidate::Path(path));
                }
            } else {
                let mut path = entry_path.display().to_string();
                if is_dir {
                    path = path + "/";
                }
                files.push(Candidate::Path(path));
            }
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

        let p = path_completion_first("src/b")?;
        assert_eq!(Some("src/builtin/".to_string()), p);

        let p = path_completion_first("/")?;
        assert_eq!(None, p);

        let p = path_completion_first("/s")?;
        assert_eq!(Some("/sbin/".to_string()), p);

        let p = path_completion_first("/usr/b")?;
        assert_eq!(Some("/usr/bin/".to_string()), p);

        let p = path_completion_first("~/.lo")?;
        assert_eq!(Some("~/.local/".to_string()), p);

        let p = path_completion_first("~/.config/gi")?;
        assert_eq!(Some("~/.config/git/".to_string()), p);

        Ok(())
    }
}
