//! What a directory holds, file by file, to tell whether a run changed it (decision 0017). Tools touch files without
//! changing them, such as Cargo's `.cargo-lock`, so the content counts, not the time.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io;
use std::path::Path;
use std::time::SystemTime;

use crate::ir::{Digest, Hasher};
use crate::store::StoreError;

/// The files and links of a directory, below it, with their content.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DirectoryState {
    entries: BTreeMap<String, Entry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    size: u64,
    modified: Option<SystemTime>,
    content: Digest,
}

impl DirectoryState {
    /// The state of `dir`, without the entries at its top that `skip` names.
    pub fn of(dir: &Path, skip: &[&str]) -> Result<Self, StoreError> {
        DirectoryState::default().again(dir, skip)
    }

    /// The state of `dir` now. A file with the size and time that it had in `self` keeps its content digest, so only
    /// files that changed are read.
    pub fn again(&self, dir: &Path, skip: &[&str]) -> Result<Self, StoreError> {
        let mut entries = BTreeMap::new();
        self.collect(dir, "", skip, &mut entries)?;
        Ok(DirectoryState { entries })
    }

    /// The size of all files.
    pub fn size(&self) -> u64 {
        self.entries.values().map(|entry| entry.size).sum()
    }

    /// How `other` differs from this state: the size of the files that it changed, added or removed, and the largest
    /// of them. A size of zero means the same paths with the same content.
    pub fn difference(&self, other: &DirectoryState) -> (u64, Option<String>) {
        let changed = other
            .entries
            .iter()
            .filter(|(path, entry)| self.entries.get(*path).map(|before| before.content) != Some(entry.content));
        let removed = self.entries.iter().filter(|(path, _)| !other.entries.contains_key(*path));
        let differing: Vec<(&String, u64)> = changed.chain(removed).map(|(path, entry)| (path, entry.size)).collect();
        let largest = differing.iter().max_by_key(|(_, size)| *size).map(|(path, _)| (*path).clone());
        (differing.iter().map(|(_, size)| size).sum(), largest)
    }

    fn collect(
        &self,
        dir: &Path,
        prefix: &str,
        skip: &[&str],
        entries: &mut BTreeMap<String, Entry>,
    ) -> Result<(), StoreError> {
        let listing = match fs::read_dir(dir) {
            Ok(listing) => listing,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(StoreError::io(dir, e)),
        };
        for entry in listing {
            let entry = entry.map_err(|e| StoreError::io(dir, e))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if prefix.is_empty() && skip.contains(&name.as_str()) {
                continue;
            }
            let relative = format!("{prefix}/{name}");
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(|e| StoreError::io(&path, e))?;
            if metadata.is_dir() {
                self.collect(&path, &relative, skip, entries)?;
                continue;
            }
            let (size, modified) = (metadata.len(), metadata.modified().ok());
            let known = self.entries.get(&relative).filter(|known| known.size == size && known.modified == modified);
            let content = match known {
                Some(known) => known.content,
                None => content_of(&path, &metadata)?,
            };
            entries.insert(relative, Entry { size, modified, content });
        }
        Ok(())
    }
}

fn content_of(path: &Path, metadata: &fs::Metadata) -> Result<Digest, StoreError> {
    if metadata.is_symlink() {
        let target = fs::read_link(path).map_err(|e| StoreError::io(path, e))?;
        return Ok(Digest::of_bytes(target.to_string_lossy().as_bytes()));
    }
    let mut file = File::open(path).map_err(|e| StoreError::io(path, e))?;
    let mut hasher = Hasher::new();
    io::copy(&mut file, &mut hasher).map_err(|e| StoreError::io(path, e))?;
    Ok(hasher.finish())
}
