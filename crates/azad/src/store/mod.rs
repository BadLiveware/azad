//! The content-addressed store (CAS): blobs on disk named by their digest, and directory trees built from them.
//! See decisions 0006 and 0009 for the layout.

mod blobs;
mod caches;
mod checkout;
mod compare;
mod directory_state;
mod error;
mod ingest;
mod materialize;
mod scratch;
mod state;

pub use caches::{CacheInfo, CacheVolume, VOLUME_LOCK_FILE};
pub use compare::{ChangedPath, PathChange};
pub use directory_state::DirectoryState;
pub use error::StoreError;
pub use ingest::Selection;
pub use materialize::SharedTree;
pub use scratch::ScratchDir;
pub use state::{ActionCache, State};

use std::fs;
use std::path::{Path, PathBuf};

const BLOBS_DIR: &str = "blobs";
const TMP_DIR: &str = "tmp";
const TREES_DIR: &str = "trees";

/// A store in one directory. Several processes can use one store at the same time: every write goes to a temporary
/// file first and is renamed into place.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let root = root.into();
        for dir in [BLOBS_DIR, TMP_DIR, TREES_DIR] {
            let path = root.join(dir);
            fs::create_dir_all(&path).map_err(|e| StoreError::io(&path, e))?;
        }
        Ok(Store { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The action cache, aliases and run records of this store.
    pub fn state(&self) -> State {
        State::new(self.clone())
    }

    pub fn tmp_dir(&self) -> PathBuf {
        self.root.join(TMP_DIR)
    }
}
