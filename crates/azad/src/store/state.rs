//! The state of a store beside its blobs: the action cache, aliases, and the records of finished runs. Each entry is a
//! small file named by a digest, written to a temporary file and renamed into place, so readers never see a partial
//! entry and several processes can share a store. See decision 0009.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::ir::{ActionResult, Digest};
use tempfile::NamedTempFile;

use crate::store::{Store, StoreError};
use crate::trust::Trust;

const ACTION_CACHE_DIR: &str = "ac";
/// The results of runs of pull requests (decision 0020).
const PULL_REQUEST_ACTION_CACHE_DIR: &str = "ac-pull-request";
const ALIASES_DIR: &str = "aliases";
const RUNS_DIR: &str = "runs";
const GOAL_FILE: &str = "goal";
const RUN_NUMBER_WIDTH: usize = 10;

/// The action cache, aliases and run records of a store.
#[derive(Debug, Clone)]
pub struct State {
    store: Store,
}

impl State {
    pub(crate) fn new(store: Store) -> Self {
        State { store }
    }

    /// The result of an action in the action cache of `main`, when it is known and everything it names is still in
    /// the store.
    pub fn result(&self, action: &Digest) -> Result<Option<ActionResult>, StoreError> {
        self.result_in(ACTION_CACHE_DIR, action)
    }

    /// Records a result in the action cache of `main`.
    pub fn put_result(&self, action: &Digest, result: &ActionResult) -> Result<(), StoreError> {
        self.put_result_in(ACTION_CACHE_DIR, action, result)
    }

    /// The action cache as a run of a trust level sees it.
    pub fn action_cache(&self, trust: Trust) -> ActionCache {
        ActionCache { state: self.clone(), trust }
    }

    fn result_in(&self, dir: &str, action: &Digest) -> Result<Option<ActionResult>, StoreError> {
        let Some(digest) = self.read_digest(&self.digest_path(dir, action))? else {
            return Ok(None);
        };
        let result: ActionResult = match self.store.get(&digest) {
            Ok(result) => result,
            Err(StoreError::Missing(_)) => return Ok(None),
            Err(e) => return Err(e),
        };
        let complete = result.logs().chain(result.outputs.values()).all(|d| self.store.contains(d));
        Ok(complete.then_some(result))
    }

    fn put_result_in(&self, dir: &str, action: &Digest, result: &ActionResult) -> Result<(), StoreError> {
        let digest = self.store.put(result)?;
        self.write(&self.digest_path(dir, action), digest.to_string().as_bytes())
    }

    /// A digest that the store remembers under a name, such as the tree of a pulled image. The digest must still be in
    /// the store.
    pub fn alias(&self, key: &str) -> Result<Option<Digest>, StoreError> {
        let path = self.digest_path(ALIASES_DIR, &Digest::of_bytes(key.as_bytes()));
        let Some(digest) = self.read_digest(&path)? else {
            return Ok(None);
        };
        Ok(self.store.contains(&digest).then_some(digest))
    }

    pub fn set_alias(&self, key: &str, digest: &Digest) -> Result<(), StoreError> {
        let path = self.digest_path(ALIASES_DIR, &Digest::of_bytes(key.as_bytes()));
        self.write(&path, digest.to_string().as_bytes())
    }

    /// Records a finished run of a goal, and returns its number: 1 for the first run of the goal.
    pub fn record_run(&self, goal: &str, record: &[u8]) -> Result<u64, StoreError> {
        let dir = self.runs_dir(goal);
        fs::create_dir_all(&dir).map_err(|e| StoreError::io(&dir, e))?;
        let goal_file = dir.join(GOAL_FILE);
        if !goal_file.is_file() {
            self.write(&goal_file, goal.as_bytes())?;
        }
        let mut number = self.last_number_in(&dir)? + 1;
        loop {
            let path = dir.join(run_file_name(number));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    file.write_all(record).map_err(|e| StoreError::io(&path, e))?;
                    return Ok(number);
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => number += 1,
                Err(e) => return Err(StoreError::io(&path, e)),
            }
        }
    }

    /// The number and the record of the last run of a goal.
    pub fn last_run(&self, goal: &str) -> Result<Option<(u64, Vec<u8>)>, StoreError> {
        let dir = self.runs_dir(goal);
        let number = self.last_number_in(&dir)?;
        if number == 0 {
            return Ok(None);
        }
        let path = dir.join(run_file_name(number));
        let record = fs::read(&path).map_err(|e| StoreError::io(&path, e))?;
        Ok(Some((number, record)))
    }

    /// The record of a run of a goal by its number, when the store has it.
    pub fn run(&self, goal: &str, number: u64) -> Result<Option<Vec<u8>>, StoreError> {
        let path = self.runs_dir(goal).join(run_file_name(number));
        match fs::read(&path) {
            Ok(record) => Ok(Some(record)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(StoreError::io(&path, e)),
        }
    }

    /// The number of the last run of a goal; 0 when the goal has no recorded run.
    pub fn last_run_number(&self, goal: &str) -> Result<u64, StoreError> {
        self.last_number_in(&self.runs_dir(goal))
    }

    fn last_number_in(&self, dir: &Path) -> Result<u64, StoreError> {
        let listing = match fs::read_dir(dir) {
            Ok(listing) => listing,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(StoreError::io(dir, e)),
        };
        let mut last = 0;
        for entry in listing {
            let entry = entry.map_err(|e| StoreError::io(dir, e))?;
            if let Some(number) = entry.file_name().to_str().and_then(|n| n.parse::<u64>().ok()) {
                last = last.max(number);
            }
        }
        Ok(last)
    }

    fn runs_dir(&self, goal: &str) -> PathBuf {
        self.store.root().join(RUNS_DIR).join(Digest::of_bytes(goal.as_bytes()).hex())
    }

    fn digest_path(&self, dir: &str, key: &Digest) -> PathBuf {
        let hex = key.hex();
        self.store.root().join(dir).join(key.function().name()).join(&hex[..2]).join(hex)
    }

    fn read_digest(&self, path: &Path) -> Result<Option<Digest>, StoreError> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(StoreError::io(path, e)),
        };
        let digest =
            text.trim().parse::<Digest>().map_err(|e| StoreError::Invalid(format!("{}: {e}", path.display())))?;
        Ok(Some(digest))
    }

    /// Replaces the content of a file in one step.
    fn write(&self, path: &Path, content: &[u8]) -> Result<(), StoreError> {
        let parent = path.parent().expect("a state path has a parent");
        fs::create_dir_all(parent).map_err(|e| StoreError::io(parent, e))?;
        let tmp_dir = self.store.tmp_dir();
        let mut tmp = NamedTempFile::new_in(&tmp_dir).map_err(|e| StoreError::io(&tmp_dir, e))?;
        tmp.write_all(content).map_err(|e| StoreError::io(tmp.path(), e))?;
        tmp.persist(path).map_err(|e| StoreError::io(path, e.error))?;
        Ok(())
    }
}

fn run_file_name(number: u64) -> String {
    format!("{number:0RUN_NUMBER_WIDTH$}")
}

/// The action cache as a run of a trust level sees it (decision 0020). A run reads the results of `main` and of its own
/// level, and writes only to its own level: a result that a pull request made never becomes a result that `main`
/// reads. A run of the level `none` writes nothing.
#[derive(Debug, Clone)]
pub struct ActionCache {
    state: State,
    trust: Trust,
}

impl ActionCache {
    pub fn trust(&self) -> Trust {
        self.trust
    }

    pub fn result(&self, action: &Digest) -> Result<Option<ActionResult>, StoreError> {
        for dir in self.reads() {
            if let Some(result) = self.state.result_in(dir, action)? {
                return Ok(Some(result));
            }
        }
        Ok(None)
    }

    pub fn put_result(&self, action: &Digest, result: &ActionResult) -> Result<(), StoreError> {
        match self.writes() {
            Some(dir) => self.state.put_result_in(dir, action, result),
            None => Ok(()),
        }
    }

    fn reads(&self) -> &'static [&'static str] {
        match self.trust {
            Trust::Main | Trust::None => &[ACTION_CACHE_DIR],
            Trust::PullRequest => &[ACTION_CACHE_DIR, PULL_REQUEST_ACTION_CACHE_DIR],
        }
    }

    fn writes(&self) -> Option<&'static str> {
        match self.trust {
            Trust::Main => Some(ACTION_CACHE_DIR),
            Trust::PullRequest => Some(PULL_REQUEST_ACTION_CACHE_DIR),
            Trust::None => None,
        }
    }
}
