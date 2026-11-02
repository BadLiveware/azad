//! Output for a GitHub Actions job: a collapsible group per action with its output, an error annotation for each
//! failure, and a Markdown table in the job summary.
//!
//! GitHub shows a group's lines while it is open, but groups cannot overlap, and actions run in parallel. So one
//! running action, the earliest started, has the open group and streams its output. The output of the others is
//! kept, and printed as whole groups when the open group closes; then the next running action streams. A stop signal
//! prints what is kept, so the output of a hung action is not lost.
//!
//! While an action's output is printed, workflow commands are stopped, so that a line such as `::add-mask::` in a
//! build's output cannot act on the user's job. The output's own `::error::`, `::warning::` and `::notice::` lines
//! show as text, as in the other renderers, and Azad makes each one again as an annotation of the job, after the
//! action's group, where commands work: from the parsed values only, with its file mapped to the path in the
//! repository (see `azad::frontend::gha::annotation`). Each failed action also gets one annotation, with its reason,
//! and each warning of Azad itself, such as a glob that matches no file, gets one too.

use std::collections::{BTreeMap, HashMap};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use azad::frontend::gha::annotation::{escape_data, escape_property};
use azad::frontend::{Note, NoteLevel};
use azad::{Event, Progress, Status};

use super::{Names, Renderer, Summary, discarded_note, duration, reason, shared_note};

const STOP_TOKEN_PREFIX: &str = "azad-";
const STOP_TOKEN_BYTES: usize = 16;
const TIMINGS_TITLE: &str = "Long steps that were not commands";

struct Running {
    since: Duration,
    /// The output that is not printed yet.
    kept: Vec<String>,
    /// The annotations that the output asks for, as commands to print after the action's group.
    annotations: Vec<String>,
}

pub(crate) struct Github {
    out: Box<dyn Write>,
    summary_file: Option<PathBuf>,
    /// By start time, so the earliest started is next to stream.
    running: BTreeMap<(Duration, String), Running>,
    /// The action whose group is open.
    open: Option<(Duration, String)>,
    /// Lines that wait for the open group to close.
    pending: Vec<String>,
    times: HashMap<String, Duration>,
    remote: Option<String>,
    /// The noteworthy steps that were not commands, as lines.
    timings: Vec<String>,
    stop_token: String,
    names: Names,
    /// The output of cache hits, which `--replay` sends before their result.
    replayed: HashMap<String, Vec<String>>,
}

impl Github {
    pub(crate) fn new(out: Box<dyn Write>, summary_file: Option<PathBuf>, debug: bool) -> Self {
        Github {
            out,
            summary_file,
            running: BTreeMap::new(),
            open: None,
            pending: Vec::new(),
            times: HashMap::new(),
            remote: None,
            timings: Vec::new(),
            stop_token: stop_token(),
            names: Names::new(debug),
            replayed: HashMap::new(),
        }
    }

    fn print(&mut self, lines: &[String]) {
        for line in lines {
            writeln!(self.out, "{line}").ok();
        }
        self.out.flush().ok();
    }

    /// Prints now, or after the open group closes.
    fn emit(&mut self, lines: Vec<String>) {
        match self.open {
            Some(_) => self.pending.extend(lines),
            None => self.print(&lines),
        }
    }

    fn group_start(&self, title: &str) -> [String; 2] {
        [format!("::group::{}", escape_data(title)), format!("::stop-commands::{}", self.stop_token)]
    }

    fn group_end(&self) -> [String; 2] {
        [format!("::{}::", self.stop_token), "::endgroup::".to_owned()]
    }

    /// A whole group with kept output.
    fn closed_group(&self, title: &str, kept: Vec<String>) -> Vec<String> {
        let mut lines: Vec<String> = self.group_start(title).into();
        lines.extend(kept);
        lines.extend(self.group_end());
        lines
    }

    /// Gives the open group to the earliest running action, and prints its kept output.
    fn open_next(&mut self) {
        let Some(key) = self.running.keys().next().cloned() else { return };
        let mut lines: Vec<String> = self.group_start(&format!("▶ {}", self.names.title(&key.1))).into();
        lines.append(&mut self.running.get_mut(&key).expect("the key is running").kept);
        self.open = Some(key);
        self.print(&lines);
    }

    /// The result of an action; `took` is its time, or what it shared.
    fn result_lines(&self, label: &str, status: &Status, took: &str) -> Vec<String> {
        let name = self.names.title(label);
        let mut lines = match status {
            Status::Ran { .. } => vec![format!("✓ {name}{}", details(&[took]))],
            Status::Failed { .. } => vec![
                format!("✗ {name}{}", details(&[&reason(status), took])),
                format!("::error title={}::{}", escape_property(name), escape_data(&reason(status))),
            ],
            _ => Vec::new(),
        };
        if let Some(note) = discarded_note(status) {
            lines.push(format!("::warning title={}::{}", escape_property(name), escape_data(&note)));
        }
        lines
    }

    fn done(&mut self, at: Duration, name: &str, status: &Status) {
        let key = self.running.keys().find(|(_, n)| n == name).cloned();
        let Some((key, running)) = key.and_then(|key| self.running.remove(&key).map(|r| (key, r))) else {
            let mut lines = match self.replayed.remove(name) {
                Some(kept) => self.closed_group(&format!("{} (cached)", self.names.title(name)), kept),
                None => Vec::new(),
            };
            lines.extend(self.not_started_lines(name, status));
            self.emit(lines);
            return;
        };
        let took = at.saturating_sub(running.since);
        self.times.insert(name.to_owned(), took);
        let mut result = self.result_lines(name, status, &duration(took));
        result.extend(running.annotations);
        if self.open.as_ref() != Some(&key) {
            let title = format!("{} ({})", self.names.title(name), duration(took));
            let mut lines = self.closed_group(&title, running.kept);
            lines.extend(result);
            self.emit(lines);
            return;
        }
        let mut lines: Vec<String> = self.group_end().into();
        lines.extend(result);
        lines.append(&mut self.pending);
        self.open = None;
        self.print(&lines);
        self.open_next();
    }

    /// The result of an action that did not start: a cache hit, a skip, or an action that shared an execution that
    /// was done.
    fn not_started_lines(&self, label: &str, status: &Status) -> Vec<String> {
        let name = self.names.title(label);
        match status {
            Status::Cached { .. } => vec![format!("• {name} (cached)")],
            Status::Skipped => vec![format!("↷ {name} (skipped: a dependency failed)")],
            Status::Ran { .. } | Status::Failed { .. } => {
                let shared = self.names.shared_with(label, status).map(shared_note).unwrap_or_default();
                self.result_lines(label, status, &shared)
            }
            Status::NotNeeded | Status::WillRun { .. } | Status::Waiting => Vec::new(),
        }
    }

    fn markdown(&self, at: Duration, summary: &Summary) -> String {
        let (mark, verdict) = match summary.holds {
            true => ("✅", "holds"),
            false => ("❌", "does not hold"),
        };
        let mut text = format!(
            "### {mark} Azad: `{}` {verdict}\n\n{} in {} (run {})\n\n| Action | Result | Time |\n|---|---|---|\n",
            summary.goal,
            summary.counts(),
            duration(at),
            summary.run_number
        );
        for (name, status) in &summary.statuses {
            let result = match status {
                Status::Ran { .. } => "ran".to_owned(),
                Status::Cached { .. } => "cached".to_owned(),
                Status::Failed { .. } | Status::Skipped => reason(status),
                Status::NotNeeded | Status::WillRun { .. } | Status::Waiting => continue,
            };
            let time = self.times.get(name).map(|d| duration(*d)).unwrap_or_default();
            text.push_str(&format!("| `{}` | {result} | {time} |\n", self.names.title(name)));
        }
        if let Some(remote) = &self.remote {
            text.push_str(&format!("\n{remote}\n"));
        }
        if !self.timings.is_empty() {
            let steps: String = self.timings.iter().map(|step| format!("- {step}\n")).collect();
            text.push_str(&format!("\n<details><summary>{TIMINGS_TITLE}</summary>\n\n{steps}\n</details>\n"));
        }
        text
    }
}

impl Renderer for Github {
    fn event(&mut self, at: Duration, event: &Event) {
        self.names.event(event);
        match event {
            Event::Pull { reference } => self.emit(vec![format!("↓ pull {reference}")]),
            Event::Fetch { url, rev } => self.emit(vec![format!("↓ fetch {url}@{rev}")]),
            Event::Warning(message) => self.emit(vec![format!("::warning title=Azad::{}", escape_data(message))]),
            Event::Note(Note { level: NoteLevel::Normal, text }) => {
                self.emit(vec![format!("::notice title=Azad::{}", escape_data(text))]);
            }
            Event::Note(Note { level: NoteLevel::Diagnostic, text }) => self.emit(vec![format!("note: {text}")]),
            Event::Remote(summary) => {
                self.remote = Some(summary.to_string());
                let mut lines = vec![summary.to_string()];
                lines.extend(summary.errors.iter().map(|e| format!("::warning title=Azad cache::{}", escape_data(e))));
                self.emit(lines);
            }
            Event::Timings(steps) => {
                self.timings = steps.iter().map(ToString::to_string).collect();
                let mut lines = vec![format!("::group::⏱ {TIMINGS_TITLE}")];
                lines.extend(self.timings.iter().cloned());
                lines.push("::endgroup::".to_owned());
                self.emit(lines);
            }
            Event::Loaded { .. } => {}
            Event::Progress(Progress::Started { name, .. }) => {
                let running = Running { since: at, kept: Vec::new(), annotations: Vec::new() };
                self.running.insert((at, name.clone()), running);
                if self.open.is_none() {
                    self.open_next();
                }
            }
            Event::Progress(Progress::Log { name, line, .. }) => {
                let shown = self.names.show(line);
                let annotation = self.names.annotation(name, line).map(|annotation| annotation.command());
                let streaming = self.open.as_ref().is_some_and(|(_, open)| open == name);
                let Some((_, running)) = self.running.iter_mut().find(|((_, n), _)| n == name) else {
                    self.replayed.entry(name.clone()).or_default().extend(shown);
                    return;
                };
                running.annotations.extend(annotation);
                if streaming {
                    self.print(&shown);
                    return;
                }
                running.kept.extend(shown);
            }
            Event::Progress(Progress::Done { name, status }) => self.done(at, name, status),
        }
    }

    fn interrupt(&mut self, at: Duration) {
        let mut lines = Vec::new();
        if self.open.take().is_some() {
            lines.extend(self.group_end());
        }
        lines.append(&mut self.pending);
        for ((_, name), running) in std::mem::take(&mut self.running) {
            let stopped = duration(at.saturating_sub(running.since));
            let title = format!("{} (stopped after {stopped})", self.names.title(&name));
            lines.extend(self.closed_group(&title, running.kept));
            lines.extend(running.annotations);
        }
        lines.push("::error title=Azad::the run was stopped".to_owned());
        self.print(&lines);
    }

    fn finish(&mut self, at: Duration, summary: &Summary) {
        let verdict = match summary.holds {
            true => "holds",
            false => "does not hold",
        };
        let line = format!(
            "goal {} {verdict}: {} in {} (run {})",
            summary.goal,
            summary.counts(),
            duration(at),
            summary.run_number
        );
        self.print(&[line]);
        let Some(path) = &self.summary_file else { return };
        let markdown = self.markdown(at, summary);
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
            file.write_all(markdown.as_bytes()).ok();
        }
    }
}

/// A token that resumes workflow commands, which an action's output cannot guess: random, and new for each run.
fn stop_token() -> String {
    let mut bytes = [0u8; STOP_TOKEN_BYTES];
    getrandom::fill(&mut bytes).expect("the operating system gives random bytes");
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{STOP_TOKEN_PREFIX}{hex}")
}

/// ` (a, b)` for the parts that are not empty, or nothing.
fn details(parts: &[&str]) -> String {
    let parts: Vec<&str> = parts.iter().copied().filter(|part| !part.is_empty()).collect();
    match parts.is_empty() {
        true => String::new(),
        false => format!(" ({})", parts.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::*;
    use super::*;

    fn renderer() -> (Github, Shared) {
        let out = Shared::default();
        (Github::new(Box::new(out.clone()), None, false), out)
    }

    /// The lines without the stop-commands markers, which hold a token that differs per run.
    fn visible(out: &Shared) -> Vec<String> {
        out.lines().into_iter().filter(|l| !l.starts_with("::stop-commands::") && !l.starts_with("::azad-")).collect()
    }

    fn assert_groups_do_not_overlap(lines: &[String]) {
        let mut open = false;
        for line in lines {
            if line.starts_with("::group::") {
                assert!(!open, "a group opens inside another: {lines:#?}");
                open = true;
            }
            if line == "::endgroup::" {
                assert!(open, "a group closes that is not open: {lines:#?}");
                open = false;
            }
        }
        assert!(!open, "a group stays open: {lines:#?}");
    }

    /// A cache hit whose output `--replay` sends gets a group of its own, while an action streams in the open group.
    #[test]
    fn a_replayed_cache_hit_gets_a_group_of_its_own() {
        let (mut github, out) = renderer();
        github.event(at(0), &started("a"));
        github.event(at(1), &log("b", "b said"));
        github.event(at(1), &cached("b"));
        github.event(at(2), &ran("a"));
        let lines = visible(&out);
        assert_groups_do_not_overlap(&lines);
        let group =
            lines.iter().position(|l| l.starts_with("::group::") && l.contains("b (cached)")).expect("b's group");
        assert_eq!(lines[group + 1], "b said", "{lines:#?}");
        assert!(lines.iter().any(|l| l == "• b (cached)"), "{lines:#?}");
    }

    #[test]
    fn the_earliest_action_streams_and_the_others_follow_as_whole_groups() {
        let (mut github, out) = renderer();
        github.event(at(0), &started("a"));
        github.event(at(1), &started("b"));
        github.event(at(2), &log("b", "b1"));
        github.event(at(2), &log("a", "a1"));
        assert_eq!(visible(&out), ["::group::▶ a", "a1"], "a streams; b's output is kept");

        github.event(at(3), &ran("b"));
        github.event(at(4), &log("a", "a2"));
        github.event(at(5), &ran("a"));
        github.event(at(6), &started("c"));
        github.event(at(7), &log("c", "c1"));
        assert_eq!(
            visible(&out),
            [
                "::group::▶ a",
                "a1",
                "a2",
                "::endgroup::",
                "✓ a (5.0s)",
                "::group::b (2.0s)",
                "b1",
                "::endgroup::",
                "✓ b (2.0s)",
                "::group::▶ c",
                "c1",
            ]
        );
        github.event(at(8), &ran("c"));
        assert_groups_do_not_overlap(&out.lines());
    }

    #[test]
    fn the_next_running_action_streams_with_what_it_printed_before() {
        let (mut github, out) = renderer();
        github.event(at(0), &started("a"));
        github.event(at(1), &started("b"));
        github.event(at(2), &log("b", "early"));
        github.event(at(3), &ran("a"));
        github.event(at(4), &log("b", "late"));
        let lines = visible(&out);
        assert_eq!(lines[lines.len() - 3..], ["::group::▶ b", "early", "late"]);
    }

    #[test]
    fn a_stop_prints_the_kept_output() {
        let (mut github, out) = renderer();
        github.event(at(0), &started("a"));
        github.event(at(1), &started("b"));
        github.event(at(2), &log("b", "the line that explains the hang"));
        github.interrupt(at(9));
        assert!(visible(&out).contains(&"the line that explains the hang".to_owned()));
        assert_groups_do_not_overlap(&out.lines());
    }

    #[test]
    fn workflow_commands_in_output_are_stopped() {
        let (mut github, out) = renderer();
        github.event(at(0), &started("a"));
        github.event(at(1), &log("a", "::add-mask::secret"));
        github.event(at(2), &ran("a"));
        let lines = out.lines();
        let stop = lines.iter().position(|l| l.starts_with("::stop-commands::")).unwrap();
        let token = lines[stop].trim_start_matches("::stop-commands::");
        let resume = lines.iter().position(|l| *l == format!("::{token}::")).unwrap();
        let command = lines.iter().position(|l| l == "::add-mask::secret").unwrap();
        assert!(stop < command && command < resume);
    }

    #[test]
    fn the_output_shows_workflow_commands_as_text_and_annotations_follow_the_group() {
        let (mut github, out) = renderer();
        github.event(at(0), &started("a"));
        for line in COMMANDS {
            github.event(at(1), &log("a", line));
        }
        github.event(at(2), &ran("a"));
        assert_eq!(
            visible(&out),
            [
                "::group::▶ a",
                "▸ Build",
                "inside",
                "Error: a.rs:3: bad",
                "next",
                "Warning: careful 100%",
                "Notice: fyi",
                "::endgroup::",
                "✓ a (2.0s)",
                "::error::bad%0Anext",
                "::warning::careful 100%25",
                "::notice::fyi",
            ]
        );
    }

    /// The output with the run's stop token as `<token>`.
    fn with_token(out: &Shared) -> Vec<String> {
        let lines = out.lines();
        let stop = lines.iter().find_map(|l| l.strip_prefix("::stop-commands::")).expect("commands are stopped");
        lines.iter().map(|line| line.replace(stop, "<token>")).collect()
    }

    /// Cargo's output for a warning and a type error, as Cargo printed them in a workspace with a member `crates/a`.
    const CARGO_OUTPUT: [&str; 13] = [
        "   Compiling a v0.1.0 (/work/crates/a)",
        "warning: unused variable: `unused`",
        " --> crates/a/src/lib.rs:2:9",
        "  |",
        "2 |     let unused = 1;",
        "  |         ^^^^^^ help: if this is intentional, prefix it with an underscore: `_unused`",
        "warning: `a` (lib) generated 1 warning",
        "error[E0308]: mismatched types",
        " --> crates/a/src/lib.rs:3:5",
        "  |",
        "3 |     \"text\"",
        "  |     ^^^^^^ expected `u32`, found `&str`",
        "error: could not compile `a` (lib) due to 1 previous error",
    ];

    #[test]
    fn annotations_in_the_output_become_annotations_of_the_job_after_the_group() {
        let (mut github, out) = renderer();
        github.event(at(0), &loaded_with_views(&[("a", "/work", "crates/a")]));
        github.event(at(0), &started("a"));
        for line in [
            "::error file=/work/crates/a.rs,line=3::bad",
            "::warning file=src/lib.rs,line=7,col=2,title=Lint::careful",
            "::set-env name=PATH::/evil",
            "::add-mask::secret",
            "::error file=/etc/passwd,line=1::outside",
        ] {
            github.event(at(1), &log("a", line));
        }
        github.event(at(2), &failed("a", "exit code 1"));
        assert_eq!(
            with_token(&out),
            [
                "::group::▶ a",
                "::stop-commands::<token>",
                "Error: /work/crates/a.rs:3: bad",
                "Warning: src/lib.rs:7:2: Lint: careful",
                "::set-env name=PATH::/evil",
                "::add-mask::secret",
                "Error: /etc/passwd:1: outside",
                "::<token>::",
                "::endgroup::",
                "✗ a (exit code 1, 2.0s)",
                "::error title=a::exit code 1",
                "::error file=crates/a.rs,line=3::bad",
                "::warning file=crates/a/src/lib.rs,line=7,col=2,title=Lint::careful",
                "::error::outside",
            ]
        );
    }

    #[test]
    fn an_action_that_waited_for_the_open_group_gets_its_annotations_after_its_own_group() {
        let (mut github, out) = renderer();
        github.event(at(0), &loaded_with_views(&[("job", "/tmp/github/workspace", "")]));
        github.event(at(0), &started("a"));
        github.event(at(0), &started("job"));
        github.event(at(1), &log("job", "::error file=/tmp/github/workspace/app/main.py,line=9::bad"));
        github.event(at(2), &failed("job", "exit code 1"));
        github.event(at(3), &ran("a"));
        assert_eq!(
            visible(&out),
            [
                "::group::▶ a",
                "::endgroup::",
                "✓ a (3.0s)",
                "::group::job (2.0s)",
                "Error: /tmp/github/workspace/app/main.py:9: bad",
                "::endgroup::",
                "✗ job (exit code 1, 2.0s)",
                "::error title=job::exit code 1",
                "::error file=app/main.py,line=9::bad",
            ]
        );
    }

    #[test]
    fn cargo_errors_are_printed_as_cargo_wrote_them_so_that_the_action_s_matcher_reads_them() {
        let (mut github, out) = renderer();
        github.event(at(0), &started("//:clippy"));
        for line in CARGO_OUTPUT {
            github.event(at(1), &log("//:clippy", line));
        }
        github.event(at(2), &failed("//:clippy", "exit code 101"));
        let found = matches(&out.lines());
        let found: Vec<[&str; 4]> = found.iter().map(|(a, b, c, d)| [a.as_str(), b, c, d]).collect();
        assert_eq!(
            found,
            [
                ["warning", "", "unused variable: `unused`", "crates/a/src/lib.rs:2:9"],
                ["error", "E0308", "mismatched types", "crates/a/src/lib.rs:3:5"],
            ]
        );
    }

    /// What the problem matchers of the Azad action find in the output, as GitHub's runner reads it: the first pattern
    /// on a line and each next pattern on the line after it. Each match is (severity, code, message, file:line:column).
    fn matches(lines: &[String]) -> Vec<(String, String, String, String)> {
        let matcher: serde_json::Value =
            serde_json::from_str(include_str!("../../../../action/cargo-matcher.json")).expect("the matcher is JSON");
        let mut found = Vec::new();
        for matcher in matcher["problemMatcher"].as_array().expect("a list of matchers") {
            let patterns = matcher["pattern"].as_array().expect("a list of patterns");
            let regex = |index: usize| regex::Regex::new(patterns[index]["regexp"].as_str().unwrap()).unwrap();
            let (first, second) = (regex(0), regex(1));
            for pair in lines.windows(2) {
                let (Some(head), Some(location)) = (first.captures(&pair[0]), second.captures(&pair[1])) else {
                    continue;
                };
                let group = |captures: &regex::Captures, pattern: usize, key: &str| {
                    let index = patterns[pattern][key].as_u64().expect("a group number") as usize;
                    captures.get(index).map(|m| m.as_str().to_owned()).unwrap_or_default()
                };
                let place = ["file", "line", "column"].map(|key| group(&location, 1, key)).join(":");
                found.push((group(&head, 0, "severity"), group(&head, 0, "code"), group(&head, 0, "message"), place));
            }
        }
        found
    }

    #[test]
    fn a_warning_of_azad_is_an_annotation() {
        let (mut github, out) = renderer();
        github.event(at(0), &Event::Warning("glob([\"scr/*.py\"]) in //app matches no file\nsee 100%".to_owned()));
        assert_eq!(visible(&out), ["::warning title=Azad::glob([\"scr/*.py\"]) in //app matches no file%0Asee 100%25"]);
    }

    #[test]
    fn a_matrix_job_and_an_action_that_shared_an_execution_get_their_rows() {
        let (mut github, out) = renderer();
        github.event(at(0), &loaded(&[("one", "one (3.12, fast)"), ("two", "two (3.12, slow)")]));
        github.event(at(0), &started_as("one", "same"));
        github.event(at(1), &ran_as("one", "same"));
        github.event(at(1), &ran_as("two", "same"));
        github.event(at(1), &cached("three"));
        assert_eq!(
            visible(&out),
            [
                "::group::▶ one (3.12, fast)",
                "::endgroup::",
                "✓ one (3.12, fast) (1.0s)",
                "✓ two (3.12, slow) (same action as one (3.12, fast))",
                "• three (cached)",
            ]
        );
    }
}
