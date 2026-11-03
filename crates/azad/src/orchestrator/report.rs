//! What a check run shows of its goal (decision 0033): a table of the goal's steps, one for each action, with its state
//! and its time; a line of metrics; and the end of each step's output, in GitHub's Markdown. Without a public address,
//! the check run is the only place where people see the output, so it holds it, within GitHub's limits.

use std::collections::{BTreeMap, VecDeque};
use std::fmt::Write as _;
use std::time::{Duration, Instant};

use crate::orchestrator::checks::CheckOutput;
use crate::scheduler::{Progress, Status};

/// GitHub refuses a summary or a text of more than 65535 characters; the rest is room for the closing lines.
const TEXT_LIMIT: usize = 60_000;
/// How many lines of output a step keeps while it runs, and shows when it failed or still runs.
const FAILED_TAIL: usize = 100;
/// How many lines a step that succeeded shows, folded.
const SUCCEEDED_TAIL: usize = 20;
const FENCE: char = '`';
const MIN_FENCE: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Queued,
    Running,
    Ran,
    Cached,
    Failed,
    Skipped,
}

impl State {
    fn icon(self) -> &'static str {
        match self {
            State::Queued => "⏸️",
            State::Running => "⏳",
            State::Ran => "✅",
            State::Cached => "♻️",
            State::Failed => "❌",
            State::Skipped => "⏭️",
        }
    }

    fn word(self) -> &'static str {
        match self {
            State::Queued => "queued",
            State::Running => "running",
            State::Ran => "ran",
            State::Cached => "cached",
            State::Failed => "failed",
            State::Skipped => "skipped",
        }
    }
}

#[derive(Debug)]
struct Step {
    state: State,
    started: Option<Instant>,
    took: Option<Duration>,
    /// Why it failed or was skipped.
    reason: Option<String>,
    /// The last lines of its output: while it runs, from its events; when it ended, from its stored output.
    tail: VecDeque<String>,
}

impl Step {
    fn new() -> Self {
        Step { state: State::Queued, started: None, took: None, reason: None, tail: VecDeque::new() }
    }

    fn time(&self, now: Instant) -> Option<Duration> {
        self.took.or_else(|| self.started.map(|started| now - started))
    }
}

/// The steps of one goal's run, as its events arrive.
#[derive(Debug)]
pub(super) struct GoalReport {
    started: Instant,
    steps: BTreeMap<String, Step>,
}

impl GoalReport {
    /// A report of the goal whose actions are `actions`, all queued.
    pub(super) fn new<'a>(actions: impl Iterator<Item = &'a String>) -> Self {
        GoalReport { started: Instant::now(), steps: actions.map(|name| (name.clone(), Step::new())).collect() }
    }

    pub(super) fn see(&mut self, progress: &Progress) {
        match progress {
            Progress::Started { name, .. } => {
                let step = self.step(name);
                step.state = State::Running;
                step.started = Some(Instant::now());
            }
            Progress::Log { name, line, .. } => {
                let tail = &mut self.step(name).tail;
                tail.push_back(console::strip_ansi_codes(line).into_owned());
                if tail.len() > FAILED_TAIL {
                    tail.pop_front();
                }
            }
            Progress::Done { name, status } => {
                let step = self.step(name);
                step.took = step.started.map(|started| started.elapsed());
                let (state, reason) = state_of(status);
                step.state = state;
                step.reason = reason;
            }
        }
    }

    /// Replaces a step's lines with the end of its whole output, once it ended.
    pub(super) fn set_output(&mut self, name: &str, output: &str) {
        let lines: Vec<&str> = output.lines().collect();
        let start = lines.len().saturating_sub(FAILED_TAIL);
        self.step(name).tail = lines[start..].iter().map(|line| console::strip_ansi_codes(line).into_owned()).collect();
    }

    fn step(&mut self, name: &str) -> &mut Step {
        self.steps.entry(name.to_owned()).or_insert_with(Step::new)
    }

    fn count(&self, state: State) -> usize {
        self.steps.values().filter(|step| step.state == state).count()
    }

    /// The check run's output: the title says how the goal went, the summary holds the steps and the metrics, and the
    /// text holds their output.
    pub(super) fn output(&self) -> CheckOutput {
        let now = Instant::now();
        CheckOutput { title: self.title(), summary: self.summary(now), text: self.text(now) }
    }

    fn title(&self) -> String {
        let (ran, cached, failed) = (self.count(State::Ran), self.count(State::Cached), self.count(State::Failed));
        let ended = ran + cached + failed + self.count(State::Skipped);
        match self.count(State::Running) + self.count(State::Queued) {
            0 => format!("{ran} ran, {cached} cached, {failed} failed"),
            _ => format!("Running: {ended} of {} steps done", self.steps.len()),
        }
    }

    fn summary(&self, now: Instant) -> String {
        let mut text = String::from("| | Step | Result | Time |\n|---|---|---|---|\n");
        for (index, (name, step)) in self.steps.iter().enumerate() {
            let result = match &step.reason {
                Some(reason) => format!("{}: {}", step.state.word(), table_cell(reason)),
                None => step.state.word().to_owned(),
            };
            let time = step.time(now).map(seconds).unwrap_or_default();
            let row = format!("| {} | `{name}` | {result} | {time} |\n", step.state.icon());
            if text.len() + row.len() > TEXT_LIMIT {
                let _ = writeln!(text, "\n{} more steps are left out.", self.steps.len() - index);
                break;
            }
            text.push_str(&row);
        }
        text.push('\n');
        text.push_str(&self.metrics(now));
        text
    }

    /// How many steps there are, how many results the cache gave, how long the goal took, and how long its steps ran.
    fn metrics(&self, now: Instant) -> String {
        let total = self.steps.len();
        let cached = self.count(State::Cached);
        let hits = (cached * 100).checked_div(total).unwrap_or_default();
        let busy: Duration = self.steps.values().filter_map(|step| step.time(now)).sum();
        format!(
            "**{total} steps**: {} ran, {cached} cached ({hits}% from the cache), {} failed, {} skipped · the goal took {} \
             · its steps ran for {} together",
            self.count(State::Ran),
            self.count(State::Failed),
            self.count(State::Skipped),
            seconds(now - self.started),
            seconds(busy),
        )
    }

    /// The output of the steps: failed and running steps first and open, then the steps that ran, folded. Cached steps
    /// show no output, because they did not run now.
    fn text(&self, now: Instant) -> String {
        let order = [State::Failed, State::Running, State::Ran];
        let mut text = String::new();
        let mut left_out = 0;
        for state in order {
            for (name, step) in self.steps.iter().filter(|(_, step)| step.state == state && !step.tail.is_empty()) {
                let block = block(name, step, now);
                if text.len() + block.len() > TEXT_LIMIT {
                    left_out += 1;
                    continue;
                }
                text.push_str(&block);
            }
        }
        if left_out > 0 {
            let _ = writeln!(
                text,
                "\nThe output of {left_out} more steps is left out: GitHub shows at most 65535 characters."
            );
        }
        text
    }
}

/// The output of one step: open when it failed or runs, folded when it succeeded.
fn block(name: &str, step: &Step, now: Instant) -> String {
    let time = step.time(now).map(|time| format!(", {}", seconds(time))).unwrap_or_default();
    let heading = format!("{} <code>{name}</code>: {}{time}", step.state.icon(), step.state.word());
    let shown = match step.state {
        State::Ran => SUCCEEDED_TAIL,
        _ => FAILED_TAIL,
    };
    let lines: Vec<&str> = step.tail.iter().map(String::as_str).collect();
    let lines = &lines[lines.len().saturating_sub(shown)..];
    let fence = fence_for(lines);
    let body = format!("{fence}text\n{}\n{fence}", lines.join("\n"));
    match step.state {
        State::Ran => format!("<details><summary>{heading}</summary>\n\n{body}\n</details>\n\n"),
        _ => format!("<details open><summary>{heading}</summary>\n\n{body}\n</details>\n\n"),
    }
}

/// A code fence longer than any run of backticks in `lines`, so that no line ends the block early.
fn fence_for(lines: &[&str]) -> String {
    let longest = lines.iter().flat_map(|line| line.split(|c| c != FENCE)).map(str::len).max().unwrap_or_default();
    FENCE.to_string().repeat((longest + 1).max(MIN_FENCE))
}

/// Text in a cell of a Markdown table: one line, without the `|` that ends a cell.
fn table_cell(text: &str) -> String {
    text.lines().next().unwrap_or_default().replace('|', "\\|")
}

fn seconds(time: Duration) -> String {
    let total = time.as_secs_f64();
    match total {
        t if t < 60.0 => format!("{t:.1}s"),
        t => format!("{}m {}s", (t / 60.0) as u64, (t % 60.0) as u64),
    }
}

fn state_of(status: &Status) -> (State, Option<String>) {
    match status {
        Status::Ran { .. } => (State::Ran, None),
        Status::Cached { .. } => (State::Cached, None),
        Status::Failed { reason, .. } => (State::Failed, Some(reason.clone())),
        Status::Skipped => (State::Skipped, Some("a step that it needs failed".to_owned())),
        Status::NotNeeded | Status::WillRun { .. } | Status::Waiting => (State::Queued, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fence_is_longer_than_the_backticks_in_the_output() {
        assert_eq!(fence_for(&["plain"]), "```");
        assert_eq!(fence_for(&["a ```` fence", "``"]), "`````");
    }
}
