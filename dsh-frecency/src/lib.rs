mod stats;
mod store;

#[cfg(test)]
mod extra_tests;

#[derive(Debug, Clone)]
pub enum SortMethod {
    Recent,
    Frequent,
    Frecent,
}

/// Return the current time in seconds as a float
pub fn current_time_secs() -> f64 {
    match std::time::SystemTime::now().duration_since(std::time::SystemTime::UNIX_EPOCH) {
        Ok(n) => (n.as_secs() as u128 * 1000 + n.subsec_millis() as u128) as f64 / 1000.0,
        Err(e) => {
            tracing::error!("invalid system time: {}", e);
            std::process::exit(1);
        }
    }
}

pub use crate::stats::ItemStats;
pub use crate::stats::ItemStatsSerializer;
pub use crate::store::FrecencyStore;
pub use crate::store::FrecencyStoreSerializer;
