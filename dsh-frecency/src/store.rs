use super::*;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// A collection of statistics about the stored items
#[derive(Clone)]
pub struct FrecencyStore {
    pub reference_time: f64,
    pub half_life: f32,
    pub items: Vec<ItemStats>,
    pub size: usize,
    pub changed: bool,
}

impl Default for FrecencyStore {
    fn default() -> FrecencyStore {
        FrecencyStore {
            reference_time: current_time_secs(),
            half_life: 60.0 * 60.0 * 12.0 * 1.0,
            items: Vec::new(),
            size: 0,
            changed: false,
        }
    }
}

impl FrecencyStore {
    /// Remove all but the top N (sorted by `sort_method`) from the `UsageStore`
    /// IMPORTANT: This method restores the name-sorted invariant after truncation.
    pub fn truncate(&mut self, keep_num: usize, sort_method: &SortMethod) {
        let mut sorted_vec = self.sorted(sort_method);
        if sorted_vec.len() > keep_num {
            sorted_vec.truncate(keep_num);
        }
        // Restore name-sorted order for get() and search_prefix_range()
        sorted_vec.sort_by(|a, b| a.item.cmp(&b.item));
        self.items = sorted_vec;
    }

    /// Change the half life and reweight such that frecency does not change
    pub fn set_half_life(&mut self, half_life: f32) {
        self.reset_time();
        self.half_life = half_life;

        for item in self.items.iter_mut() {
            item.set_half_life(half_life);
        }
    }

    /// Return the number of half lives passed since the reference time
    pub fn half_lives_passed(&self) -> f64 {
        (current_time_secs() - self.reference_time) / self.half_life as f64
    }

    /// Reset the reference time to now, and reweight all the statistics to reflect that
    pub fn reset_time(&mut self) {
        let current_time = current_time_secs();

        self.reference_time = current_time;

        for item in self.items.iter_mut() {
            item.reset_ref_time(current_time);
        }
    }

    /// Log a visit to a item
    pub fn add(&mut self, item: &str, context: Option<String>) {
        let item_stats = self.get(item, context);

        item_stats.update_frecency(1.0);
        item_stats.update_num_accesses(1);
        item_stats.update_last_access(current_time_secs());

        // Mark as changed since we've added/updated an item
        self.changed = true;
    }

    pub fn check_changed(&mut self) {
        let changed = self.size != self.items.len();
        self.size = self.items.len();
        if changed {
            self.changed = true;
        }
    }

    /// Adjust the score of a item by a given weight
    pub fn adjust(&mut self, item: &str, weight: f32) {
        let item_stats = self.get(item, None);

        item_stats.update_frecency(weight);
        item_stats.update_num_accesses(weight as i32);
    }

    /// Delete an item from the store
    pub fn delete(&mut self, item: &str) {
        if let Some(idx) = self.items.iter().position(|i| i.item == item) {
            self.items.remove(idx);
        }
        self.check_changed();
    }

    pub fn prune(&mut self) {
        self.items.retain(|item| Path::new(&item.item).exists());
        self.check_changed();
    }

    /// Return a sorted vector of all the items in the store, sorted by `sort_method`
    pub fn sorted(&self, sort_method: &SortMethod) -> Vec<ItemStats> {
        let mut new_vec = self.items.clone();
        new_vec.sort_by(|item1, item2| item1.cmp_score(item2, sort_method).reverse());

        new_vec
    }

    /// Return a sorted vector of all items in the store, sorted by `sort_method`
    /// If `current_context` is provided, items with matching context will be boosted (for Frecent sort method)
    pub fn sorted_with_context(
        &self,
        sort_method: &SortMethod,
        current_context: Option<&str>,
    ) -> Vec<ItemStats> {
        let mut new_vec = self.items.clone();
        new_vec.sort_by(|item1, item2| {
            item1
                .cmp_score_with_context(item2, sort_method, current_context)
                .reverse()
        });

        new_vec
    }

    /// Retrieve a mutable reference to a item in the store.
    /// If the item does not exist, create it and return a reference to the created item
    /// Retrieve a mutable reference to a item in the store.
    /// If the item does not exist, create it and return a reference to the created item
    /// Get the range of items that start with the given prefix.
    /// Since items are sorted by name, we can use binary search to find the start.
    pub fn search_prefix_range(&self, prefix: &str) -> std::ops::Range<usize> {
        let start = self
            .items
            .partition_point(|item| item.item.as_str() < prefix);

        let end = start + self.items[start..].partition_point(|item| item.item.starts_with(prefix));

        start..end
    }

    fn get(&mut self, item: &str, context: Option<String>) -> &mut ItemStats {
        match self
            .items
            .binary_search_by_key(&item, |item_stats| &item_stats.item)
        {
            Ok(idx) => {
                let stats = &mut self.items[idx];
                // Update context if provided
                if context.is_some() {
                    stats.context = context;
                }
                stats
            }
            Err(idx) => {
                self.items.insert(
                    idx,
                    ItemStats::new(item, self.reference_time, self.half_life, context),
                );
                &mut self.items[idx]
            }
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct FrecencyStoreSerializer {
    reference_time: f64,
    half_life: f32,
    items: Vec<ItemStatsSerializer>,
}

impl From<&FrecencyStore> for FrecencyStoreSerializer {
    fn from(store: &FrecencyStore) -> Self {
        let items: Vec<ItemStatsSerializer> =
            store.items.iter().map(ItemStatsSerializer::from).collect();

        FrecencyStoreSerializer {
            reference_time: store.reference_time,
            half_life: store.half_life,
            items,
        }
    }
}

impl From<&FrecencyStoreSerializer> for FrecencyStore {
    fn from(store: &FrecencyStoreSerializer) -> Self {
        let ref_time = store.reference_time;
        let half_life = store.half_life;
        let items: Vec<ItemStats> = store
            .items
            .iter()
            .map(|s| s.to_item_stats(ref_time, half_life))
            .collect();

        let size = items.len();
        FrecencyStore {
            reference_time: store.reference_time,
            half_life: store.half_life,
            items,
            size,
            changed: false,
        }
    }
}
