use anyhow::Result;
use log::error;
use std::fs::{create_dir_all, File};
use std::io::{BufReader, BufWriter};
use std::path::PathBuf;
use std::process;
use std::time::SystemTime;

mod stats;
mod store;

#[derive(Debug, Clone)]
pub enum SortMethod {
    Recent,
    Frequent,
    Frecent,
}

/// Return the current time in seconds as a float
pub fn current_time_secs() -> f64 {
    match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
        Ok(n) => (n.as_secs() as u128 * 1000 + n.subsec_millis() as u128) as f64 / 1000.0,
        Err(e) => {
            error!("invalid system time: {}", e);
            process::exit(1);
        }
    }
}

pub fn read_store(path: &PathBuf) -> Result<FrecencyStore> {
    if path.is_file() {
        let file = File::open(&path)?;
        let reader = BufReader::new(file);
        let store: FrecencyStoreSerializer = serde_json::from_reader(reader)?;
        Ok(FrecencyStore::from(&store))
    } else {
        Ok(FrecencyStore::default())
    }
}

pub fn write_store(store: &FrecencyStore, path: &PathBuf) -> Result<()> {
    let store_dir = path.parent().expect("file must have parent");
    create_dir_all(&store_dir)?;
    let file = File::create(&path)?;
    let writer = BufWriter::new(file);
    serde_json::to_writer_pretty(writer, &FrecencyStoreSerializer::from(store))?;

    Ok(())
}

pub use crate::frecency::stats::ItemStats;
pub use crate::frecency::stats::ItemStatsSerializer;
pub use crate::frecency::store::FrecencyStore;
pub use crate::frecency::store::FrecencyStoreSerializer;
