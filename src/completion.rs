use crate::dirs::is_dir;
use crate::frecency::ItemStats;
use crate::lisp;
use crate::lisp::Value;
use anyhow::Result;
use regex::Regex;
use skim::prelude::*;
use skim::{Skim, SkimItemReceiver, SkimItemSender};
use std::fs::read_dir;
use std::path::PathBuf;
use std::{process::Command, sync::Arc};
use tracing::debug;

#[derive(Debug, Clone)]
pub struct AutoComplete {
    pub target: String,
    pub cmd: Option<String>,
    pub func: Option<Value>,
    pub candidates: Option<Vec<String>>,
}

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
        let item = ItemStats::new(input, 0.0, 0.0);

        self.input = if input.is_empty() {
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
    Basic(String),
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
    } else if let Some(dir) = parent {
        if dir.display().to_string().is_empty() {
            // current dir
            path_completion_path(PathBuf::from("."))?
        } else {
            path_completion_path(PathBuf::from(dir))?
        }
    } else {
        path_completion()?
    };

    for cand in paths.iter() {
        if let Candidate::Path(ref path) = cand {
            let path_str = path.to_string();
            if path.starts_with(&search) {
                return Ok(Some(path_str));
            }

            if let Ok(striped) = PathBuf::from(path).strip_prefix("./") {
                let striped_str = striped.display().to_string();
                if striped_str.starts_with(&search) {
                    return Ok(Some(path_str[2..].to_string()));
                }
            }
        }
    }
    Ok(None)
}

fn path_is_dir(path: &PathBuf) -> Result<bool> {
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

    for entry in dir.flatten() {
        let entry_path = entry.path();
        let is_dir = path_is_dir(&entry_path)?;
        if expand {
            if let Ok(part) = entry_path.strip_prefix(&home) {
                let mut pb = PathBuf::new();
                pb.push("~/");
                pb.push(part);
                let mut path = pb.display().to_string();
                if is_dir {
                    path += "/";
                }
                files.push(Candidate::Path(path));
            }
        } else {
            let mut path = entry_path.display().to_string();
            if is_dir {
                path += "/";
            }
            files.push(Candidate::Path(path));
        }
    }
    files.sort();
    Ok(files)
}

impl SkimItem for Candidate {
    fn text(&self) -> Cow<str> {
        match self {
            Candidate::Path(p) => Cow::Borrowed(p),
            Candidate::Basic(p) => Cow::Borrowed(p),
        }
    }

    fn output(&self) -> Cow<str> {
        match self {
            Candidate::Path(p) => Cow::Borrowed(p),
            Candidate::Basic(p) => Cow::Borrowed(p),
        }
    }
}

pub fn select_item(items: Vec<Candidate>, query: Option<&str>) -> Option<String> {
    let options = SkimOptionsBuilder::default()
        //        .height(Some("30%"))
        .bind(vec!["Enter:accept"])
        .query(query)
        .build()
        .unwrap();

    let (tx_item, rx_item): (SkimItemSender, SkimItemReceiver) = unbounded();
    for item in items {
        let _ = tx_item.send(Arc::new(item));
    }
    drop(tx_item);

    let selected = Skim::run_with(&options, Some(rx_item))
        .map(|out| match out.final_key {
            Key::Enter => out.selected_items,
            _ => Vec::new(),
        })
        .unwrap_or_else(Vec::new);

    if !selected.is_empty() {
        let val = selected[0].output().to_string();
        return Some(val);
    }

    None
}

pub fn completion_from_cmd(input: String, query: Option<&str>) -> Option<String> {
    debug!("{} ", &input);
    match Command::new("sh").arg("-c").arg(input).output() {
        Ok(output) => {
            if let Ok(out) = String::from_utf8(output.stdout) {
                let items: Vec<Candidate> = out
                    .split('\n')
                    // TODO filter
                    .map(|x| Candidate::Basic(x.trim().to_string()))
                    .collect();

                return select_item(items, query);
            }
            None
        }
        _ => None,
    }
}

pub fn input_completion(
    input: &str,
    lisp_engine: Rc<RefCell<lisp::LispEngine>>,
    query: Option<&str>,
) -> Option<String> {
    // TODO convert input
    let has = query.is_some();
    let environment = Rc::clone(&lisp_engine.borrow().shell_env);
    // 1. completion from autocomplete
    for compl in environment.borrow().autocompletion.iter() {
        let cmd_str = format!("{}", compl.target);

        // debug!("match cmd:'{}' in:'{}'", cmd_str, replace_space(input));
        if replace_space(input).starts_with(cmd_str.as_str()) {
            if let Some(func) = &compl.func {
                // run lisp func
                match lisp_engine.borrow().apply_func(func.to_owned(), vec![]) {
                    Ok(Value::List(list)) => {
                        let mut items: Vec<Candidate> = Vec::new();
                        for val in list.into_iter() {
                            items.push(Candidate::Basic(val.to_string()));
                        }
                        return select_item(items, query);
                    }
                    Ok(Value::String(str)) => {
                        return Some(str);
                    }
                    Err(err) => {
                        println!("{:?}", err);
                    }
                    _ => {}
                }
            } else if let Some(cmd) = &compl.cmd {
                // run command
                if let Some(val) = completion_from_cmd(cmd.to_string(), query) {
                    if val.starts_with('*') {
                        return Some(val[2..].to_string());
                    } else {
                        return Some(val);
                    }
                }
            } else if let Some(items) = &compl.candidates {
                let items: Vec<Candidate> = items
                    .iter()
                    .map(|x| Candidate::Basic(x.trim().to_string()))
                    .collect();
                return select_item(items, query);
            }
            return None;
        }
    }

    // 2 . try path completion
    if has && is_dir(query.unwrap()) {
        return list_files(query);
    }
    None
}

fn list_files(query: Option<&str>) -> Option<String> {
    completion_from_cmd(format!("ls -1 {}", query.unwrap()), None)
}

fn replace_space(s: &str) -> String {
    let re = Regex::new(r"\s+").unwrap();
    re.replace_all(s, "_").to_string()
}

#[cfg(test)]
mod test {

    use super::*;

    fn init() {
        tracing_subscriber::fmt::init();
    }

    #[test]
    fn test_completion() -> Result<()> {
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

    #[test]
    #[ignore]
    fn test_select_item() {
        let mut items: Vec<Candidate> = Vec::new();
        // items.push();
        items.push(Candidate::Basic("test1".to_string()));
        items.push(Candidate::Basic("test2".to_string()));

        let a = select_item(items, Some("test"));
        assert_eq!("test1", a.unwrap());
    }

    #[test]
    fn test_replace_space() {
        let a = replace_space("aa     bb");
        assert_eq!(a, "aa_bb")
    }
}
