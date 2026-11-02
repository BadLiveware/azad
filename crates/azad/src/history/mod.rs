//! The history of the workspace's repository: the facts of commits, which their IDs fix (decision 0021).
//!
//! The local repository is the first source. A shallow clone lacks the history before its boundary; another source,
//! such as GitHub's API, can give what is missing (decision 0021, amendment). Each answer says which source gave it, so
//! that a note can tell the user.

mod github;
mod local;

pub use github::GitHubHistory;
pub use local::LocalRepository;

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

/// The facts of a commit that its ID fixes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitFacts {
    pub id: String,
    /// The committer date, in seconds since 1970-01-01 UTC.
    pub time: i64,
    /// The IDs of the parents, the first parent first.
    pub parents: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    #[error("{0}")]
    Read(String),
    /// The local clone is shallow, and no other source is given.
    #[error(
        "to read {what} of {id}, Azad needs history that this clone does not have, because it is shallow; fetch the whole history \
         (`git fetch --unshallow`), or give Azad another source of history (`azad --github-history`, which the Azad \
         action sets in GitHub Actions)"
    )]
    Shallow { what: String, id: String },
}

/// A source of history other than the local repository.
pub trait History: Send + Sync + fmt::Debug {
    /// What the facts come from, for notes, such as `GitHub's API (owner/name)`.
    fn name(&self) -> String;
    /// How many commits `id` contains: itself and all of its ancestors.
    fn count(&self, id: &str) -> Result<u64, HistoryError>;
    /// The commit `id` and its first parents, `id` first, at most `limit` of them.
    fn first_parents(&self, id: &str, limit: usize) -> Result<Vec<CommitFacts>, HistoryError>;
}

/// Where an answer came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Local,
    /// Another source, by its name, because the clone is shallow.
    Fallback(String),
}

/// The local repository of the workspace, and another source for what a shallow clone lacks.
#[derive(Clone, Debug)]
pub struct Lookup {
    pub local: LocalRepository,
    pub fallback: Option<Arc<dyn History>>,
}

/// The counts that are known, and their sources, by repository and commit ID.
type Counts = HashMap<(PathBuf, String), (u64, Source)>;

/// The counts that are known: a commit's history never changes.
fn counts() -> &'static Mutex<Counts> {
    static COUNTS: OnceLock<Mutex<Counts>> = OnceLock::new();
    COUNTS.get_or_init(Mutex::default)
}

/// A commit and its first parents, the commit first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chain {
    pub commits: Vec<CommitFacts>,
    /// How many commits at the end came from the other source, and its name; `None` when all came from the clone.
    pub borrowed: Option<(usize, String)>,
}

impl Lookup {
    /// How many commits `id` contains, and where the count came from.
    pub fn count(&self, id: &str) -> Result<(u64, Source), HistoryError> {
        let key = (self.local.common_dir().to_path_buf(), id.to_owned());
        if let Some(known) = counts().lock().expect("the counts are never poisoned").get(&key) {
            return Ok(known.clone());
        }
        let counted = match (self.local.count(id), &self.fallback) {
            (Err(HistoryError::Shallow { .. }), Some(fallback)) => {
                (fallback.count(id)?, Source::Fallback(fallback.name()))
            }
            (counted, _) => (counted?, Source::Local),
        };
        counts().lock().expect("the counts are never poisoned").insert(key, counted.clone());
        Ok(counted)
    }

    /// The commit `id` and its first parents, at most `limit`.
    pub fn first_parents(&self, id: &str, limit: usize) -> Result<Chain, HistoryError> {
        let (mut chain, complete) = self.local.first_parents(id, limit)?;
        if complete {
            return Ok(Chain { commits: chain, borrowed: None });
        }
        let Some(fallback) = &self.fallback else {
            return Err(HistoryError::Shallow { what: format!("{limit} first parents"), id: id.to_owned() });
        };
        let last = chain.last().expect("the chain starts with the commit");
        let parent = last.parents.first().expect("an incomplete chain ends at a commit with a parent").clone();
        let rest = fallback.first_parents(&parent, limit - chain.len())?;
        let added = rest.len();
        chain.extend(rest);
        Ok(Chain { commits: chain, borrowed: Some((added, fallback.name())) })
    }
}
