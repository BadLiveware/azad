//! Kept state in a remote cache (decision 0017): a cache volume, or a directory of the workspace that a host task
//! keeps, as one gzipped tar that keeps modification times, under a name that a provider can find by prefix.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use sha2::{Digest as _, Sha256};

use crate::remote::{FORMAT, RemoteError};
use crate::store::{DirectoryState, Store, StoreError, VOLUME_LOCK_FILE};

const VOLUME_KIND: &str = "volume";
const DIRECTORY_KIND: &str = "dir";
const STAGING_PREFIX: &str = ".azad-restore-";
const ID_HEX_LEN: usize = 16;
/// Wide enough for the nanoseconds since 1970 for centuries, so that names sort by time.
const TIME_WIDTH: usize = 20;

/// State that runs keep between them: a cache volume of the store, or a directory of the workspace that a host task
/// keeps, such as Cargo's `target/` (decision 0017).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kept {
    Volume { name: String, platform: String },
    Directory { name: String, platform: String, path: PathBuf },
}

impl Kept {
    pub fn name(&self) -> &str {
        match self {
            Kept::Volume { name, .. } | Kept::Directory { name, .. } => name,
        }
    }

    /// The start of the names of its copies. A volume and a directory of the same name have other layouts, so they
    /// have other names.
    pub(crate) fn prefix(&self) -> String {
        let (kind, name, platform) = match self {
            Kept::Volume { name, platform } => (VOLUME_KIND, name, platform),
            Kept::Directory { name, platform, .. } => (DIRECTORY_KIND, name, platform),
        };
        let id: String =
            Sha256::digest(format!("{name}\0{platform}").as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
        format!("{FORMAT}-{kind}-{}-", &id[..ID_HEX_LEN])
    }

    /// The name of a new copy: its prefix and the time now.
    pub(crate) fn new_key(&self) -> String {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default();
        format!("{}{nanos:0TIME_WIDTH$}", self.prefix())
    }

    /// The directory that holds the state.
    pub(crate) fn root(&self, store: &Store) -> PathBuf {
        match self {
            Kept::Volume { name, platform } => store.cache_volume_root(name, platform),
            Kept::Directory { path, .. } => path.clone(),
        }
    }

    pub(crate) fn exists(&self, store: &Store) -> bool {
        match self {
            Kept::Volume { name, platform } => store.has_cache_volume(name, platform),
            Kept::Directory { path, .. } => path.exists(),
        }
    }

    /// The state now, or `None` when there is none.
    pub(crate) fn state(&self, store: &Store, since: &DirectoryState) -> Result<Option<DirectoryState>, StoreError> {
        if !self.exists(store) {
            return Ok(None);
        }
        since.again(&self.root(store), &[VOLUME_LOCK_FILE]).map(Some)
    }

    /// A new directory in which to unpack a copy, on the file system of the state, so that `install` can move it.
    pub(crate) fn staging(&self, store: &Store) -> Result<tempfile::TempDir, RemoteError> {
        let parent = match self {
            Kept::Volume { .. } => store.tmp_dir(),
            Kept::Directory { path, .. } => path.parent().expect("a kept directory has a parent").to_path_buf(),
        };
        std::fs::create_dir_all(&parent).map_err(|e| RemoteError::io(&parent, e))?;
        tempfile::Builder::new().prefix(STAGING_PREFIX).tempdir_in(&parent).map_err(|e| RemoteError::io(&parent, e))
    }

    /// Makes `state`, a directory from `staging`, the kept state, unless there is one already. Returns whether it did.
    pub(crate) fn install(&self, store: &Store, state: &Path) -> Result<bool, RemoteError> {
        match self {
            Kept::Volume { name, platform } => Ok(store.install_cache_volume(name, platform, state)?),
            Kept::Directory { path, .. } if path.exists() => Ok(false),
            Kept::Directory { path, .. } => {
                std::fs::rename(state, path).map_err(|e| RemoteError::io(path, e))?;
                Ok(true)
            }
        }
    }
}

/// Writes the state at `root` to `dest`, without the lock file of a volume.
pub(crate) fn pack(root: &Path, dest: &Path) -> Result<(), RemoteError> {
    let file = File::create(dest).map_err(|e| RemoteError::io(dest, e))?;
    let mut archive = tar::Builder::new(GzEncoder::new(BufWriter::new(file), Compression::fast()));
    archive.follow_symlinks(false);
    for entry in std::fs::read_dir(root).map_err(|e| RemoteError::io(root, e))? {
        let entry = entry.map_err(|e| RemoteError::io(root, e))?;
        let name = entry.file_name();
        if name == VOLUME_LOCK_FILE {
            continue;
        }
        let path = entry.path();
        let is_dir = entry.file_type().map_err(|e| RemoteError::io(&path, e))?.is_dir();
        let added = match is_dir {
            true => archive.append_dir_all(&name, &path),
            false => archive.append_path_with_name(&path, &name),
        };
        added.map_err(|e| RemoteError::io(&path, e))?;
    }
    let mut out =
        archive.into_inner().map_err(|e| RemoteError::io(dest, e))?.finish().map_err(|e| RemoteError::io(dest, e))?;
    out.flush().map_err(|e| RemoteError::io(dest, e))
}

/// Writes the state in a pack to the new directory `dest`, with the modification times of the pack.
pub(crate) fn unpack(pack: &Path, dest: &Path) -> Result<(), RemoteError> {
    let file = File::open(pack).map_err(|e| RemoteError::io(pack, e))?;
    let mut archive = tar::Archive::new(GzDecoder::new(file));
    archive.set_preserve_mtime(true);
    archive.set_preserve_permissions(true);
    archive.unpack(dest).map_err(|e| RemoteError::io(dest, e))
}
