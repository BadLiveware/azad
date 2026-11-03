//! The durable state of the orchestrator's runs: one directory for each event that it accepted, which holds the event,
//! the IDs of the check runs, a mark when the run was cancelled, and a mark when the run is done. A run that is not done
//! when the orchestrator stops runs when it starts again. A claim names the run that checks a goal on a commit at a
//! trust level, so that one commit gets one check run for each goal.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::ir::Digest;
use crate::orchestrator::event::Event;
use crate::trust::Trust;

pub(super) const RUNS_DIR: &str = "runs";
const DELIVERIES_DIR: &str = "deliveries";
pub(super) const EVENT_FILE: &str = "event.json";
const CHECKS_FILE: &str = "checks.json";
const CANCELLED_FILE: &str = "cancelled";
const CLAIMS_DIR: &str = "claims";
pub(super) const DONE_FILE: &str = "done";
const ID_WIDTH: usize = 10;

#[derive(Debug)]
pub(super) struct Runs {
    dir: PathBuf,
}

impl Runs {
    pub(super) fn new(state: &Path) -> Self {
        Runs { dir: state.to_path_buf() }
    }

    /// Records an event as a new run, and returns its ID.
    pub(super) fn add(&self, event: &Event) -> io::Result<u64> {
        let runs = self.dir.join(RUNS_DIR);
        fs::create_dir_all(&runs)?;
        let mut id = self.ids()?.last().map_or(1, |id| id + 1);
        let staged = tempfile::TempDir::new_in(&runs)?;
        write_file(&staged.path().join(EVENT_FILE), &serde_json::to_vec_pretty(event).map_err(io::Error::other)?)?;
        let staged = staged.keep();
        loop {
            match fs::rename(&staged, self.run_dir(id)) {
                Ok(()) => return Ok(id),
                Err(_) if self.run_dir(id).exists() => id += 1,
                Err(e) => return Err(e),
            }
        }
    }

    /// Whether no delivery with this ID came before: GitHub delivers an event again when it is not sure that the
    /// first delivery arrived.
    pub(super) fn first_delivery(&self, delivery: &str) -> io::Result<bool> {
        let dir = self.dir.join(DELIVERIES_DIR);
        fs::create_dir_all(&dir)?;
        let name: String = delivery.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
        match fs::OpenOptions::new().write(true).create_new(true).open(dir.join(name)) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// The runs that are not done, oldest first.
    pub(super) fn pending(&self) -> io::Result<Vec<(u64, Event)>> {
        let mut pending = Vec::new();
        for id in self.ids()? {
            if self.run_dir(id).join(DONE_FILE).exists() {
                continue;
            }
            pending.push((id, self.event(id)?));
        }
        Ok(pending)
    }

    /// Every run, done or not, oldest first.
    pub(super) fn all(&self) -> io::Result<Vec<(u64, Event)>> {
        self.ids()?.into_iter().map(|id| Ok((id, self.event(id)?))).collect()
    }

    fn event(&self, id: u64) -> io::Result<Event> {
        serde_json::from_slice(&fs::read(self.run_dir(id).join(EVENT_FILE))?)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    /// Marks a run as cancelled, and says why; it starts no more steps, also after a restart.
    pub(super) fn cancel(&self, id: u64, reason: &str) -> io::Result<()> {
        write_file(&self.run_dir(id).join(CANCELLED_FILE), reason.as_bytes())
    }

    /// Why a run was cancelled, when it was.
    pub(super) fn cancelled(&self, id: u64) -> io::Result<Option<String>> {
        match fs::read_to_string(self.run_dir(id).join(CANCELLED_FILE)) {
            Ok(reason) => Ok(Some(reason)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Claims the check of `goal` on a commit at a trust level for run `id`, and says whether the run holds the claim:
    /// the first run that claims it does, also when it claims it again after a restart.
    pub(super) fn claim(&self, event: &Event, trust: Trust, goal: &str, id: u64) -> io::Result<bool> {
        let dir = self.dir.join(CLAIMS_DIR);
        fs::create_dir_all(&dir)?;
        let key = [event.repository.full_name.as_str(), &event.head_sha, &trust.to_string(), goal].join("\0");
        let path = dir.join(Digest::of_bytes(key.as_bytes()).hex());
        match fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                file.write_all(id.to_string().as_bytes())?;
                Ok(true)
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(fs::read_to_string(&path)? == id.to_string()),
            Err(e) => Err(e),
        }
    }

    pub(super) fn finish(&self, id: u64) -> io::Result<()> {
        write_file(&self.run_dir(id).join(DONE_FILE), b"")
    }

    /// The check run of each goal of a run, once they were created.
    pub(super) fn checks(&self, id: u64) -> io::Result<Option<BTreeMap<String, u64>>> {
        match fs::read(self.run_dir(id).join(CHECKS_FILE)) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map(Some).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub(super) fn set_checks(&self, id: u64, checks: &BTreeMap<String, u64>) -> io::Result<()> {
        write_file(&self.run_dir(id).join(CHECKS_FILE), &serde_json::to_vec_pretty(checks).map_err(io::Error::other)?)
    }

    pub(super) fn run_dir(&self, id: u64) -> PathBuf {
        self.dir.join(RUNS_DIR).join(format!("{id:0ID_WIDTH$}"))
    }

    fn ids(&self) -> io::Result<Vec<u64>> {
        let listing = match fs::read_dir(self.dir.join(RUNS_DIR)) {
            Ok(listing) => listing,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut ids: Vec<u64> =
            listing.filter_map(Result::ok).filter_map(|entry| entry.file_name().to_str()?.parse().ok()).collect();
        ids.sort_unstable();
        Ok(ids)
    }
}

/// Writes a file whole, so that a reader never sees a part of it.
fn write_file(path: &Path, content: &[u8]) -> io::Result<()> {
    let dir = path.parent().unwrap_or(path);
    let mut file = tempfile::NamedTempFile::new_in(dir)?;
    file.write_all(content)?;
    file.persist(path).map_err(|e| e.error)?;
    Ok(())
}
