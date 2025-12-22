use crate::environment;
use anyhow::Context as _;
use anyhow::Result;
use chrono::Local;
use dsh_frecency::{FrecencyStore, ItemStats, SortMethod, read_store, write_store};
use easy_reader::EasyReader;
use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

fn get_current_context() -> Option<String> {
    // Try to get git root
    if let Ok(output) = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        && output.status.success()
    {
        let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !root.is_empty() {
            return Some(root);
        }
    }

    // Fallback to current directory
    std::env::current_dir()
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

#[derive(Serialize, Deserialize, Debug)]
struct Entry {
    entry: String,
    when: i64,
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct History {
    pub path: Option<String>,
    open_file: Option<File>,
    histories: Vec<Entry>,
    size: usize,
    current_index: usize,
    pub search_word: Option<String>,
}

#[allow(dead_code)]
impl Default for History {
    fn default() -> Self {
        Self::new()
    }
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

    pub fn from_file(name: &str) -> Result<Self> {
        let file_path = environment::get_data_file(name)?;
        let path = file_path
            .into_os_string()
            .into_string()
            .map_err(|_| anyhow::anyhow!("Invalid path encoding"))?;

        Ok(History {
            path: Some(path),
            open_file: None,
            histories: Vec::new(),
            size: 10000,
            current_index: 0,
            search_word: None,
        })
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
        if let Some(ref path) = self.path {
            // Use file lock to prevent concurrent access
            let lock_path = format!("{path}.lock");
            let _lock = file_lock::FileLock::lock(&lock_path, true, file_lock::FileOptions::new())
                .context("Failed to acquire history file lock")?;

            let mut history_file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .with_context(|| format!("Failed to open history file: {path}"))?;

            let now = Local::now();
            let entry = Entry {
                entry: history.to_string(),
                when: now.timestamp(),
            };

            let json =
                serde_json::to_string(&entry).context("Failed to serialize history entry")?;
            let json_line = json + "\n";

            history_file
                .write_all(json_line.as_bytes())
                .with_context(|| format!("Failed to write to history file: {path}"))?;

            history_file
                .flush()
                .with_context(|| format!("Failed to flush history file: {path}"))?;

            // Lock is automatically released when _lock goes out of scope

            let entry = Entry {
                entry: history.to_string(),
                when: now.timestamp(),
            };
            self.histories.push(entry);
            self.reset_index();

            Ok(())
        } else {
            Ok(())
        }
    }

    pub fn search_first(&self, word: &str) -> Option<&str> {
        for hist in self.histories.iter().rev() {
            if hist.entry.starts_with(word) {
                return Some(&hist.entry);
            }
        }
        None
    }
}

pub struct FrecencyHistory {
    pub path: Option<PathBuf>,
    pub store: Option<Arc<FrecencyStore>>,
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

#[allow(dead_code)]
impl Default for FrecencyHistory {
    fn default() -> Self {
        Self::new()
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
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    "Failed to read history store from {:?}: {}. Starting with empty store.",
                    file_path,
                    e
                );
                FrecencyStore::default()
            }
        };

        let f = FrecencyHistory {
            path: Some(file_path),
            store: Some(Arc::new(store)),
            histories: None,
            current_index: 0,
            search_word: None,
            prev_search_word: "".to_string(),
            matcher,
        };
        Ok(f)
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
                    let ctx = get_current_context();
                    // Use Frecent sort with context boost for better relevance
                    self.histories = Some(
                        self.store
                            .as_ref()
                            .unwrap()
                            .sorted_with_context(&SortMethod::Frecent, ctx.as_deref()),
                    );
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
                    let ctx = get_current_context();
                    self.histories = Some(
                        self.store
                            .as_ref()
                            .unwrap()
                            .sorted_with_context(&SortMethod::Frecent, ctx.as_deref()),
                    );
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
        if let Some(ref file_path) = self.path
            && let Some(ref mut store) = self.store
            && store.changed
        {
            let result = write_store(store, file_path);
            if result.is_ok() {
                // make_mut to update changed flag
                let store_mut = Arc::make_mut(store);
                store_mut.changed = false;
            }
            return result;
        }
        Ok(())
    }

    /// Save history to disk in a background task to prevent blocking the main thread
    pub fn save_background(&mut self) {
        if let Some(ref file_path) = self.path
            && let Some(ref mut store) = self.store
            && store.changed
        {
            // Clone the Arc to move into the background thread (cheap)
            let store_clone = Arc::clone(store);
            let path_clone = file_path.clone();

            // Reset dirty flag immediately in the main thread instance
            // modifying it via make_mut (COW if shared, but here we just cloned for bg)
            // Wait, if we modify *store now, we fork it from store_clone.
            // store_clone points to PREVIOUS (dirty) state. CORRECT.
            // self.store will point to NEW (clean) state.
            // But wait, if we are just setting changed=false, we fundamentally want them to share data?
            // No, FrecencyStore logic: we write the snapshot.
            // Future additions should go to self.store.
            let store_mut = Arc::make_mut(store);
            store_mut.changed = false;

            // Spawn blocking task for I/O using the snapshot
            tokio::task::spawn_blocking(move || {
                if let Err(e) = write_store(&store_clone, &path_clone) {
                    tracing::warn!("Failed to save history in background: {}", e);
                }
            });
        }
    }

    pub fn add(&mut self, history: &str) {
        if let Some(ref mut store) = self.store {
            let ctx = get_current_context();
            Arc::make_mut(store).add(history, ctx);
        }
    }

    /// 強制的にchangedフラグをtrueに設定し、保存を強制する
    pub fn force_changed(&mut self) {
        if let Some(ref mut store) = self.store {
            Arc::make_mut(store).changed = true;
            tracing::debug!(
                "Forcing changed flag to true. Items count: {}",
                store.items.len()
            );
        }
    }

    pub fn search_fuzzy_first(&self, pattern: &str) -> Option<String> {
        let results = self.sort_by_match(pattern);
        results.into_iter().next().map(|item| item.item)
    }

    pub fn search_prefix(&self, pattern: &str) -> Option<String> {
        let ctx = get_current_context();
        self.search_prefix_with_context(pattern, ctx.as_deref())
    }

    pub fn search_prefix_with_context(
        &self,
        pattern: &str,
        context: Option<&str>,
    ) -> Option<String> {
        if let Some(ref store) = self.store {
            let range = store.search_prefix_range(pattern);
            store
                .items
                .get(range)?
                .iter()
                .max_by(|a, b| a.cmp_frecent_with_context(b, context))
                .map(|item| item.item.clone())
        } else {
            None
        }
    }

    pub fn search_recent_prefix(&self, pattern: &str) -> Option<String> {
        if let Some(ref store) = self.store {
            let range = store.search_prefix_range(pattern);
            store
                .items
                .get(range)?
                .iter()
                .max_by(|a, b| a.cmp_recent(b))
                .map(|item| item.item.clone())
        } else {
            None
        }
    }

    pub fn sort_by_match(&self, pattern: &str) -> Vec<ItemStats> {
        let Some(ref store) = self.store else {
            return Vec::new();
        };

        // Pre-allocate with estimated capacity (assume ~25% match rate)
        let estimated_matches = store.items.len() / 4;
        let mut results = Vec::with_capacity(estimated_matches.max(16));

        for item in store.items.iter() {
            if let Some((score, index)) = self.matcher.fuzzy_indices(&item.item, pattern)
                && score > 25
            {
                let mut item = item.clone();
                item.match_score = score;
                item.match_index = index;
                results.push(item);
            }
        }

        let ctx = get_current_context();
        self.sort_by_match_with_context(pattern, ctx.as_deref())
    }

    pub fn sort_by_match_with_context(
        &self,
        pattern: &str,
        context: Option<&str>,
    ) -> Vec<ItemStats> {
        let Some(ref store) = self.store else {
            return Vec::new();
        };

        // Pre-allocate with estimated capacity (assume ~25% match rate)
        let estimated_matches = store.items.len() / 4;
        let mut results = Vec::with_capacity(estimated_matches.max(16));

        for item in store.items.iter() {
            if let Some((score, index)) = self.matcher.fuzzy_indices(&item.item, pattern)
                && score > 25
            {
                let mut item = item.clone();
                item.match_score = score;
                item.match_index = index;
                results.push(item);
            }
        }

        results.sort_by(|a, b| a.cmp_match_score_with_context(b, context).reverse());
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
mod tests {
    use crate::shell;

    use super::*;

    fn init() {
        let _ = tracing_subscriber::fmt::try_init();
    }

    #[test]
    fn test_new() {
        init();
        let history = History::from_file("dsh_cmd_history").unwrap();
        let mut data_dir = dirs::data_dir().unwrap();
        data_dir.push(shell::APP_NAME);
        data_dir.push("dsh_cmd_history");
        let dir = data_dir.into_os_string().into_string().unwrap();
        assert_eq!(history.path, Some(dir));
    }

    #[test]
    fn test_open() -> Result<()> {
        init();
        let mut history = History::from_file("dsh_cmd_history")?;
        let history = history.open()?;
        history.close()
    }

    #[test]
    #[ignore]
    fn test_load() -> Result<()> {
        init();
        let mut history = History::from_file("dsh_cmd_history")?;
        let s = history.load()?;
        tracing::debug!("loaded {:?}", s);
        Ok(())
    }

    #[test]
    #[ignore]
    fn test_back() -> Result<()> {
        init();
        let cmd1 = "docker";
        let cmd2 = "docker-compose";

        let mut history = History::from_file("dsh_cmd_history")?;

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
    #[ignore]
    fn frecency() -> Result<()> {
        init();
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
        assert_eq!(first, "git checkout");

        history.add("git checkout origin master");
        history.add("git config --list");
        history.add("git switch config");
        history.show_score("gc");

        Ok(())
    }

    #[test]
    fn print_item() -> Result<()> {
        init();
        let mut history = FrecencyHistory::from_file("dsh_frecency_history")?;
        history.add("git status");
        history.add("git checkout");

        let vec = history.sort_by_match("gsta");
        let mut out = std::io::stdout().lock();
        for item in vec {
            item.print(&mut out);
        }

        Ok(())
    }

    #[test]
    fn test_frecency_completion() -> Result<()> {
        init();
        // Use a temporary file for this test
        let temp_dir = tempfile::tempdir()?;
        let file_path = temp_dir.path().join("frecency_test_history");

        let mut history = FrecencyHistory::new();
        history.path = Some(file_path.clone());
        history.store = Some(Arc::new(dsh_frecency::FrecencyStore::default()));

        // Add "frequent_cmd" 5 times
        for _ in 0..5 {
            history.add("frequent_cmd");
        }

        // Add "frequent_but_old" 5 times, but simulate time passing if possible?
        // dsh_frecency uses system time, so consistent time manipulation is hard without mocking.
        // But assuming same timeframe:

        // Add "recent_cmd" once
        history.add("recent_cmd");

        // "f frequent_cmd" (5 accesses) vs "r recent_cmd" (1 access)

        // If we search for empty prefix, or common prefix if they had one.
        // Let's use common prefix "cmd"
        history.add("cmd_frequent"); // 1
        history.add("cmd_frequent"); // 2
        history.add("cmd_frequent"); // 3
        history.add("cmd_recent"); // 1 (most recent)

        // Search for "cmd"
        // cmd_frequent: score ~ 3
        // cmd_recent: score ~ 1 (but decent recency)
        // Frecency should favor cmd_frequent if weights are standard.

        let result = history.search_prefix("cmd");
        assert_eq!(result, Some("cmd_frequent".to_string()));

        // Search for "cmd_r" should still find cmd_recent
        let result_recent = history.search_prefix("cmd_r");
        assert_eq!(result_recent, Some("cmd_recent".to_string()));

        Ok(())
    }

    #[test]
    fn test_save_no_path() -> Result<()> {
        init();
        let mut history = FrecencyHistory::new();
        // Path is None by default
        assert!(history.path.is_none());

        // Should not panic
        history.save()?;

        Ok(())
    }

    #[test]
    fn test_context_aware_boosting() -> Result<()> {
        init();
        let temp_dir = tempfile::tempdir()?;
        let dir_a = temp_dir.path().join("a");
        let dir_b = temp_dir.path().join("b");
        // We don't need to create real directories since we pass strings
        // std::fs::create_dir(&dir_a)?;
        // std::fs::create_dir(&dir_b)?;
        let dir_a_str = dir_a.to_string_lossy().to_string();
        let dir_b_str = dir_b.to_string_lossy().to_string();

        let history_file = temp_dir.path().join("history_context");
        let mut history = FrecencyHistory::new();
        history.path = Some(history_file);
        history.store = Some(Arc::new(dsh_frecency::FrecencyStore::default()));

        // Simulate usage in Dir A
        if let Some(ref mut store) = history.store {
            let store_mut = Arc::make_mut(store);
            store_mut.add("cmd_common", Some(dir_a_str.clone()));
            store_mut.add("cmd_unique_a", Some(dir_a_str.clone()));

            // Simulate usage in Dir B
            store_mut.add("cmd_common", Some(dir_b_str.clone())); // common cmd used in both
            store_mut.add("cmd_unique_b", Some(dir_b_str.clone()));
        }

        // Back to Dir A context
        // Try searching in context A
        // "cmd_unique_a" (score 1.0 * 2.0 = 2.0) vs "cmd_unique_b" (score 1.0 * 1.0 = 1.0)
        let result = history.search_prefix_with_context("cmd_unique", Some(&dir_a_str));
        assert_eq!(result, Some("cmd_unique_a".to_string()));

        // Now context B
        let result_b = history.search_prefix_with_context("cmd_unique", Some(&dir_b_str));
        assert_eq!(result_b, Some("cmd_unique_b".to_string()));

        Ok(())
    }
    #[test]
    fn test_arc_cow_behavior() -> Result<()> {
        init();
        let mut history = FrecencyHistory::new();
        // Create initial store
        let mut store = dsh_frecency::FrecencyStore::default();
        store.add("initial_cmd", None);
        store.changed = true; // Simulate dirty state
        history.store = Some(Arc::new(store));

        // Create a "snapshot" by cloning the Arc (simulating background save start)
        let snapshot_arc = Arc::clone(history.store.as_ref().unwrap());

        // Modify history (simulating user typing immediately after save starts)
        history.add("new_cmd");

        // Force reset change flag on the new state (as save_background does via make_mut)
        if let Some(ref mut s) = history.store {
            let s_mut = Arc::make_mut(s);
            s_mut.changed = false;
        }

        // Verification:
        // 1. Snapshot should NOT have "new_cmd"
        // 2. Snapshot should still be marked as changed=true (from original state)
        // 3. Current history SHOULD have "new_cmd"
        // 4. Current history should be changed=false (we reset it)

        let snapshot = snapshot_arc.as_ref();
        let current = history.store.as_ref().unwrap().as_ref();

        // 1. Snapshot items check
        assert!(snapshot.items.iter().any(|i| i.item == "initial_cmd"));
        assert!(!snapshot.items.iter().any(|i| i.item == "new_cmd"));

        // 2. Snapshot dirty flag check
        assert!(
            snapshot.changed,
            "Snapshot should retain original dirty flag"
        );

        // 3. Current items check
        assert!(current.items.iter().any(|i| i.item == "initial_cmd"));
        assert!(current.items.iter().any(|i| i.item == "new_cmd"));

        // 4. Current dirty flag check
        assert!(
            !current.changed,
            "Current store should have dirty flag reset"
        );

        // 5. Pointer inequality check (proving they are distinct allocations now)
        assert!(
            !std::ptr::eq(snapshot, current),
            "Snapshot and current store should point to different memory locations"
        );

        Ok(())
    }

    #[test]
    fn test_async_history_update() {
        use parking_lot::Mutex;
        use std::sync::{Arc, Barrier};
        use std::thread;

        // Simulate the async loading pattern used in lib.rs
        let history = Arc::new(Mutex::new(FrecencyHistory::new()));
        let history_clone = history.clone();

        // Barrier to synchronize start
        let barrier = Arc::new(Barrier::new(2));
        let barrier_clone = barrier.clone();

        let handle = thread::spawn(move || {
            barrier_clone.wait();
            // Simulate heavy load
            let mut store = FrecencyStore::default();
            store.add("async_cmd", None);

            // "Load" complete - swap data
            let mut guard = history_clone.lock();
            guard.store = Some(Arc::new(store));
        });

        // Initially empty
        assert!(
            history.lock().store.is_none()
                || history.lock().store.as_ref().unwrap().items.is_empty()
        );

        barrier.wait();
        handle.join().unwrap();

        // Should now have data
        let guard = history.lock();
        assert!(guard.store.is_some());
        assert!(
            guard
                .store
                .as_ref()
                .unwrap()
                .items
                .iter()
                .any(|i| i.item == "async_cmd")
        );
    }
}
