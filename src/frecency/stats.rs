use super::*;
use crossterm::style::{Color, Stylize};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

/// A representation of statistics for a single item
#[derive(Debug, Clone)]
pub struct ItemStats {
    pub item: String,
    half_life: f32,
    reference_time: f64,
    frecency: f32,
    last_accessed: f32,
    num_accesses: i32,
    pub match_score: i64,
    pub match_index: Vec<usize>,
}

impl ItemStats {
    /// Create a new item
    pub fn new(item: &str, ref_time: f64, half_life: f32) -> ItemStats {
        ItemStats {
            half_life,
            reference_time: ref_time,
            item: item.to_string(),
            frecency: 0.0,
            last_accessed: 0.0,
            num_accesses: 0,
            match_score: 0,
            match_index: Vec::new(),
        }
    }

    /// Compare the score of two items given a sort method
    pub fn cmp_score(&self, other: &ItemStats, method: &SortMethod) -> Ordering {
        match method {
            SortMethod::Frequent => self.cmp_frequent(other),
            SortMethod::Recent => self.cmp_recent(other),
            SortMethod::Frecent => self.cmp_frecent(other),
        }
    }

    pub fn cmp_match_score(&self, other: &ItemStats) -> Ordering {
        self.match_score.cmp(&other.match_score)
    }

    /// Compare the frequency of two items
    fn cmp_frequent(&self, other: &ItemStats) -> Ordering {
        self.num_accesses.cmp(&other.num_accesses)
    }

    /// Compare the recency of two items
    fn cmp_recent(&self, other: &ItemStats) -> Ordering {
        self.last_accessed
            .partial_cmp(&other.last_accessed)
            .unwrap_or(Ordering::Less)
    }

    /// Compare the frecency of two items
    fn cmp_frecent(&self, other: &ItemStats) -> Ordering {
        self.frecency
            .partial_cmp(&other.frecency)
            .unwrap_or(Ordering::Less)
    }

    /// Change the half life of the item, maintaining the same frecency
    pub fn set_half_life(&mut self, half_life: f32) {
        let old_frecency = self.get_frecency();
        self.half_life = half_life;
        self.set_frecency(old_frecency);
    }

    /// Calculate the frecency of the item
    pub fn get_frecency(&self) -> f32 {
        self.frecency / 2.0f32.powf(secs_elapsed(self.reference_time) as f32 / self.half_life)
    }

    pub fn set_frecency(&mut self, new: f32) {
        self.frecency =
            new * 2.0f32.powf(secs_elapsed(self.reference_time) as f32 / self.half_life);
    }

    /// update the frecency of the item by the given weight
    pub fn update_frecency(&mut self, weight: f32) {
        let original_frecency = self.get_frecency();
        self.set_frecency(original_frecency + weight);
    }

    /// Update the number of accesses of the item by the given weight
    pub fn update_num_accesses(&mut self, weight: i32) {
        self.num_accesses += weight;
    }

    /// Update the time the item was last accessed
    pub fn update_last_access(&mut self, time: f64) {
        self.last_accessed = (time - self.reference_time) as f32;
    }

    /// Reset the reference time and recalculate the last_accessed time
    pub fn reset_ref_time(&mut self, new_time: f64) {
        let original_frecency = self.get_frecency();
        let delta = self.reference_time - new_time;
        self.reference_time = new_time;
        self.last_accessed += delta as f32;
        self.set_frecency(original_frecency);
    }

    /// Return the number of seconds since the item was last accessed
    pub fn secs_since_access(&self) -> f32 {
        secs_elapsed(self.reference_time) - self.last_accessed
    }

    /// sort method if `show_stats` is `true`
    pub fn to_string(&self, method: &SortMethod, show_stats: bool) -> String {
        if show_stats {
            match method {
                SortMethod::Recent => format!(
                    "{: <.3}\t{}\n",
                    self.secs_since_access() / 60.0 / 60.0,
                    self.item
                ),
                SortMethod::Frequent => format!("{: <}\t{}\n", self.num_accesses, self.item),
                SortMethod::Frecent => format!("{: <.3}\t{}\n", self.get_frecency(), self.item),
            }
        } else {
            format!("{}\n", self.item.clone())
        }
    }

    pub fn print(&self) {
        let mut index_iter = self.match_index.iter();
        let mut match_index = index_iter.next();

        for (i, ch) in self.item.as_str().chars().enumerate() {
            let color = if let Some(idx) = match_index {
                if *idx == i {
                    match_index = index_iter.next();
                    Color::Blue
                } else {
                    Color::White
                }
            } else {
                Color::White
            };
            print!("{}", ch.with(color));
        }
    }
}

/// The number of seconds elapsed since `ref_time`
pub fn secs_elapsed(ref_time: f64) -> f32 {
    (current_time_secs() - ref_time) as f32
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ItemStatsSerializer {
    pub item: String,
    pub frecency: f32,
    pub last_accessed: f32,
    pub num_accesses: i32,
}

impl From<&ItemStats> for ItemStatsSerializer {
    fn from(stats: &ItemStats) -> Self {
        ItemStatsSerializer {
            item: stats.item.to_string(),
            frecency: stats.frecency,
            last_accessed: stats.last_accessed,
            num_accesses: stats.num_accesses,
        }
    }
}

impl ItemStatsSerializer {
    pub fn to_item_stats(&self, ref_time: f64, half_life: f32) -> ItemStats {
        ItemStats {
            half_life,
            reference_time: ref_time,
            item: self.item.to_string(),
            frecency: self.frecency,
            last_accessed: self.last_accessed,
            num_accesses: self.num_accesses,
            match_score: 0,
            match_index: Vec::new(),
        }
    }
}
