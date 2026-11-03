//! The server half: the views of the pages, from the runs' records in the orchestrator's state (decision 0034), and a
//! stream of a running step's new lines. It only reads the state.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use axum::extract::{Path as UrlPath, Query};
use axum::http::HeaderMap;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use azad::orchestrator::Conclusion;
use azad::orchestrator::record::{self, Goal, RECORD_FILE, RecordEvent, Run, Step, StepState};
use azad::orchestrator::report::{duration, icon, title};
use futures::Stream;
use serde::Deserialize;

use crate::ansi::segments;
use crate::model::{About, GoalView, Line, RunRow, RunView, Standing, StepPage, StepView};

const GITHUB: &str = "https://github.com";
const LOCAL_OWNER: &str = "local/";
const SHORT_SHA: usize = 12;
const TIME_FORMAT: &str = "%Y-%m-%d %H:%M:%S UTC";
/// How often a stream of a running step looks for new lines in its run's record.
const POLL: Duration = Duration::from_millis(300);
/// The header in which a browser that connects again names the last event that it got: a byte of the record.
const LAST_EVENT_ID: &str = "last-event-id";
/// Browsers drop an event without data, so the end says what it is.
const END_DATA: &str = "end";

static STATE: OnceLock<PathBuf> = OnceLock::new();

/// Where the orchestrator keeps its state, which the site reads; set once, when the server starts.
pub fn set_state(state: PathBuf) {
    STATE.get_or_init(|| state);
}

fn state() -> Result<&'static Path, String> {
    STATE.get().map(PathBuf::as_path).ok_or_else(|| "the site has no orchestrator state".to_owned())
}

fn run_dir(id: u64) -> Result<PathBuf, String> {
    record::run_dirs(state()?)
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|(found, _)| *found == id)
        .map(|(_, dir)| dir)
        .ok_or_else(|| format!("there is no run #{id}"))
}

pub fn runs() -> Result<Vec<RunRow>, String> {
    let mut rows = Vec::new();
    for (id, dir) in record::run_dirs(state()?).map_err(|e| e.to_string())? {
        let run = Run::read(&dir).unwrap_or_default();
        let done = record::is_done(&dir);
        let now = record::now();
        rows.push(RunRow {
            id,
            standing: run_standing(&run, done),
            about: about(&dir),
            goals: run.goals.iter().map(|goal| goal.name.clone()).collect(),
            started: run.started.map(time),
            took: run.started.map(|started| duration(run_end(&run, done, now).saturating_sub(started))),
        });
    }
    Ok(rows)
}

pub fn run(id: u64) -> Result<RunView, String> {
    let dir = run_dir(id)?;
    let run = Run::read(&dir).map_err(|e| e.to_string())?;
    let done = record::is_done(&dir);
    let now = record::now();
    Ok(RunView {
        id,
        standing: run_standing(&run, done),
        about: about(&dir),
        trust: run.trust.map(|trust| trust.to_string()),
        started: run.started.map(time),
        took: run.started.map(|started| duration(run_end(&run, done, now).saturating_sub(started))),
        failure: run.failure.clone(),
        cancelled: run.cancelled.clone(),
        goals: run.goals.iter().map(|goal| goal_view(goal, now)).collect(),
        done,
    })
}

pub fn step(id: u64, goal_index: usize, step_index: usize) -> Result<StepPage, String> {
    let dir = run_dir(id)?;
    let run = Run::read(&dir).map_err(|e| e.to_string())?;
    let goal = run.goals.get(goal_index).ok_or_else(|| format!("run #{id} has no goal {goal_index}"))?;
    let step = goal.steps.get(step_index).ok_or_else(|| format!("{} has no step {step_index}", goal.name))?;
    let (entries, offset) = record::entries(&dir.join(RECORD_FILE), 0).map_err(|e| e.to_string())?;
    let lines = step_lines(&entries, goal_index, &goal.name, &step.name);
    let now = record::now();
    let span = goal_span(goal, now);
    Ok(StepPage {
        run: id,
        goal_index,
        step_index,
        goal: goal.name.clone(),
        step: step_view(step, goal.started, span, now),
        lines,
        offset,
        millis: step.millis(now),
    })
}

/// The output of a step of the goal that is the `goal_index`th to start in its run: a run that started again after a
/// stop has a goal more than once, and the goals of a run run at the same time, so their lines interleave.
fn step_lines(entries: &[record::Entry], goal_index: usize, goal: &str, step: &str) -> Vec<Line> {
    let mut started = 0;
    let mut current = false;
    let mut lines = Vec::new();
    for entry in entries {
        match &entry.event {
            RecordEvent::GoalStarted { goal: name, .. } => {
                if name == goal {
                    current = started == goal_index;
                }
                started += 1;
            }
            RecordEvent::Line { goal: name, step: line_step, text } if current && name == goal && line_step == step => {
                lines.push(segments(text));
            }
            _ => {}
        }
    }
    lines
}

fn run_standing(run: &Run, done: bool) -> Standing {
    let failed = run.failure.is_some() || run.goals.iter().any(|goal| goal_standing(goal) == Standing::Failed);
    if run.cancelled.is_some() {
        return Standing::Cancelled;
    }
    match (failed, done, run.started.is_some()) {
        (true, _, _) => Standing::Failed,
        (false, true, _) => Standing::Passed,
        (false, false, true) => Standing::Running,
        (false, false, false) => Standing::Queued,
    }
}

fn goal_standing(goal: &Goal) -> Standing {
    match goal.conclusion {
        Some(Conclusion::Success) => Standing::Passed,
        Some(Conclusion::Failure) => Standing::Failed,
        Some(Conclusion::Cancelled) => Standing::Cancelled,
        None if goal.count(StepState::Failed) > 0 => Standing::Failed,
        None => Standing::Running,
    }
}

fn run_end(run: &Run, done: bool, now: u64) -> u64 {
    match done {
        true => run.last.unwrap_or(now),
        false => now,
    }
}

fn goal_span(goal: &Goal, now: u64) -> u64 {
    goal.ended.unwrap_or(now).saturating_sub(goal.started).max(1)
}

fn goal_view(goal: &Goal, now: u64) -> GoalView {
    let span = goal_span(goal, now);
    GoalView {
        name: goal.name.clone(),
        standing: goal_standing(goal),
        title: goal.title.clone().unwrap_or_else(|| title(goal)),
        metrics: metrics(goal, now),
        steps: goal.steps.iter().map(|step| step_view(step, goal.started, span, now)).collect(),
    }
}

fn step_view(step: &Step, goal_started: u64, span: u64, now: u64) -> StepView {
    let bar = match (step.started, step.millis(now)) {
        (Some(started), Some(millis)) => {
            let left = started.saturating_sub(goal_started) as f64 * 100.0 / span as f64;
            Some((left, (millis as f64 * 100.0 / span as f64).max(0.5)))
        }
        _ => None,
    };
    StepView {
        name: step.name.clone(),
        state: step.state.word().to_owned(),
        icon: icon(step.state).to_owned(),
        reason: step.reason.clone(),
        time: step.millis(now).map(duration),
        bar,
        ended: step.state.has_ended(),
    }
}

/// How many steps there are, how many results the cache gave, how long the goal took, and how long its steps ran.
fn metrics(goal: &Goal, now: u64) -> String {
    let total = goal.steps.len();
    let cached = goal.count(StepState::Cached);
    let hits = (cached * 100).checked_div(total).unwrap_or_default();
    let busy: u64 = goal.steps.iter().filter_map(|step| step.millis(now)).sum();
    format!(
        "{total} steps: {} ran, {cached} cached ({hits}% from the cache), {} failed, {} skipped · the goal took {} · \
         its steps ran for {} together",
        goal.count(StepState::Ran),
        goal.count(StepState::Failed),
        goal.count(StepState::Skipped),
        duration(goal.ended.unwrap_or(now).saturating_sub(goal.started)),
        duration(busy),
    )
}

fn about(dir: &Path) -> About {
    let Some(event) = record::event(dir) else { return About::default() };
    let commit = event.head_sha.get(..SHORT_SHA).unwrap_or(&event.head_sha).to_owned();
    let commit_url = match event.repository.full_name.starts_with(LOCAL_OWNER) {
        true => None,
        false => Some(format!("{GITHUB}/{}/commit/{}", event.repository.full_name, event.head_sha)),
    };
    let text = match event.pull_request {
        Some(number) => format!("pull request #{number}"),
        None => format!("push of {}", event.git_ref),
    };
    About { repository: event.repository.full_name, event: text, commit, commit_url }
}

fn time(millis: u64) -> String {
    match jiff::Timestamp::from_millisecond(millis as i64) {
        Ok(timestamp) => timestamp.strftime(TIME_FORMAT).to_string(),
        Err(_) => String::new(),
    }
}

#[derive(Debug, Deserialize)]
pub struct Follow {
    offset: u64,
}

/// What a stream of a running step has left to do.
struct Following {
    record: PathBuf,
    dir: PathBuf,
    goal: String,
    step: String,
    offset: u64,
    pending: VecDeque<SseEvent>,
    ended: bool,
}

/// Follows a running step: each new line of its output, after the record's byte `offset`, is a `line` event with the
/// line's segments in JSON and the byte after its entry as its ID; an `end` event comes when the step, its goal or its
/// run ends. A browser that connects again, as browsers do when a stream breaks, goes on after the last line it got.
pub async fn follow(
    UrlPath((id, goal_index, step_index)): UrlPath<(u64, usize, usize)>,
    Query(Follow { offset }): Query<Follow>,
    headers: HeaderMap,
) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let offset = headers
        .get(LAST_EVENT_ID)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
        .unwrap_or(offset);
    let found = run_dir(id).ok().and_then(|dir| {
        let run = Run::read(&dir).ok()?;
        let goal = run.goals.get(goal_index)?;
        let step = goal.steps.get(step_index)?;
        Some((dir, goal.name.clone(), step.name.clone()))
    });
    let following = found.map(|(dir, goal, step)| Following {
        record: dir.join(RECORD_FILE),
        dir,
        goal,
        step,
        offset,
        pending: VecDeque::new(),
        ended: false,
    });
    let stream = futures::stream::unfold(following, |following| async move {
        let mut following = following?;
        loop {
            if let Some(event) = following.pending.pop_front() {
                return Some((Ok(event), Some(following)));
            }
            if following.ended {
                return None;
            }
            following.read();
            if following.pending.is_empty() && !following.ended {
                tokio::time::sleep(POLL).await;
            }
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

impl Following {
    fn read(&mut self) {
        let Ok(entries) = record::placed_entries(&self.record, self.offset) else {
            self.end();
            return;
        };
        for (entry, end) in entries {
            self.offset = end;
            match entry.event {
                RecordEvent::Line { goal, step, text } if goal == self.goal && step == self.step => {
                    let data = serde_json::to_string(&segments(&text)).unwrap_or_default();
                    let event = SseEvent::default().event(crate::LINE_EVENT).id(end.to_string()).data(data);
                    self.pending.push_back(event);
                }
                RecordEvent::StepEnded { goal, step, .. } if goal == self.goal && step == self.step => self.end(),
                RecordEvent::GoalEnded { goal, .. } if goal == self.goal => self.end(),
                RecordEvent::RunFailed { .. } => self.end(),
                _ => {}
            }
        }
        if !self.ended && record::is_done(&self.dir) {
            self.end();
        }
    }

    fn end(&mut self) {
        if !self.ended {
            self.ended = true;
            self.pending.push_back(SseEvent::default().event(crate::END_EVENT).data(END_DATA));
        }
    }
}
