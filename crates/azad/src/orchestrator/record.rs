//! The record of a run (decision 0034): each event of a run, appended to a file in the run's directory as it happens,
//! one JSON object on each line. The orchestrator writes it; the check runs and the site read it, and a reader never
//! changes it, so the site follows a run while it runs without asking the orchestrator anything.

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::orchestrator::checks::Conclusion;
use crate::scheduler::Status;
use crate::trust::Trust;

pub const RECORD_FILE: &str = "record.jsonl";
const CANCELLED_REASON: &str = "the run was cancelled";
/// How many lines of output a step keeps in a summary of a record; the whole output stays in the record.
pub const TAIL_LINES: usize = 100;

/// The state of a step: one action of a goal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    Queued,
    Running,
    Ran,
    Cached,
    Failed,
    Skipped,
}

impl StepState {
    pub fn word(self) -> &'static str {
        match self {
            StepState::Queued => "queued",
            StepState::Running => "running",
            StepState::Ran => "ran",
            StepState::Cached => "cached",
            StepState::Failed => "failed",
            StepState::Skipped => "skipped",
        }
    }

    pub fn has_ended(self) -> bool {
        !matches!(self, StepState::Queued | StepState::Running)
    }

    /// The state that a finished action's status gives, and why it failed or was skipped.
    pub fn of(status: &Status) -> (StepState, Option<String>) {
        match status {
            Status::Ran { .. } => (StepState::Ran, None),
            Status::Cached { .. } => (StepState::Cached, None),
            Status::Failed { reason, .. } => (StepState::Failed, Some(reason.clone())),
            Status::Skipped => (StepState::Skipped, Some("a step that it needs failed".to_owned())),
            Status::NotNeeded | Status::WillRun { .. } | Status::Waiting => (StepState::Queued, None),
        }
    }
}

/// One event of a run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecordEvent {
    /// The commit is checked out, and its goals run at this trust level.
    RunStarted {
        trust: Trust,
    },
    GoalStarted {
        goal: String,
        steps: Vec<String>,
    },
    StepStarted {
        goal: String,
        step: String,
    },
    Line {
        goal: String,
        step: String,
        text: String,
    },
    StepEnded {
        goal: String,
        step: String,
        state: StepState,
        reason: Option<String>,
    },
    GoalEnded {
        goal: String,
        conclusion: Conclusion,
        title: String,
    },
    /// The run could not go on, such as when its commit cannot be fetched.
    RunFailed {
        reason: String,
    },
    /// The run starts no more steps, because a newer commit of its branch or pull request came.
    RunCancelled {
        reason: String,
    },
}

/// An event, and when it happened, in milliseconds since the Unix epoch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub at: u64,
    #[serde(flatten)]
    pub event: RecordEvent,
}

/// Now, in milliseconds since the Unix epoch.
pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|time| time.as_millis() as u64).unwrap_or_default()
}

/// Appends events to the record of a run.
#[derive(Debug)]
pub struct Recorder {
    file: File,
}

impl Recorder {
    pub fn open(run_dir: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(run_dir.join(RECORD_FILE))?;
        Ok(Recorder { file })
    }

    /// Appends one event as one line, in one write, so that a reader sees whole lines.
    pub fn append(&mut self, entry: &Entry) -> io::Result<()> {
        let mut line = serde_json::to_vec(entry).map_err(io::Error::other)?;
        line.push(b'\n');
        self.file.write_all(&line)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub name: String,
    pub state: StepState,
    pub started: Option<u64>,
    pub ended: Option<u64>,
    pub reason: Option<String>,
    pub lines: usize,
    /// The last `TAIL_LINES` lines of its output, as the action wrote them, with their terminal escapes.
    pub tail: VecDeque<String>,
}

impl Step {
    fn new(name: &str) -> Self {
        Step {
            name: name.to_owned(),
            state: StepState::Queued,
            started: None,
            ended: None,
            reason: None,
            lines: 0,
            tail: VecDeque::new(),
        }
    }

    /// How long it ran, or has run until `now`.
    pub fn millis(&self, now: u64) -> Option<u64> {
        let started = self.started?;
        Some(self.ended.unwrap_or(now).saturating_sub(started))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Goal {
    pub name: String,
    pub started: u64,
    pub ended: Option<u64>,
    pub conclusion: Option<Conclusion>,
    pub title: Option<String>,
    pub steps: Vec<Step>,
}

impl Goal {
    pub fn new(name: &str, steps: &[String], started: u64) -> Self {
        Goal {
            name: name.to_owned(),
            started,
            ended: None,
            conclusion: None,
            title: None,
            steps: steps.iter().map(|step| Step::new(step)).collect(),
        }
    }

    /// Applies an event of this goal.
    pub fn apply(&mut self, at: u64, event: &RecordEvent) {
        match event {
            RecordEvent::StepStarted { step, .. } => {
                let step = self.step(step);
                step.state = StepState::Running;
                step.started = Some(at);
            }
            RecordEvent::Line { step, text, .. } => {
                let step = self.step(step);
                step.lines += 1;
                step.tail.push_back(text.clone());
                if step.tail.len() > TAIL_LINES {
                    step.tail.pop_front();
                }
            }
            RecordEvent::StepEnded { step, state, reason, .. } => {
                let step = self.step(step);
                step.state = *state;
                step.reason = reason.clone();
                step.ended = step.started.map(|_| at);
            }
            RecordEvent::GoalEnded { conclusion, title, .. } => {
                self.ended = Some(at);
                self.conclusion = Some(*conclusion);
                self.title = Some(title.clone());
                if *conclusion == Conclusion::Cancelled {
                    self.skip_waiting_steps(CANCELLED_REASON);
                }
            }
            RecordEvent::RunStarted { .. }
            | RecordEvent::GoalStarted { .. }
            | RecordEvent::RunFailed { .. }
            | RecordEvent::RunCancelled { .. } => {}
        }
    }

    fn step(&mut self, name: &str) -> &mut Step {
        match self.steps.iter().position(|step| step.name == name) {
            Some(index) => &mut self.steps[index],
            None => {
                self.steps.push(Step::new(name));
                self.steps.last_mut().expect("a step was pushed")
            }
        }
    }

    /// The steps that did not start will not start.
    fn skip_waiting_steps(&mut self, reason: &str) {
        for step in self.steps.iter_mut().filter(|step| step.state == StepState::Queued) {
            step.state = StepState::Skipped;
            step.reason = Some(reason.to_owned());
        }
    }

    pub fn count(&self, state: StepState) -> usize {
        self.steps.iter().filter(|step| step.state == state).count()
    }
}

/// A run as its record says it went.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Run {
    pub started: Option<u64>,
    pub trust: Option<Trust>,
    pub goals: Vec<Goal>,
    pub failure: Option<String>,
    /// Why the run was cancelled.
    pub cancelled: Option<String>,
    pub last: Option<u64>,
}

impl Run {
    /// The run of a run's directory; a run whose record has no line yet is empty. A last line that is being written
    /// is left out.
    pub fn read(run_dir: &Path) -> io::Result<Self> {
        let mut run = Run::default();
        for entry in entries(&run_dir.join(RECORD_FILE), 0)?.0 {
            run.apply(&entry);
        }
        Ok(run)
    }

    pub fn apply(&mut self, entry: &Entry) {
        self.started.get_or_insert(entry.at);
        self.last = Some(entry.at);
        match &entry.event {
            RecordEvent::RunStarted { trust } => self.trust = Some(*trust),
            RecordEvent::RunFailed { reason } => self.failure = Some(reason.clone()),
            RecordEvent::RunCancelled { reason } => self.cancelled = Some(reason.clone()),
            RecordEvent::GoalStarted { goal, steps } => self.goals.push(Goal::new(goal, steps, entry.at)),
            RecordEvent::StepStarted { goal, .. }
            | RecordEvent::Line { goal, .. }
            | RecordEvent::StepEnded { goal, .. }
            | RecordEvent::GoalEnded { goal, .. } => {
                if let Some(found) = self.goals.iter_mut().rev().find(|found| found.name == *goal) {
                    found.apply(entry.at, &entry.event);
                }
            }
        }
    }
}

/// The entries of a record from the byte `offset` on, and the offset after the last whole line; a record that does
/// not exist yet has none.
pub fn entries(record: &Path, offset: u64) -> io::Result<(Vec<Entry>, u64)> {
    let placed = placed_entries(record, offset)?;
    let end = placed.last().map_or(offset, |(_, end)| *end);
    Ok((placed.into_iter().map(|(entry, _)| entry).collect(), end))
}

/// The entries of a record from the byte `offset` on, each with the offset after its line, so that a reader can go on
/// after any of them. A last line that is not whole yet is left out.
pub fn placed_entries(record: &Path, offset: u64) -> io::Result<Vec<(Entry, u64)>> {
    let file = match File::open(record) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut reader = BufReader::new(file);
    reader.seek(SeekFrom::Start(offset))?;
    let mut entries = Vec::new();
    let mut read = offset;
    loop {
        let mut line = Vec::new();
        let length = reader.read_until(b'\n', &mut line)?;
        if length == 0 || line.last() != Some(&b'\n') {
            return Ok(entries);
        }
        read += length as u64;
        if let Ok(entry) = serde_json::from_slice(&line) {
            entries.push((entry, read));
        }
    }
}

/// The event that started the run of a run's directory; none while the orchestrator writes it.
pub fn event(run_dir: &Path) -> Option<crate::orchestrator::Event> {
    serde_json::from_slice(&fs::read(run_dir.join(crate::orchestrator::runs::EVENT_FILE)).ok()?).ok()
}

/// Whether the run of a run's directory is done: it will not run again.
pub fn is_done(run_dir: &Path) -> bool {
    run_dir.join(crate::orchestrator::runs::DONE_FILE).exists()
}

/// The directory of each run of an orchestrator's state, by ID, newest first.
pub fn run_dirs(state: &Path) -> io::Result<Vec<(u64, PathBuf)>> {
    let dir = state.join(crate::orchestrator::runs::RUNS_DIR);
    let listing = match fs::read_dir(&dir) {
        Ok(listing) => listing,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut runs: Vec<(u64, PathBuf)> = listing
        .filter_map(Result::ok)
        .filter_map(|entry| Some((entry.file_name().to_str()?.parse().ok()?, entry.path())))
        .collect();
    runs.sort_unstable_by_key(|(id, _)| std::cmp::Reverse(*id));
    Ok(runs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reader_leaves_out_a_line_that_is_being_written_and_takes_it_when_it_is_whole() {
        let dir = tempfile::TempDir::new().expect("a directory");
        let mut recorder = Recorder::open(dir.path()).expect("a record");
        let started = RecordEvent::GoalStarted { goal: "//:check".to_owned(), steps: vec!["//:lint".to_owned()] };
        recorder.append(&Entry { at: 1, event: started }).expect("a line");
        let line = Entry {
            at: 2,
            event: RecordEvent::Line { goal: "//:check".to_owned(), step: "//:lint".to_owned(), text: "ok".to_owned() },
        };
        let whole = serde_json::to_string(&line).expect("JSON");
        let path = dir.path().join(RECORD_FILE);
        let (first, offset) = entries(&path, 0).expect("entries");
        assert_eq!(first.len(), 1);
        let mut file = OpenOptions::new().append(true).open(&path).expect("the record");
        file.write_all(&whole.as_bytes()[..10]).expect("half a line");
        assert_eq!(entries(&path, offset).expect("entries"), (Vec::new(), offset));
        file.write_all(&whole.as_bytes()[10..]).expect("the rest");
        file.write_all(b"\n").expect("the end");
        let (rest, _) = entries(&path, offset).expect("entries");
        assert_eq!(rest, [line]);
        let run = Run::read(dir.path()).expect("the run");
        assert_eq!(run.goals[0].steps[0].tail, ["ok"]);
    }
}
