//! Checkouts: a directory that a caller keeps, updated to the content of a tree as a version control checkout is.
//!
//! The files of the store's shared trees are hard links to blobs, so a file has the modification time of the first
//! time its content was stored, even when it came back after a revert. A tool that keeps state between runs, such as
//! Cargo in a cache volume, compares modification times and would take that file as unchanged. In a checkout, a file
//! is written again, with the time of the write, exactly when its content at that path changes.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::ir::{Digest, Directory, Node};
use crate::store::materialize::Placement;
use crate::store::scratch::make_writable;
use crate::store::{Store, StoreError};

/// The extension of the file next to a checkout that holds the digest of its tree.
const STATE_EXTENSION: &str = "tree";

impl Store {
    /// Makes `dest` hold the content of `tree`. Only entries that differ from the tree that `dest` held before are
    /// written, so the files that did not change keep their modification times.
    pub fn check_out(&self, tree: &Digest, dest: &Path) -> Result<(), StoreError> {
        let state = state_path(dest);
        let previous = read_state(&state).filter(|_| dest.is_dir());
        if previous == Some(*tree) {
            return Ok(());
        }
        // Without the state, an update that stops half-way is written again in full the next time.
        remove_if_exists(&state)?;
        match previous {
            Some(previous) => {
                let old: Directory = self.get(&previous)?;
                self.update(&old, tree, dest)?;
            }
            None => {
                remove_if_exists(dest)?;
                self.write_tree(tree, dest, Placement::Copy)?;
            }
        }
        fs::write(&state, tree.to_string()).map_err(|e| StoreError::io(&state, e))
    }

    /// Makes the next `check_out` into `dest` write every file again.
    pub fn forget_checkout(&self, dest: &Path) -> Result<(), StoreError> {
        remove_if_exists(&state_path(dest))
    }

    fn update(&self, old: &Directory, tree: &Digest, dest: &Path) -> Result<(), StoreError> {
        let new: Directory = self.get(tree)?;
        for name in old.entries().keys().filter(|name| !new.entries().contains_key(*name)) {
            remove_if_exists(&dest.join(name))?;
        }
        for (name, node) in new.entries() {
            let path = dest.join(name);
            match (old.entries().get(name), node) {
                (Some(before), _) if before == node => {}
                (Some(Node::Directory { digest: before }), Node::Directory { digest }) => {
                    let before: Directory = self.get(before)?;
                    self.update(&before, digest, &path)?;
                }
                _ => {
                    remove_if_exists(&path)?;
                    self.write_node(node, &path, Placement::Copy)?;
                }
            }
        }
        Ok(())
    }
}

fn state_path(dest: &Path) -> PathBuf {
    dest.with_extension(STATE_EXTENSION)
}

fn read_state(state: &Path) -> Option<Digest> {
    fs::read_to_string(state).ok()?.trim().parse().ok()
}

fn remove_if_exists(path: &Path) -> Result<(), StoreError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(StoreError::io(path, e)),
    };
    if !metadata.is_dir() {
        return fs::remove_file(path).map_err(|e| StoreError::io(path, e));
    }
    make_writable(path)?;
    fs::remove_dir_all(path).map_err(|e| StoreError::io(path, e))
}
