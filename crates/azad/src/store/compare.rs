//! The paths that differ between two trees, so that `azad why` can name the files that changed.

use std::collections::BTreeSet;

use crate::ir::{Digest, Directory, Node};
use crate::store::{Store, StoreError};

const SEPARATOR: &str = "/";

/// How a path differs between an earlier tree and a later one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathChange {
    Added,
    Removed,
    Changed,
}

/// A path that differs between two trees. A directory that only one tree has is one path, which ends in `/`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedPath {
    pub path: String,
    pub change: PathChange,
}

impl Store {
    /// The paths that differ between the tree `old` and the tree `new`, in the order of their names. A directory
    /// that both trees have with the same digest is not read.
    pub fn compare_trees(&self, old: &Digest, new: &Digest) -> Result<Vec<ChangedPath>, StoreError> {
        let mut changes = Vec::new();
        self.compare_directories(old, new, "", &mut changes)?;
        Ok(changes)
    }

    fn compare_directories(
        &self,
        old: &Digest,
        new: &Digest,
        prefix: &str,
        changes: &mut Vec<ChangedPath>,
    ) -> Result<(), StoreError> {
        if old == new {
            return Ok(());
        }
        let old: Directory = self.get(old)?;
        let new: Directory = self.get(new)?;
        let names: BTreeSet<&String> = old.entries().keys().chain(new.entries().keys()).collect();
        for name in names {
            let path = format!("{prefix}{name}");
            match (old.entries().get(name), new.entries().get(name)) {
                (Some(Node::Directory { digest: a }), Some(Node::Directory { digest: b })) => {
                    self.compare_directories(a, b, &format!("{path}{SEPARATOR}"), changes)?;
                }
                (Some(a), Some(b)) if a == b => {}
                (Some(_), Some(_)) => changes.push(ChangedPath { path, change: PathChange::Changed }),
                (Some(node), None) => {
                    changes.push(ChangedPath { path: shown(path, node), change: PathChange::Removed })
                }
                (None, Some(node)) => changes.push(ChangedPath { path: shown(path, node), change: PathChange::Added }),
                (None, None) => unreachable!("each name comes from one of the two directories"),
            }
        }
        Ok(())
    }
}

/// A path as it is shown: a directory ends in `/`.
fn shown(path: String, node: &Node) -> String {
    match node {
        Node::Directory { .. } => format!("{path}{SEPARATOR}"),
        _ => path,
    }
}
