//! What the pages show, as both halves of the site see it: the server makes it from the runs' records, and the browser
//! gets it from the server.

use serde::{Deserialize, Serialize};

/// How a run or a goal stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Standing {
    Queued,
    Running,
    Passed,
    Failed,
    Cancelled,
}

impl Standing {
    pub fn word(self) -> &'static str {
        match self {
            Standing::Queued => "queued",
            Standing::Running => "running",
            Standing::Passed => "passed",
            Standing::Failed => "failed",
            Standing::Cancelled => "cancelled",
        }
    }
}

/// What a run is about: its repository, the event, and the commit.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct About {
    pub repository: String,
    /// Such as `push of refs/heads/main`, or `pull request #3`.
    pub event: String,
    pub commit: String,
    pub commit_url: Option<String>,
}

/// One run in the list of runs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunRow {
    pub id: u64,
    pub standing: Standing,
    pub about: About,
    pub goals: Vec<String>,
    pub started: Option<String>,
    pub took: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunView {
    pub id: u64,
    pub standing: Standing,
    pub about: About,
    pub trust: Option<String>,
    pub started: Option<String>,
    pub took: Option<String>,
    pub failure: Option<String>,
    /// Why the run was cancelled.
    pub cancelled: Option<String>,
    pub goals: Vec<GoalView>,
    /// Whether the run is done, so that its page stops looking for news.
    pub done: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GoalView {
    pub name: String,
    pub standing: Standing,
    pub title: String,
    pub metrics: String,
    pub steps: Vec<StepView>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StepView {
    pub name: String,
    /// `ran`, `cached`, `failed`, and so on.
    pub state: String,
    pub icon: String,
    pub reason: Option<String>,
    pub time: Option<String>,
    /// Where the step ran in its goal's time, in percent: where it starts, and how wide it is.
    pub bar: Option<(f64, f64)>,
    pub ended: bool,
}

/// A part of a line of output, and its style as classes of the stylesheet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    pub text: String,
    pub class: String,
}

pub type Line = Vec<Segment>;

/// A step with its whole output, and the byte of its run's record after that output, from which its page follows the
/// rest while it runs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StepPage {
    pub run: u64,
    pub goal_index: usize,
    pub step_index: usize,
    pub goal: String,
    pub step: StepView,
    pub lines: Vec<Line>,
    pub offset: u64,
    /// How long the step ran, or has run until the page was made.
    pub millis: Option<u64>,
}
