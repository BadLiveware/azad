//! The durable state of the orchestrator's runs: one directory for each event that it accepted, which holds the event,
//! the IDs of the check runs, and a mark when the run is done. A run that is not done when the orchestrator stops runs
//! when it starts again.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::orchestrator::event::Event;

const RUNS_DIR: &str = "runs";
const DELIVERIES_DIR: &str = "deliveries";
const EVENT_FILE: &str = "event.json";
const CHECKS_FILE: &str = "checks.json";
const DONE_FILE: &str = "done";
const ID_WIDTH: usize = 10;

#[derive(Debug)]
pub(super) struct Runs {
    dir: PathBuf,
}

impl Runs {
    pub(super) fn new(state: &Path) -> Self {
        Runs { dir: state.to_path_buf() }
    }

    /// Records an event as a new run, and returns its ID; none when a delivery with the same ID came before, because
    /// GitHub delivers an event again when it is not sure that the first delivery arrived.
    pub(super) fn add(&self, event: &Event, delivery: Option<&str>) -> io::Result<Option<u64>> {
        let runs = self.dir.join(RUNS_DIR);
        fs::create_dir_all(&runs)?;
        if let Some(delivery) = delivery
            && !self.first_delivery(delivery)?
        {
            return Ok(None);
        }
        let mut id = self.ids()?.last().map_or(1, |id| id + 1);
        let staged = tempfile::TempDir::new_in(&runs)?;
        write_file(&staged.path().join(EVENT_FILE), &serde_json::to_vec_pretty(event).map_err(io::Error::other)?)?;
        let staged = staged.keep();
        loop {
            match fs::rename(&staged, self.run_dir(id)) {
                Ok(()) => return Ok(Some(id)),
                Err(_) if self.run_dir(id).exists() => id += 1,
                Err(e) => return Err(e),
            }
        }
    }

    fn first_delivery(&self, delivery: &str) -> io::Result<bool> {
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
            let dir = self.run_dir(id);
            if dir.join(DONE_FILE).exists() {
                continue;
            }
            let event = serde_json::from_slice(&fs::read(dir.join(EVENT_FILE))?)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            pending.push((id, event));
        }
        Ok(pending)
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

    fn run_dir(&self, id: u64) -> PathBuf {
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
