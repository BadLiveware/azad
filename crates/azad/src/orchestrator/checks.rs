//! Check runs, as GitHub has them: one for each goal on a commit, which is queued, in progress, and then completed with
//! a conclusion. Locally they go to a fake GitHub, which keeps them in files that `azad-orchestrator checks` shows; a
//! GitHub App (`github::GitHubApp`) takes the same calls through GitHub's API.

use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const CHECK_EXTENSION: &str = "json";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Queued,
    InProgress,
    Completed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Conclusion {
    Success,
    Failure,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckRun {
    pub id: u64,
    pub repository: String,
    pub head_sha: String,
    pub name: String,
    pub status: CheckStatus,
    pub conclusion: Option<Conclusion>,
    /// What happened, for people.
    pub summary: String,
}

impl fmt::Display for CheckRun {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = match (self.status, self.conclusion) {
            (_, Some(Conclusion::Success)) => "success",
            (_, Some(Conclusion::Failure)) => "failure",
            (CheckStatus::Queued, None) => "queued",
            (CheckStatus::InProgress, None) | (CheckStatus::Completed, None) => "running",
        };
        let short_sha = self.head_sha.get(..12).unwrap_or(&self.head_sha);
        write!(f, "{state:<8} {} {short_sha} {}", self.repository, self.name)?;
        if !self.summary.is_empty() {
            write!(f, ": {}", self.summary.lines().next().unwrap_or_default())?;
        }
        Ok(())
    }
}

/// Where check runs go.
pub trait CheckRuns: Send + Sync + fmt::Debug {
    /// Creates a queued check run, and returns its ID.
    fn create(&self, repository: &str, head_sha: &str, name: &str) -> io::Result<u64>;
    fn update(
        &self,
        repository: &str,
        id: u64,
        status: CheckStatus,
        conclusion: Option<Conclusion>,
        summary: &str,
    ) -> io::Result<()>;
}

/// Check runs in files, one for each, named by its ID.
#[derive(Clone, Debug)]
pub struct FakeGitHub {
    dir: PathBuf,
}

impl FakeGitHub {
    pub fn new(dir: PathBuf) -> Self {
        FakeGitHub { dir }
    }

    /// Every check run, oldest first.
    pub fn check_runs(&self) -> io::Result<Vec<CheckRun>> {
        let listing = match fs::read_dir(&self.dir) {
            Ok(listing) => listing,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut runs = Vec::new();
        for entry in listing {
            let path = entry?.path();
            let is_check = path.extension().is_some_and(|extension| extension == CHECK_EXTENSION);
            if is_check && !is_reserved(&path)? {
                runs.push(read(&path)?);
            }
        }
        runs.sort_by_key(|run| run.id);
        Ok(runs)
    }

    fn path(&self, id: u64) -> PathBuf {
        self.dir.join(format!("{id:010}.{CHECK_EXTENSION}"))
    }

    fn write(&self, run: &CheckRun) -> io::Result<()> {
        let mut file = tempfile::NamedTempFile::new_in(&self.dir)?;
        file.write_all(&serde_json::to_vec_pretty(run).map_err(io::Error::other)?)?;
        file.persist(self.path(run.id)).map_err(|e| e.error)?;
        Ok(())
    }
}

impl CheckRuns for FakeGitHub {
    fn create(&self, repository: &str, head_sha: &str, name: &str) -> io::Result<u64> {
        fs::create_dir_all(&self.dir)?;
        let mut id = self.check_runs()?.last().map_or(1, |run| run.id + 1);
        loop {
            match fs::OpenOptions::new().write(true).create_new(true).open(self.path(id)) {
                Ok(_) => break,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => id += 1,
                Err(e) => return Err(e),
            }
        }
        let run = CheckRun {
            id,
            repository: repository.to_owned(),
            head_sha: head_sha.to_owned(),
            name: name.to_owned(),
            status: CheckStatus::Queued,
            conclusion: None,
            summary: String::new(),
        };
        self.write(&run)?;
        Ok(id)
    }

    fn update(
        &self,
        _repository: &str,
        id: u64,
        status: CheckStatus,
        conclusion: Option<Conclusion>,
        summary: &str,
    ) -> io::Result<()> {
        let mut run = read(&self.path(id))?;
        run.status = status;
        run.conclusion = conclusion;
        run.summary = summary.to_owned();
        self.write(&run)
    }
}

/// An empty file holds an ID for a check run that is being created.
fn is_reserved(path: &Path) -> io::Result<bool> {
    Ok(fs::metadata(path)?.len() == 0)
}

fn read(path: &Path) -> io::Result<CheckRun> {
    serde_json::from_slice(&fs::read(path)?).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}
