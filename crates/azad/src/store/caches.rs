//! Cache volumes (decision 0011): directories that an action's tools keep between runs, outside the digest. Each trust
//! level has its own volumes (decision 0019): a pull request never writes a volume that `main` reads.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use std::os::unix::fs::MetadataExt;

use crate::store::scratch::{ScratchDir, make_writable};
use crate::store::{Store, StoreError};
use crate::trust::Trust;

const CACHES_DIR: &str = "caches";
const PULL_REQUEST_CACHES_DIR: &str = "caches-pull-request";
const DATA_DIR: &str = "data";
/// The form of a volume that a VM attaches (decision 0019).
const DISK_FILE: &str = "data.ext4";
/// `st_blocks` counts units of 512 bytes.
const BLOCK_BYTES: u64 = 512;
/// The file that a run locks to hold a volume; it is not part of the volume's state.
pub const VOLUME_LOCK_FILE: &str = "lock";
const LOCK_FILE: &str = VOLUME_LOCK_FILE;
const INPUTS_DIR: &str = "inputs";
const INPUTS_FROM_FILE: &str = "inputs-from";

/// A cache volume that an action may write. The volume stays locked while this value lives; a private volume is
/// removed when this value is dropped.
#[derive(Debug)]
pub struct CacheVolume {
    dir: PathBuf,
    path: PathBuf,
    identity: String,
    shared: bool,
    lock: Option<File>,
    _private: Option<ScratchDir>,
}

impl CacheVolume {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The disk form of the volume, which a VM sandbox makes at its first use.
    pub fn disk(&self) -> PathBuf {
        self.dir.join(DISK_FILE)
    }

    /// The directory of the volume's state: its data, its input checkouts and their record, and its lock file.
    pub fn root(&self) -> &Path {
        &self.dir
    }

    /// Whether this is the shared volume of its name, or a private one because another run held the shared volume.
    pub fn is_shared(&self) -> bool {
        self.shared
    }

    /// Where this volume keeps its checkout of the input that an action mounts at `target`.
    pub fn inputs_dir(&self, target: &str) -> PathBuf {
        let key = target.trim_start_matches('/').replace('%', "%25").replace('/', "%2F");
        self.dir.join(INPUTS_DIR).join(key)
    }

    /// Records that the last run with this volume read the checkouts of `holder`.
    pub fn set_inputs_from(&self, holder: &CacheVolume) -> Result<(), StoreError> {
        let path = self.dir.join(INPUTS_FROM_FILE);
        fs::write(&path, &holder.identity).map_err(|e| StoreError::io(&path, e))
    }

    /// Whether the last run with this volume read the checkouts of `holder`. When it did not, a file in them can be
    /// older than this volume's state and still differ from what that state was built from.
    pub fn had_inputs_from(&self, holder: &CacheVolume) -> bool {
        fs::read_to_string(self.dir.join(INPUTS_FROM_FILE)).is_ok_and(|from| from == holder.identity)
    }
}

// Closing the lock file frees the lock only when no other process has the file open, and a process that Azad starts
// for another action has a copy of every open file until it runs its program. So the lock is freed explicitly, which
// frees it for every copy.
impl Drop for CacheVolume {
    fn drop(&mut self) {
        if let Some(lock) = &self.lock {
            lock.unlock().ok();
        }
    }
}

/// What `Store::caches` reports about one volume.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheInfo {
    pub name: String,
    pub platform: String,
    pub trust: Trust,
    pub path: PathBuf,
    pub last_used: Option<SystemTime>,
    /// The size of the files in the volume, and the space that its disk takes, in bytes.
    pub bytes: u64,
}

impl Store {
    /// The directory of the shared volumes of a trust level; the level `none` has none.
    fn caches_dir(&self, trust: Trust) -> Option<PathBuf> {
        match trust {
            Trust::Main => Some(self.root.join(CACHES_DIR)),
            Trust::PullRequest => Some(self.root.join(PULL_REQUEST_CACHES_DIR)),
            Trust::None => None,
        }
    }

    fn volume_dir(&self, name: &str, platform: &str) -> PathBuf {
        self.root.join(CACHES_DIR).join(name).join(platform.replace('/', "-"))
    }

    /// The volume of a cache name and platform for a run of a trust level, locked for this run; or a fresh private
    /// volume when another run holds it, and always for the level `none`.
    pub fn cache_volume(&self, name: &str, platform: &str, trust: Trust) -> Result<CacheVolume, StoreError> {
        let Some(caches) = self.caches_dir(trust) else { return self.private_volume() };
        let dir = caches.join(name).join(platform.replace('/', "-"));
        let data = dir.join(DATA_DIR);
        for path in [&data, &dir.join(INPUTS_DIR)] {
            fs::create_dir_all(path).map_err(|e| StoreError::io(path, e))?;
        }
        let lock_path = dir.join(LOCK_FILE);
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .map_err(|e| StoreError::io(&lock_path, e))?;
        match lock.try_lock() {
            Ok(()) => {
                lock.set_modified(SystemTime::now()).map_err(|e| StoreError::io(&lock_path, e))?;
                let identity = format!("{trust}/{name}/{platform}");
                Ok(CacheVolume { dir, path: data, identity, shared: true, lock: Some(lock), _private: None })
            }
            Err(fs::TryLockError::WouldBlock) => self.private_volume(),
            Err(fs::TryLockError::Error(e)) => Err(StoreError::io(&lock_path, e)),
        }
    }

    fn private_volume(&self) -> Result<CacheVolume, StoreError> {
        let private = self.scratch_dir()?;
        let dir = private.path().to_path_buf();
        let data = dir.join(DATA_DIR);
        for path in [&data, &dir.join(INPUTS_DIR)] {
            fs::create_dir(path).map_err(|e| StoreError::io(path, e))?;
        }
        let identity = dir.display().to_string();
        Ok(CacheVolume { dir, path: data, identity, shared: false, lock: None, _private: Some(private) })
    }

    /// The directory of the shared `main` volume of a name and platform, as `CacheVolume::root` gives it, whether or
    /// not it exists. Remote caches share only these volumes.
    pub fn cache_volume_root(&self, name: &str, platform: &str) -> PathBuf {
        self.volume_dir(name, platform)
    }

    /// Whether this machine has the shared `main` volume of a name and platform.
    pub fn has_cache_volume(&self, name: &str, platform: &str) -> bool {
        self.volume_dir(name, platform).join(DATA_DIR).is_dir()
    }

    /// Makes the directory `state`, which holds the state of a volume as `CacheVolume::root` does, the shared volume of
    /// a name and platform, unless this machine has one already. `state` must be on the store's file system. Returns
    /// whether it took the place of the volume.
    pub fn install_cache_volume(&self, name: &str, platform: &str, state: &Path) -> Result<bool, StoreError> {
        let dir = self.volume_dir(name, platform);
        if self.has_cache_volume(name, platform) {
            return Ok(false);
        }
        let parent = dir.parent().expect("a volume directory has a parent");
        fs::create_dir_all(parent).map_err(|e| StoreError::io(parent, e))?;
        match fs::rename(state, &dir) {
            Ok(()) => Ok(true),
            Err(e) if dir.exists() => {
                make_writable(&dir).ok();
                fs::remove_dir_all(&dir).map_err(|_| StoreError::io(&dir, e))?;
                fs::rename(state, &dir).map_err(|e| StoreError::io(&dir, e))?;
                Ok(true)
            }
            Err(e) => Err(StoreError::io(&dir, e)),
        }
    }

    /// Every cache volume, by trust level, name and platform.
    pub fn caches(&self) -> Result<Vec<CacheInfo>, StoreError> {
        let mut found = Vec::new();
        for trust in [Trust::Main, Trust::PullRequest] {
            let Some(caches) = self.caches_dir(trust) else { continue };
            for (name, name_dir) in entries(&caches)? {
                for (platform, dir) in entries(&name_dir)? {
                    let last_used = fs::metadata(dir.join(LOCK_FILE)).and_then(|m| m.modified()).ok();
                    let platform = platform.replacen('-', "/", 1);
                    let data = dir.join(DATA_DIR);
                    let bytes = size_of(&data) + allocated(&dir.join(DISK_FILE));
                    found.push(CacheInfo { name: name.clone(), platform, trust, path: data, last_used, bytes });
                }
            }
        }
        Ok(found)
    }

    /// Removes the cache volumes with the given name, or all of them, except volumes that a run holds now. Returns how
    /// many it removed.
    pub fn clear_caches(&self, name: Option<&str>) -> Result<usize, StoreError> {
        let mut removed = 0;
        for info in self.caches()?.into_iter().filter(|info| name.is_none_or(|n| n == info.name)) {
            let dir = info.path.parent().expect("a volume's data directory has a parent").to_path_buf();
            let lock_path = dir.join(LOCK_FILE);
            let lock = File::open(&lock_path).map_err(|e| StoreError::io(&lock_path, e))?;
            if lock.try_lock().is_err() {
                continue;
            }
            make_writable(&dir)?;
            fs::remove_dir_all(&dir).map_err(|e| StoreError::io(&dir, e))?;
            removed += 1;
        }
        Ok(removed)
    }
}

fn entries(dir: &Path) -> Result<Vec<(String, PathBuf)>, StoreError> {
    let listing = match fs::read_dir(dir) {
        Ok(listing) => listing,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(StoreError::io(dir, e)),
    };
    let mut found = Vec::new();
    for entry in listing {
        let entry = entry.map_err(|e| StoreError::io(dir, e))?;
        if entry.file_type().map_err(|e| StoreError::io(dir, e))?.is_dir() {
            found.push((entry.file_name().to_string_lossy().into_owned(), entry.path()));
        }
    }
    found.sort();
    Ok(found)
}

/// The space that a sparse file takes on disk.
fn allocated(path: &Path) -> u64 {
    fs::metadata(path).map(|metadata| metadata.blocks() * BLOCK_BYTES).unwrap_or(0)
}

fn size_of(path: &Path) -> u64 {
    let Ok(metadata) = fs::symlink_metadata(path) else { return 0 };
    if !metadata.is_dir() {
        return metadata.len();
    }
    let Ok(listing) = fs::read_dir(path) else { return 0 };
    listing.filter_map(Result::ok).map(|entry| size_of(&entry.path())).sum()
}
