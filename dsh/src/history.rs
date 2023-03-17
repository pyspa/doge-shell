use crate::environment;
use anyhow::Context as _;
use anyhow::Result;
use chrono::Local;
use dsh_frecency::{read_store, write_store, FrecencyStore, ItemStats, SortMethod};
use easy_reader::EasyReader;
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Debug)]
struct Entry {
    entry: String,
    when: i64,
}

#[derive(Debug)]
pub struct History {
    pub path: Option<String>,
    open_file: Option<File>,
    histories: Vec<Entry>,
    size: usize,
    current_index: usize,
    pub search_word: Option<String>,
}

impl History {
    pub fn new() -> Self {
        History {
            path: None,
            open_file: None,
            histories: Vec::new(),
            size: 10000,
            current_index: 0,
            search_word: None,
        }
    }

    pub fn from_file(name: &str) -> Self {
        let file_path = environment::get_data_file(name).unwrap();
        let path = file_path.into_os_string().into_string().unwrap();

        History {
            path: Some(path),
            open_file: None,
            histories: Vec::new(),
            size: 10000,
            current_index: 0,
            search_word: None,
        }
    }

    fn get(&self, index: usize) -> Option<String> {
        if index < self.histories.len() {
            let entry = &self.histories[index].entry;
            Some(entry.to_string())
        } else {
            None
        }
    }

    pub fn back(&mut self) -> Option<String> {
        if self.current_index > 0 {
            self.current_index -= 1;
            match &self.search_word {
                Some(word) => {
                    while let Some(entry) = self.get(self.current_index) {
                        if entry.contains(word) {
                            return Some(entry);
                        }
                        self.current_index -= 1;
                        if self.current_index == 0 {
                            break;
                        }
                    }
                    None
                }
                None => self.get(self.current_index),
            }
        } else {
            None
        }
    }

    pub fn forward(&mut self) -> Option<String> {
        if self.histories.len() - 1 > self.current_index {
            self.current_index += 1;
            match &self.search_word {
                Some(word) => {
                    while let Some(entry) = self.get(self.current_index) {
                        if entry.contains(word) {
                            return Some(entry);
                        }
                        self.current_index += 1;
                    }
                    None
                }
                None => self.get(self.current_index),
            }
        } else {
            None
        }
    }

    pub fn reset_index(&mut self) {
        self.current_index = self.histories.len();
    }

    pub fn load(&mut self) -> Result<usize> {
        if self.path.is_none() {
            return Ok(0);
        }
        self.open()?;
        if let Some(file) = &self.open_file {
            let mut reader = EasyReader::new(file)?;
            while let Some(line) = reader.next_line()? {
                if let Ok::<Entry, _>(e) = serde_json::from_str(&line) {
                    self.histories.push(e);
                }
            }
            self.current_index = self.histories.len();
        }
        self.close()?;
        Ok(self.histories.len())
    }

    fn open(&mut self) -> Result<&mut History> {
        if self.open_file.is_some() {
            Ok(self)
        } else if let Some(ref path) = self.path {
            let file = OpenOptions::new()
                .read(true)
                .open(path)
                .context("failed open file")?;

            self.open_file = Some(file);
            Ok(self)
        } else {
            Ok(self)
        }
    }

    fn close(&mut self) -> Result<()> {
        self.open_file = None;
        Ok(())
    }

    pub fn write_history(&mut self, history: &str) -> Result<()> {
        // TODO file lock
        if let Some(ref path) = self.path {
            let mut history_file = OpenOptions::new()
                .write(true)
                .create(true)
                .append(true)
                .open(path)
                .context("failed open file")?;

            let now = Local::now();
            let entry = Entry {
                entry: history.to_string(),
                when: now.timestamp(),
            };

            let json = serde_json::to_string(&entry)? + "\n";
            let _size = history_file
                .write(json.as_bytes())
                .context("failed write entry")?;
            history_file.flush().context("failed flush")?;

            self.histories.push(entry);
            self.reset_index();

            Ok(())
        } else {
            Ok(())
        }
    }

    pub fn search_first(&self, word: &str) -> Option<String> {
        for hist in self.histories.iter().rev() {
            if hist.entry.starts_with(word) {
                return Some(hist.entry.clone());
            }
        }
        None
    }
}

pub struct FrecencyHistory {
    pub path: Option<PathBuf>,
    store: Option<FrecencyStore>,
    histories: Option<Vec<ItemStats>>,
    current_index: usize,
    pub search_word: Option<String>,
    prev_search_word: String,
    matcher: SkimMatcherV2,
}

impl std::fmt::Debug for FrecencyHistory {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::result::Result<(), std::fmt::Error> {
        f.debug_struct("FrecencyHistory")
            .field("path", &self.path)
            .field("current_index", &self.current_index)
            .field("search_word", &self.search_word)
            .field("prev_search_word", &self.prev_search_word)
            .finish()
    }
}

impl FrecencyHistory {
    pub fn new() -> Self {
        let matcher = SkimMatcherV2::default();
        FrecencyHistory {
            path: None,
            store: None,
            histories: None,
            current_index: 0,
            search_word: None,
            prev_search_word: "".to_string(),
            matcher,
        }
    }

    pub fn from_file(name: &str) -> Result<Self> {
        let file_path = environment::get_data_file(name)?;
        let matcher = SkimMatcherV2::default();

        let store = match read_store(&file_path) {
            Ok(store) => store,
            Err(_) => FrecencyStore::default(),
        };

        let f = FrecencyHistory {
            path: Some(file_path),
            store: Some(store),
            histories: None,
            current_index: 0,
            search_word: None,
            prev_search_word: "".to_string(),
            matcher,
        };
        Ok(f)
    }

    fn is_change_search_word(&self) -> bool {
        if let Some(search_word) = &self.search_word {
            return search_word != &self.prev_search_word;
        }
        false
    }

    pub fn set_search_word(&mut self, word: String) {
        if let Some(ref prev) = self.search_word {
            self.prev_search_word = prev.clone();
        }
        if word.is_empty() {
            self.search_word = None;
        } else {
            self.search_word = Some(word);
        }
    }

    fn get(&self, index: usize) -> Option<ItemStats> {
        if let Some(ref histories) = self.histories {
            if index < histories.len() {
                let item = &histories[index];
                Some(item.clone())
            } else {
                None
            }
        } else {
            None
        }
    }

    pub fn backward(&mut self) -> Option<ItemStats> {
        if self.histories.is_none() {
            self.current_index = 0;
            match &self.search_word {
                Some(word) => {
                    self.histories = Some(self.sort_by_match(word));
                }
                None => {
                    self.histories = Some(self.sorted(&SortMethod::Recent));
                }
            }
        }

        if let Some(ref histories) = self.histories {
            if histories.len() > self.current_index {
                self.current_index += 1;
                self.get(self.current_index - 1)
            } else {
                None
            }
        } else {
            None
        }
    }

    pub fn forward(&mut self) -> Option<ItemStats> {
        if self.histories.is_none() {
            self.current_index = 0;
            match &self.search_word {
                Some(word) => {
                    self.histories = Some(self.sort_by_match(word));
                }
                None => {
                    self.histories = Some(self.sorted(&SortMethod::Recent));
                }
            }
        }

        if self.current_index > 1 {
            self.current_index -= 1;
            self.get(self.current_index - 1)
        } else {
            None
        }
    }

    pub fn reset_index(&mut self) {
        self.current_index = 0;
        self.histories = None;
    }

    pub fn write_history(&mut self, history: &str) -> Result<()> {
        self.add(history);
        self.save()
    }

    pub fn save(&mut self) -> Result<()> {
        let file_path = self.path.clone().unwrap();
        if let Some(ref store) = self.store {
            write_store(store, &file_path)
        } else {
            Ok(())
        }
    }

    pub fn add(&mut self, history: &str) {
        if let Some(ref mut store) = self.store {
            store.add(history);
        }
    }

    pub fn search_fuzzy_first(&self, pattern: &str) -> Option<String> {
        let results = self.sort_by_match(pattern);
        if results.is_empty() {
            None
        } else {
            Some(results[0].item.clone())
        }
    }

    pub fn search_prefix(&self, pattern: &str) -> Option<String> {
        let results = self.sort_by_match(pattern);
        for res in results {
            if res.item.starts_with(pattern) {
                return Some(res.item);
            }
        }
        None
    }

    pub fn sort_by_match(&self, pattern: &str) -> Vec<ItemStats> {
        let mut results: Vec<ItemStats> = vec![];
        if let Some(ref store) = self.store {
            for item in store.items.iter() {
                if let Some((score, index)) = self.matcher.fuzzy_indices(&item.item, pattern) {
                    let mut item = item.clone();
                    item.match_score = score;
                    item.match_index = index;
                    results.push(item);
                }
            }
        }
        results.sort_by(|a, b| a.cmp_match_score(b).reverse());
        results
    }

    pub fn sorted(&self, sort_method: &SortMethod) -> Vec<ItemStats> {
        if let Some(ref store) = self.store {
            store.sorted(sort_method)
        } else {
            Vec::new()
        }
    }

    pub fn show_score(&self, pattern: &str) {
        for res in self.sort_by_match(pattern) {
            println!("'{}' match_score:{:?}", &res.item, res.match_score);
        }
    }
}

#[cfg(test)]
mod test {
    use crate::shell;

    use super::*;

    fn init() {
        tracing_subscriber::fmt::init();
    }

    #[test]
    fn test_new() {
        let history = History::from_file("dsh_cmd_history");
        let mut data_dir = dirs::data_dir().unwrap();
        data_dir.push(shell::APP_NAME);
        data_dir.push("dsh_cmd_history");
        let dir = data_dir.into_os_string().into_string().unwrap();
        assert_eq!(history.path, Some(dir));
    }

    #[test]
    fn test_open() -> Result<()> {
        let mut history = History::from_file("dsh_cmd_history");
        let history = history.open()?;
        history.close()
    }

    #[test]
    #[ignore]
    fn test_load() -> Result<()> {
        let mut history = History::from_file("dsh_cmd_history");
        let s = history.load()?;
        tracing::debug!("loaded {:?}", s);
        Ok(())
    }

    #[test]
    #[ignore]
    fn test_back() -> Result<()> {
        let cmd1 = "docker";
        let cmd2 = "docker-compose";

        let mut history = History::from_file("dsh_cmd_history");

        let s = history.load()?;
        tracing::debug!("loaded {:?}", s);

        history.write_history(cmd1)?;
        history.write_history(cmd2)?;

        if let Some(h) = history.back() {
            assert_eq!(cmd2, h);
        } else {
            panic!("failed read history");
        }
        if let Some(h) = history.back() {
            assert_eq!(cmd1, h);
        } else {
            panic!("failed read history");
        }

        Ok(())
    }

    #[test]
    fn frecency() -> Result<()> {
        let mut history = FrecencyHistory::from_file("dsh_frecency_history")?;
        history.add("git");
        history.add("git");
        std::thread::sleep(std::time::Duration::from_millis(100));
        history.add("git checkout");
        let recent = history.sorted(&SortMethod::Recent);
        assert_eq!(recent[0].item, "git checkout");
        assert_eq!(recent[1].item, "git");

        let frequent = history.sorted(&SortMethod::Frequent);
        assert_eq!(frequent[0].item, "git");
        assert_eq!(frequent[1].item, "git checkout");

        let first = history.search_prefix("gi").unwrap();
        assert_eq!(first, "git");

        history.add("git checkout origin master");
        history.add("git config --list");
        history.add("git switch config");
        history.show_score("gc");

        Ok(())
    }

    #[test]
    fn print_item() -> Result<()> {
        let mut history = FrecencyHistory::from_file("dsh_frecency_history")?;
        history.add("git status");
        history.add("git checkout");

        let vec = history.sort_by_match("gsta");

        for item in vec {
            item.print();
        }

        Ok(())
    }
}
