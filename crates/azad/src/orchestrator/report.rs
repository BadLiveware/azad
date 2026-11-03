//! What a check run shows of its goal (decision 0033): a table of the goal's steps, one for each action, with its state
//! and its time; a line of metrics; and the end of each step's output, in GitHub's Markdown, from the run's record
//! (decision 0034). A link to the run's page on the site, when there is one, shows the rest.

use std::fmt::Write as _;

use crate::orchestrator::checks::CheckOutput;
use crate::orchestrator::record::{Goal, Step, StepState};

/// GitHub refuses a summary or a text of more than 65535 characters; the rest is room for the closing lines.
const TEXT_LIMIT: usize = 60_000;
/// How many lines a step that failed or runs shows, open.
const FAILED_TAIL: usize = 100;
/// How many lines a step that succeeded shows, folded.
const SUCCEEDED_TAIL: usize = 20;
const FENCE: char = '`';
const MIN_FENCE: usize = 3;
const MILLIS_IN_SECOND: u64 = 1000;
const SECONDS_IN_MINUTE: u64 = 60;

pub fn icon(state: StepState) -> &'static str {
    match state {
        StepState::Queued => "⏸️",
        StepState::Running => "⏳",
        StepState::Ran => "✅",
        StepState::Cached => "♻️",
        StepState::Failed => "❌",
        StepState::Skipped => "⏭️",
    }
}

/// A time in milliseconds, for people: `35ms`, `4.2s`, or `3m 5s`.
pub fn duration(millis: u64) -> String {
    let seconds = millis / MILLIS_IN_SECOND;
    match (seconds, seconds < SECONDS_IN_MINUTE) {
        (0, _) => format!("{millis}ms"),
        (_, true) => format!("{seconds}.{}s", (millis % MILLIS_IN_SECOND) / 100),
        (_, false) => format!("{}m {}s", seconds / SECONDS_IN_MINUTE, seconds % SECONDS_IN_MINUTE),
    }
}

/// The title of a goal: how it went, or how far it is.
pub fn title(goal: &Goal) -> String {
    let (ran, cached, failed) =
        (goal.count(StepState::Ran), goal.count(StepState::Cached), goal.count(StepState::Failed));
    let ended = goal.steps.iter().filter(|step| step.state.has_ended()).count();
    match ended == goal.steps.len() {
        true => format!("{ran} ran, {cached} cached, {failed} failed"),
        false => format!("Running: {ended} of {} steps done", goal.steps.len()),
    }
}

/// The check run's output at `now`: the title says how the goal went, the summary holds the steps and the metrics,
/// and the text holds their output.
pub fn output(goal: &Goal, now: u64) -> CheckOutput {
    CheckOutput { title: title(goal), summary: summary(goal, now), text: text(goal, now) }
}

fn summary(goal: &Goal, now: u64) -> String {
    let mut text = String::from("| | Step | Result | Time |\n|---|---|---|---|\n");
    for (index, step) in goal.steps.iter().enumerate() {
        let result = match &step.reason {
            Some(reason) => format!("{}: {}", step.state.word(), table_cell(reason)),
            None => step.state.word().to_owned(),
        };
        let time = step.millis(now).map(duration).unwrap_or_default();
        let row = format!("| {} | `{}` | {result} | {time} |\n", icon(step.state), step.name);
        if text.len() + row.len() > TEXT_LIMIT {
            let _ = writeln!(text, "\n{} more steps are left out.", goal.steps.len() - index);
            break;
        }
        text.push_str(&row);
    }
    text.push('\n');
    text.push_str(&metrics(goal, now));
    text
}

/// How many steps there are, how many results the cache gave, how long the goal took, and how long its steps ran.
pub fn metrics(goal: &Goal, now: u64) -> String {
    let total = goal.steps.len();
    let cached = goal.count(StepState::Cached);
    let hits = (cached * 100).checked_div(total).unwrap_or_default();
    let busy: u64 = goal.steps.iter().filter_map(|step| step.millis(now)).sum();
    format!(
        "**{total} steps**: {} ran, {cached} cached ({hits}% from the cache), {} failed, {} skipped · the goal took {} · \
         its steps ran for {} together",
        goal.count(StepState::Ran),
        goal.count(StepState::Failed),
        goal.count(StepState::Skipped),
        duration(goal.ended.unwrap_or(now).saturating_sub(goal.started)),
        duration(busy),
    )
}

/// The output of the steps: failed and running steps first and open, then the steps that ran, folded. Cached steps
/// show no output, because they did not run now.
fn text(goal: &Goal, now: u64) -> String {
    let order = [StepState::Failed, StepState::Running, StepState::Ran];
    let mut text = String::new();
    let mut left_out = 0;
    for state in order {
        for step in goal.steps.iter().filter(|step| step.state == state && !step.tail.is_empty()) {
            let block = block(step, now);
            if text.len() + block.len() > TEXT_LIMIT {
                left_out += 1;
                continue;
            }
            text.push_str(&block);
        }
    }
    if left_out > 0 {
        let _ =
            writeln!(text, "\nThe output of {left_out} more steps is left out: GitHub shows at most 65535 characters.");
    }
    text
}

/// The output of one step, without terminal escapes: open when it failed or runs, folded when it succeeded.
fn block(step: &Step, now: u64) -> String {
    let time = step.millis(now).map(|time| format!(", {}", duration(time))).unwrap_or_default();
    let heading = format!("{} <code>{}</code>: {}{time}", icon(step.state), step.name, step.state.word());
    let shown = match step.state {
        StepState::Ran => SUCCEEDED_TAIL,
        _ => FAILED_TAIL,
    };
    let lines: Vec<String> = step
        .tail
        .iter()
        .skip(step.tail.len().saturating_sub(shown))
        .map(|line| console::strip_ansi_codes(line).into_owned())
        .collect();
    let fence = fence_for(&lines);
    let body = format!("{fence}text\n{}\n{fence}", lines.join("\n"));
    match step.state {
        StepState::Ran => format!("<details><summary>{heading}</summary>\n\n{body}\n</details>\n\n"),
        _ => format!("<details open><summary>{heading}</summary>\n\n{body}\n</details>\n\n"),
    }
}

/// A code fence longer than any run of backticks in `lines`, so that no line ends the block early.
fn fence_for(lines: &[String]) -> String {
    let longest = lines.iter().flat_map(|line| line.split(|c| c != FENCE)).map(str::len).max().unwrap_or_default();
    FENCE.to_string().repeat((longest + 1).max(MIN_FENCE))
}

/// Text in a cell of a Markdown table: one line, without the `|` that ends a cell.
fn table_cell(text: &str) -> String {
    text.lines().next().unwrap_or_default().replace('|', "\\|")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fence_is_longer_than_the_backticks_in_the_output() {
        assert_eq!(fence_for(&["plain".to_owned()]), "```");
        assert_eq!(fence_for(&["a ```` fence".to_owned(), "``".to_owned()]), "`````");
    }

    #[test]
    fn a_duration_reads_in_seconds_or_minutes() {
        assert_eq!(duration(35), "35ms");
        assert_eq!(duration(4_250), "4.2s");
        assert_eq!(duration(185_000), "3m 5s");
    }
}
