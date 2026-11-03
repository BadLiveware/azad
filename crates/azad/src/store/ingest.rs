use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::ir::tree::check_name;
use crate::ir::{Digest, Directory, Node};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

use crate::store::{Store, StoreError};

const EXECUTABLE_BITS: u32 = 0o111;

/// Which entries of a directory go into a tree.
#[derive(Debug, Clone)]
pub enum Selection {
    /// Every entry, also empty directories.
    All,
    /// The files and links whose path, relative to the root, matches an `include` glob and no `exclude` glob. A
    /// directory is in the tree only when it holds a selected entry. `*` does not match `/`; `**` does. `reach` says
    /// which directories can hold a match, so that the others are not read.
    Globs { include: GlobSet, exclude: GlobSet, reach: Vec<Reach> },
    /// Exactly these files and links, by path relative to the root. A directory is in the tree only when it holds one.
    Paths(BTreeSet<PathBuf>),
}

impl Selection {
    pub fn globs<S: AsRef<str>>(include: &[S], exclude: &[S]) -> Result<Self, StoreError> {
        let reach = include.iter().map(|pattern| Reach::of(pattern.as_ref())).collect();
        Ok(Selection::Globs { include: glob_set(include)?, exclude: glob_set(exclude)?, reach })
    }

    /// Whether the selection keeps a file or link, by its path relative to the root.
    pub fn keeps_leaf(&self, relative: &Path) -> bool {
        match self {
            Selection::All => true,
            Selection::Globs { include, exclude, .. } => include.is_match(relative) && !exclude.is_match(relative),
            Selection::Paths(paths) => paths.contains(relative),
        }
    }

    /// Whether a directory can hold a selected entry, so that it is worth reading.
    fn may_hold(&self, relative: &Path) -> bool {
        match self {
            Selection::Paths(paths) => paths.iter().any(|p| p.starts_with(relative)),
            Selection::Globs { reach, .. } => reach.iter().any(|reach| reach.may_hold(relative)),
            Selection::All => true,
        }
    }

    fn keeps_empty_directories(&self) -> bool {
        matches!(self, Selection::All)
    }

    /// The files and links below `root` that `Store::ingest` would store, by path relative to `root`, sorted.
    pub fn list(&self, root: &Path) -> Result<Vec<PathBuf>, StoreError> {
        let mut found = Vec::new();
        self.list_into(root, Path::new(""), &mut found)?;
        found.sort();
        Ok(found)
    }

    fn list_into(&self, path: &Path, relative: &Path, found: &mut Vec<PathBuf>) -> Result<(), StoreError> {
        for entry in fs::read_dir(path).map_err(|e| StoreError::io(path, e))? {
            let entry = entry.map_err(|e| StoreError::io(path, e))?;
            let entry_relative = relative.join(entry.file_name());
            let file_type = entry.file_type().map_err(|e| StoreError::io(&entry.path(), e))?;
            if file_type.is_dir() {
                if self.may_hold(&entry_relative) {
                    self.list_into(&entry.path(), &entry_relative, found)?;
                }
                continue;
            }
            if self.keeps_leaf(&entry_relative) {
                found.push(entry_relative);
            }
        }
        Ok(())
    }
}

/// The characters that make a segment of a pattern more than its literal name.
const GLOB_CHARACTERS: [char; 5] = ['*', '?', '[', '{', '\\'];
const ANY_DEPTH: &str = "**";

/// Where the files that a pattern matches can be: below the directory before its first wildcard, and at most `depth`
/// levels below it, or at any depth when the pattern has `**`.
#[derive(Debug, Clone)]
pub struct Reach {
    prefix: Vec<String>,
    depth: Option<usize>,
}

impl Reach {
    fn of(pattern: &str) -> Self {
        let segments: Vec<&str> = pattern.split('/').collect();
        let wildcard = segments.iter().position(|s| s.contains(GLOB_CHARACTERS)).unwrap_or(segments.len() - 1);
        let rest = &segments[wildcard..];
        let depth = match rest.iter().any(|segment| segment.contains(ANY_DEPTH)) {
            true => None,
            false => Some(rest.len()),
        };
        Reach { prefix: segments[..wildcard].iter().map(|s| (*s).to_owned()).collect(), depth }
    }

    /// Whether the directory `relative` can hold a file that the pattern matches: it is on the way to the pattern's
    /// directory, or below it and not too deep.
    fn may_hold(&self, relative: &Path) -> bool {
        let Some(dir) = relative.iter().map(|part| part.to_str()).collect::<Option<Vec<&str>>>() else {
            return true;
        };
        let shared = dir.len().min(self.prefix.len());
        if dir[..shared] != self.prefix[..shared] {
            return false;
        }
        match self.depth {
            None => true,
            Some(depth) => dir.len() < self.prefix.len() + depth,
        }
    }
}

fn glob_set<S: AsRef<str>>(patterns: &[S]) -> Result<GlobSet, StoreError> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob = GlobBuilder::new(pattern.as_ref())
            .literal_separator(true)
            .build()
            .map_err(|e| StoreError::Invalid(e.to_string()))?;
        builder.add(glob);
    }
    builder.build().map_err(|e| StoreError::Invalid(e.to_string()))
}

impl Store {
    /// Stores the selected content of a directory, and returns the digest of its tree. Timestamps, owners and all mode
    /// bits except "executable" do not change the digest.
    pub fn ingest(&self, root: &Path, selection: &Selection) -> Result<Digest, StoreError> {
        let directory = self.ingest_directory(root, Path::new(""), selection)?;
        self.put(&directory.unwrap_or_default())
    }

    fn ingest_directory(
        &self,
        path: &Path,
        relative: &Path,
        selection: &Selection,
    ) -> Result<Option<Directory>, StoreError> {
        let mut entries = BTreeMap::new();
        let listing = fs::read_dir(path).map_err(|e| StoreError::io(path, e))?;
        for entry in listing {
            let entry = entry.map_err(|e| StoreError::io(path, e))?;
            let entry_path = entry.path();
            let name = entry.file_name().into_string().map_err(|_| StoreError::NotUtf8(entry_path.clone()))?;
            check_name(&name).map_err(|e| StoreError::Invalid(e.to_string()))?;
            let entry_relative = relative.join(&name);
            if let Some(node) = self.ingest_entry(&entry_path, &entry_relative, selection)? {
                entries.insert(name, node);
            }
        }
        if entries.is_empty() && !selection.keeps_empty_directories() {
            return Ok(None);
        }
        Directory::new(entries).map(Some).map_err(|e| StoreError::Invalid(e.to_string()))
    }

    fn ingest_entry(&self, path: &Path, relative: &Path, selection: &Selection) -> Result<Option<Node>, StoreError> {
        let metadata = fs::symlink_metadata(path).map_err(|e| StoreError::io(path, e))?;
        let file_type = metadata.file_type();
        if file_type.is_dir() {
            if !selection.may_hold(relative) {
                return Ok(None);
            }
            let Some(directory) = self.ingest_directory(path, relative, selection)? else {
                return Ok(None);
            };
            return Ok(Some(Node::Directory { digest: self.put(&directory)? }));
        }
        if !selection.keeps_leaf(relative) {
            return Ok(None);
        }
        if file_type.is_symlink() {
            let target = fs::read_link(path).map_err(|e| StoreError::io(path, e))?;
            let target = target.into_os_string().into_string().map_err(|_| StoreError::NotUtf8(path.to_owned()))?;
            return Ok(Some(Node::Symlink { target }));
        }
        if file_type.is_file() {
            let digest = self.put_file(path)?;
            let executable = metadata.permissions().mode() & EXECUTABLE_BITS != 0;
            return Ok(Some(Node::File { digest, executable }));
        }
        Err(StoreError::UnsupportedFileType(path.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn may_hold(pattern: &str, dir: &str) -> bool {
        Reach::of(pattern).may_hold(Path::new(dir))
    }

    #[test]
    fn a_pattern_reaches_only_the_directories_that_can_hold_its_files() {
        assert!(may_hold("libs/core/*.cs", "libs"));
        assert!(may_hold("libs/core/*.cs", "libs/core"));
        assert!(!may_hold("libs/core/*.cs", "libs/core/bin"));
        assert!(!may_hold("libs/core/*.cs", "apps"));
        assert!(may_hold("libs/*/package.json", "libs/web"));
        assert!(!may_hold("libs/*/package.json", "libs/web/node_modules"));
        assert!(!may_hold("Directory.Build.props", "src"));
        assert!(may_hold("libs/**/*.cs", "libs/core/deep/er"));
        assert!(may_hold("*/{a,b/c}.txt", "x/b"));
        assert!(may_hold("**", "any/where"));
    }
}
