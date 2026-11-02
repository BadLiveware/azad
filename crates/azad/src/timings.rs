//! How long the steps of a run took that are not commands, such as downloads, unpacking and copies of trees, so that a
//! slow run shows where it waits.

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::ir::Digest;

/// Steps shorter than this are not reported: they are not where a run waits.
pub const NOTEWORTHY: Duration = Duration::from_secs(1);
const SHORT_DIGEST: usize = 12;

/// The record of steps of an engine, shared by its parts.
#[derive(Clone, Debug)]
pub struct Timings {
    since: Instant,
    steps: Arc<Mutex<Vec<Step>>>,
}

/// One step: what it did, when it started after the engine started, how long it took, and how many bytes it moved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub what: String,
    pub started: Duration,
    pub took: Duration,
    pub bytes: Option<u64>,
}

/// A step that started. A timer that is dropped without `stop`, such as on an error, records nothing.
#[derive(Debug)]
pub struct Timer {
    timings: Timings,
    what: String,
    started: Instant,
}

impl Default for Timings {
    fn default() -> Self {
        Timings { since: Instant::now(), steps: Arc::new(Mutex::new(Vec::new())) }
    }
}

impl Timings {
    pub fn start(&self, what: impl Into<String>) -> Timer {
        Timer { timings: self.clone(), what: what.into(), started: Instant::now() }
    }

    /// The noteworthy steps since the last call, in the order that they started.
    pub fn take_noteworthy(&self) -> Vec<Step> {
        let mut steps = std::mem::take(&mut *self.steps.lock().expect("the timings lock is not poisoned"));
        steps.retain(|step| step.took >= NOTEWORTHY);
        steps.sort_by_key(|step| step.started);
        steps
    }

    fn record(&self, step: Step) {
        self.steps.lock().expect("the timings lock is not poisoned").push(step);
    }
}

impl Timer {
    pub fn stop(self) {
        self.stop_with(None);
    }

    pub fn stop_with_bytes(self, bytes: u64) {
        self.stop_with(Some(bytes));
    }

    fn stop_with(self, bytes: Option<u64>) {
        let step = Step {
            what: self.what,
            started: self.started.duration_since(self.timings.since),
            took: self.started.elapsed(),
            bytes,
        };
        self.timings.record(step);
    }
}

/// A short name for a tree in a step.
pub fn tree_name(tree: &Digest) -> String {
    let hex = tree.hex();
    format!("tree {}", &hex[..SHORT_DIGEST])
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const MEGABYTE: f64 = 1_000_000.0;
        write!(f, "{:.1}s at {:.1}s: {}", self.took.as_secs_f64(), self.started.as_secs_f64(), self.what)?;
        if let Some(bytes) = self.bytes {
            let megabytes = bytes as f64 / MEGABYTE;
            write!(f, " ({megabytes:.1} MB, {:.1} MB/s)", megabytes / self.took.as_secs_f64())?;
        }
        Ok(())
    }
}
