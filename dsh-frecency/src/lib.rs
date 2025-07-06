use anyhow::Result;
use nix::fcntl::{FlockArg, flock};
use std::fs::{File, create_dir_all};
use std::io::{BufReader, BufWriter};
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::process;
use std::time::SystemTime;
use tracing::error;

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
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let store: FrecencyStoreSerializer = bincode::deserialize_from(reader)?;
        Ok(FrecencyStore::from(&store))
    } else {
        Ok(FrecencyStore::default())
    }
}

pub fn write_store(store: &FrecencyStore, path: &PathBuf) -> Result<()> {
    let store_dir = path.parent().expect("file must have parent");
    create_dir_all(store_dir)?;
    let file = File::create(path)?;
    let fd = file.as_raw_fd();
    flock(fd, FlockArg::LockExclusive)?;
    let writer = BufWriter::new(file);
    bincode::serialize_into(writer, &FrecencyStoreSerializer::from(store))?;
    Ok(())
}

pub use crate::stats::ItemStats;
pub use crate::stats::ItemStatsSerializer;
pub use crate::store::FrecencyStore;
pub use crate::store::FrecencyStoreSerializer;
