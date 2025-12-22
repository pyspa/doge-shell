use anyhow::Result;
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
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

#[cfg(test)]
mod extra_tests; // Verification tests

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
        let mut decoder = GzDecoder::new(reader);
        let store: FrecencyStoreSerializer =
            bincode::serde::decode_from_std_read(&mut decoder, bincode::config::standard())?;
        Ok(FrecencyStore::from(&store))
    } else {
        Ok(FrecencyStore::default())
    }
}

pub fn write_store(store: &FrecencyStore, path: &PathBuf) -> Result<()> {
    let store_dir = path.parent().expect("file must have parent");
    create_dir_all(store_dir)?;

    tracing::debug!(
        "Writing frecency store to {}, items: {}, changed: {}",
        path.display(),
        store.items.len(),
        store.changed
    );

    let file = File::create(path)?;
    let fd = file.as_raw_fd();
    flock(fd, FlockArg::LockExclusive)?;
    let writer = BufWriter::new(file);
    let mut encoder = GzEncoder::new(writer, Compression::default());
    bincode::serde::encode_into_std_write(
        FrecencyStoreSerializer::from(store),
        &mut encoder,
        bincode::config::standard(),
    )?;
    encoder.finish()?;

    tracing::debug!("Successfully wrote frecency store to {}", path.display());
    Ok(())
}

pub use crate::stats::ItemStats;
pub use crate::stats::ItemStatsSerializer;
pub use crate::store::FrecencyStore;
pub use crate::store::FrecencyStoreSerializer;

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_read_write_store() -> Result<()> {
        let dir = tempdir()?;
        let path = dir.path().join("frecency.bin");
        let mut store = FrecencyStore::default();
        store.add("foo", None);
        store.add("bar", None);

        write_store(&store, &path)?;

        let loaded_store = read_store(&path)?;
        assert_eq!(store.items.len(), loaded_store.items.len());
        assert_eq!(loaded_store.items[0].item, "bar");
        assert_eq!(loaded_store.items[1].item, "foo");

        Ok(())
    }
}
