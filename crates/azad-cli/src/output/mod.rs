//! How a run looks where it runs: a live view in a terminal, plain lines in a log, and groups and a job summary in
//! GitHub Actions. Each renderer consumes the engine's events; `drive` feeds them, with ticks for timers.

mod echo;
mod github;
mod plain;
mod terminal;

use std::collections::{BTreeMap, HashMap};
use std::env;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::process;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use azad::frontend::gha::annotation::Annotation;
use azad::frontend::gha::display;
use azad::frontend::{Note, NoteLevel, WorkspaceView};
use azad::ir::Digest;
use azad::{Event, RunReport, Status};
use clap::ValueEnum;

const TICK: Duration = Duration::from_millis(100);
/// The exit code of a process stopped by SIGINT, as shells report it.
const INTERRUPTED_EXIT: i32 = 130;
const GITHUB_ACTIONS_ENV: &str = "GITHUB_ACTIONS";
const GITHUB_SUMMARY_ENV: &str = "GITHUB_STEP_SUMMARY";
const NO_COLOR_ENV: &str = "NO_COLOR";
/// The lines of a failed action's output that the terminal and plain renderers show.
const FAILURE_TAIL_LINES: usize = 20;
/// The discarded paths that a note names.
const DISCARDED_SHOWN: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum Format {
    /// GitHub Actions when $GITHUB_ACTIONS is set; a terminal view when standard error is a terminal; plain otherwise.
    Auto,
    Terminal,
    Plain,
    Github,
}

/// What a run shows beyond its results and errors (decision 0022).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
pub(crate) enum Verbosity {
    /// The results, the summary of the remote cache, warnings and errors.
    Minimal,
    /// Also the decisions that change what happens, such as facts from another source, and downloads and slow steps.
    Normal,
    /// Also every other decision, such as the commit that each ref named and where each fact came from.
    Diagnostic,
}

impl Verbosity {
    pub(crate) fn shows(self, event: &Event) -> bool {
        match event {
            Event::Note(Note { level: NoteLevel::Normal, .. }) => self >= Verbosity::Normal,
            Event::Note(Note { level: NoteLevel::Diagnostic, .. }) => self >= Verbosity::Diagnostic,
            Event::Pull { .. } | Event::Fetch { .. } | Event::Timings(_) => self >= Verbosity::Normal,
            _ => true,
        }
    }
}

/// A renderer that gets only the events that the verbosity shows.
struct Filtered {
    inner: Box<dyn Renderer>,
    verbosity: Verbosity,
}

impl Renderer for Filtered {
    fn event(&mut self, at: Duration, event: &Event) {
        if self.verbosity.shows(event) {
            self.inner.event(at, event);
        }
    }

    fn tick(&mut self, at: Duration) {
        self.inner.tick(at);
    }

    fn interrupt(&mut self, at: Duration) {
        self.inner.interrupt(at);
    }

    fn finish(&mut self, at: Duration, summary: &Summary) {
        self.inner.finish(at, summary);
    }
}

/// The end of a run, for the closing lines of a renderer.
#[derive(Debug)]
pub(crate) struct Summary {
    pub(crate) goal: String,
    pub(crate) holds: bool,
    pub(crate) run_number: u64,
    pub(crate) statuses: BTreeMap<String, Status>,
}

impl Summary {
    pub(crate) fn from_report(goal: &str, report: &RunReport) -> Self {
        Summary {
            goal: goal.to_owned(),
            holds: report.outcome.goal_holds(),
            run_number: report.number,
            statuses: report.outcome.statuses.clone(),
        }
    }

    fn count(&self, matches: impl Fn(&Status) -> bool) -> usize {
        self.statuses.values().filter(|s| matches(s)).count()
    }

    /// The outcome for other programs, as `azad run --report` writes it.
    pub(crate) fn to_json(&self) -> String {
        let count = |matches: fn(&Status) -> bool| self.count(matches);
        let report = serde_json::json!({
            "goal": self.goal,
            "holds": self.holds,
            "run": self.run_number,
            "needed": count(|s| !matches!(s, Status::NotNeeded)),
            "ran": count(|s| matches!(s, Status::Ran { .. })),
            "cached": count(|s| matches!(s, Status::Cached { .. })),
            "failed": count(|s| matches!(s, Status::Failed { .. })),
            "skipped": count(|s| matches!(s, Status::Skipped)),
        });
        report.to_string()
    }

    /// `3 needed, 1 ran, 2 cached`, and the failed and skipped counts when there are any.
    fn counts(&self) -> String {
        let needed = self.count(|s| !matches!(s, Status::NotNeeded));
        let ran = self.count(|s| matches!(s, Status::Ran { .. }));
        let cached = self.count(|s| matches!(s, Status::Cached { .. }));
        let failed = self.count(|s| matches!(s, Status::Failed { .. }));
        let skipped = self.count(|s| matches!(s, Status::Skipped));
        let mut parts = vec![format!("{needed} needed"), format!("{ran} ran"), format!("{cached} cached")];
        if failed > 0 {
            parts.push(format!("{failed} failed"));
        }
        if skipped > 0 {
            parts.push(format!("{skipped} skipped"));
        }
        parts.join(", ")
    }
}

pub(crate) trait Renderer {
    /// An event, at a time since the start.
    fn event(&mut self, at: Duration, event: &Event);
    /// Time passes with no event; a live view redraws its timers.
    fn tick(&mut self, _at: Duration) {}
    /// The run is stopped by a signal: print what is not printed yet.
    fn interrupt(&mut self, _at: Duration) {}
    fn finish(&mut self, at: Duration, summary: &Summary);
}

/// Text without its terminal escape sequences: colors and styles, and commands such as links, so that it can be
/// written where no terminal reads it.
pub(crate) fn without_escapes(text: &str) -> String {
    console::strip_ansi_codes(text).into_owned()
}

/// The renderer that a format names: `auto` chooses by where the output goes.
fn chosen(format: Format) -> Format {
    match format {
        Format::Auto if env::var_os(GITHUB_ACTIONS_ENV).is_some() => Format::Github,
        Format::Auto if io::stderr().is_terminal() => Format::Terminal,
        Format::Auto => Format::Plain,
        other => other,
    }
}

/// Whether the output is for people, who read hints; GitHub Actions would make a hint an annotation on every run.
pub(crate) fn is_for_people(format: Format) -> bool {
    chosen(format) != Format::Github
}

/// Whether the output shows colors: in the terminal view, unless `NO_COLOR` is set.
pub(crate) fn shows_color(format: Format) -> bool {
    chosen(format) == Format::Terminal && env::var_os(NO_COLOR_ENV).is_none()
}

/// The renderer for a format, writing to standard error.
pub(crate) fn renderer(format: Format, verbosity: Verbosity, replay: bool) -> Box<dyn Renderer> {
    let color = env::var_os(NO_COLOR_ENV).is_none();
    let debug = display::debug_logging(|name| env::var(name).ok());
    let chosen = chosen(format);
    let out: Box<dyn Write> = Box::new(io::stderr());
    let inner: Box<dyn Renderer> = match chosen {
        Format::Terminal => Box::new(terminal::Terminal::stderr(color, debug, replay)),
        Format::Github => Box::new(github::Github::new(out, env::var_os(GITHUB_SUMMARY_ENV).map(PathBuf::from), debug)),
        Format::Plain | Format::Auto => Box::new(plain::Plain::new(out, debug)),
    };
    Box::new(Filtered { inner, verbosity })
}

enum Message {
    Event(Box<Event>),
    Interrupt,
}

/// Runs `work` on another thread and renders its events here, with a tick when no event comes for a while. On SIGINT
/// or SIGTERM, the renderer prints what it kept, and the process exits; the sandboxes die with it.
pub(crate) fn drive<T: Send>(
    renderer: &mut dyn Renderer,
    start: Instant,
    work: impl FnOnce(&mut dyn FnMut(Event)) -> T + Send,
) -> anyhow::Result<T> {
    let (sender, receiver) = mpsc::channel();
    let interrupts = sender.clone();
    ctrlc::set_handler(move || {
        interrupts.send(Message::Interrupt).ok();
    })?;
    Ok(thread::scope(|scope| {
        let handle = scope.spawn(move || {
            work(&mut |event| {
                sender.send(Message::Event(Box::new(event))).ok();
            })
        });
        loop {
            match receiver.recv_timeout(TICK) {
                Ok(Message::Event(event)) => renderer.event(start.elapsed(), &event),
                Ok(Message::Interrupt) => {
                    renderer.interrupt(start.elapsed());
                    process::exit(INTERRUPTED_EXIT);
                }
                Err(RecvTimeoutError::Timeout) if handle.is_finished() => break,
                Err(RecvTimeoutError::Timeout) => renderer.tick(start.elapsed()),
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        handle.join().expect("the work thread does not panic")
    }))
}

/// `0.4s`, `12.3s`, `2m05s`.
pub(crate) fn duration(d: Duration) -> String {
    let seconds = d.as_secs_f64();
    match seconds < 60.0 {
        true => format!("{seconds:.1}s"),
        false => format!("{}m{:02}s", d.as_secs() / 60, d.as_secs() % 60),
    }
}

/// What a renderer knows of the run beyond one event: the names to show, which action started each execution, where
/// actions see the workspace, and whether debug messages show.
#[derive(Debug, Default)]
struct Names {
    titles: BTreeMap<String, String>,
    first: HashMap<Digest, String>,
    workspace_views: BTreeMap<String, WorkspaceView>,
    debug: bool,
}

impl Names {
    fn new(debug: bool) -> Self {
        Names { debug, ..Names::default() }
    }

    /// Takes in what an event says about names.
    fn event(&mut self, event: &Event) {
        match event {
            Event::Loaded { titles, workspace_views } => {
                self.titles = titles.clone();
                self.workspace_views = workspace_views.clone();
            }
            Event::Progress(azad::Progress::Started { name, action }) => {
                self.first.entry(*action).or_insert_with(|| name.clone());
            }
            _ => {}
        }
    }

    /// The name to show for an action: its title, or its label.
    fn title<'a>(&'a self, name: &'a str) -> &'a str {
        self.titles.get(name).map_or(name, String::as_str)
    }

    /// For an action that is done without having started: the action that started the execution that it shared.
    fn shared_with(&self, name: &str, status: &Status) -> Option<&str> {
        let action = match status {
            Status::Ran { action, .. } | Status::Failed { action, .. } => action,
            _ => return None,
        };
        let first = self.first.get(action).filter(|first| *first != name)?;
        Some(self.title(first))
    }

    /// The lines that a line of output shows.
    fn show(&self, line: &str) -> Vec<String> {
        display::show(line, self.debug).lines()
    }

    /// The annotation that a line of an action's output asks for, with its file as a path in the workspace.
    fn annotation(&self, name: &str, line: &str) -> Option<Annotation> {
        Annotation::of_line(line, self.workspace_views.get(name))
    }
}

/// `same action as <name>`, for an action that shared the execution of another.
fn shared_note(first: &str) -> String {
    format!("same action as {first}")
}

/// The last lines of a failed action's output, without the blank lines around them.
fn failure_excerpt(lines: &[String]) -> &[String] {
    let lines = without_blank_edges(lines);
    without_blank_edges(&lines[lines.len().saturating_sub(FAILURE_TAIL_LINES)..])
}

/// The lines between the first and the last line that is not blank.
fn without_blank_edges(lines: &[String]) -> &[String] {
    let end = lines.iter().rposition(|l| !is_blank(l)).map_or(0, |last| last + 1);
    let start = lines[..end].iter().position(|l| !is_blank(l)).unwrap_or(end);
    &lines[start..end]
}

fn is_blank(line: &str) -> bool {
    without_escapes(line).trim().is_empty()
}

/// The narrowest that `wrap` makes a line, so that a narrow terminal still shows whole words.
const MIN_WRAP_WIDTH: usize = 20;

/// Plain text as lines of at most `width` columns, broken where Unicode allows a line break, such as at a space or
/// after a `/`, and inside a word that is wider than a line.
fn wrap(text: &str, width: usize) -> Vec<String> {
    textwrap::wrap(text, width.max(MIN_WRAP_WIDTH)).into_iter().map(|line| line.into_owned()).collect()
}

/// Undeclared paths do not fail an action, but they are lost: no other action can read them. The note says so, so that
/// the author can declare them as outputs or write them to /tmp.
fn discarded_note(status: &Status) -> Option<String> {
    let Status::Ran { discarded, .. } = status else { return None };
    if discarded.is_empty() {
        return None;
    }
    let shown: Vec<&str> = discarded.iter().take(DISCARDED_SHOWN).map(String::as_str).collect();
    let rest = match discarded.len().saturating_sub(DISCARDED_SHOWN) {
        0 => String::new(),
        more => format!(" and {more} more"),
    };
    Some(format!("discarded {}{rest}: not declared as outputs; declare them, or write them to /tmp", shown.join(", ")))
}

/// The reason of a failed or skipped status, for messages.
fn reason(status: &Status) -> String {
    match status {
        Status::Failed { reason, .. } => reason.clone(),
        Status::Skipped => "skipped: a dependency failed".to_owned(),
        _ => String::new(),
    }
}

/// Events and an output buffer for the tests of the renderers.
#[cfg(test)]
mod testing {
    use std::collections::BTreeMap;
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use azad::frontend::WorkspaceView;
    use azad::ir::{ActionResult, Digest};
    use azad::log::Stream;
    use azad::{Event, Progress, Status};

    #[derive(Clone, Default)]
    pub(crate) struct Shared(Arc<Mutex<Vec<u8>>>);

    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Shared {
        pub(crate) fn lines(&self) -> Vec<String> {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap().lines().map(str::to_owned).collect()
        }
    }

    pub(crate) fn at(seconds: u64) -> Duration {
        Duration::from_secs(seconds)
    }

    pub(crate) fn digest(text: &str) -> Digest {
        Digest::of_bytes(text.as_bytes())
    }

    pub(crate) fn loaded(titles: &[(&str, &str)]) -> Event {
        let titles = titles.iter().map(|(label, title)| ((*label).to_owned(), (*title).to_owned())).collect();
        Event::Loaded { titles, workspace_views: BTreeMap::new() }
    }

    /// A goal whose actions see the workspace at `root`, and start in `start` below it.
    pub(crate) fn loaded_with_views(views: &[(&str, &str, &str)]) -> Event {
        let workspace_views = views
            .iter()
            .map(|(label, root, start)| {
                ((*label).to_owned(), WorkspaceView { root: (*root).to_owned(), start: (*start).to_owned() })
            })
            .collect();
        Event::Loaded { titles: BTreeMap::new(), workspace_views }
    }

    pub(crate) fn started(name: &str) -> Event {
        started_as(name, name)
    }

    /// An action that starts the execution of `action`, which other actions can share.
    pub(crate) fn started_as(name: &str, action: &str) -> Event {
        Event::Progress(Progress::Started { name: name.to_owned(), action: digest(action) })
    }

    pub(crate) fn log(name: &str, line: &str) -> Event {
        Event::Progress(Progress::Log { name: name.to_owned(), stream: Stream::Stdout, line: line.to_owned() })
    }

    pub(crate) fn ran(name: &str) -> Event {
        ran_as(name, name)
    }

    pub(crate) fn ran_as(name: &str, action: &str) -> Event {
        let empty = digest("");
        let result = ActionResult { exit_code: 0, outputs: BTreeMap::new(), stdout: empty, stderr: empty, log: None };
        let status = Status::Ran { action: digest(action), result, discarded: Vec::new() };
        Event::Progress(Progress::Done { name: name.to_owned(), status })
    }

    pub(crate) fn failed(name: &str, reason: &str) -> Event {
        let status = Status::Failed { action: digest(name), reason: reason.to_owned(), result: None };
        Event::Progress(Progress::Done { name: name.to_owned(), status })
    }

    pub(crate) fn cached(name: &str) -> Event {
        let empty = digest("");
        let result = ActionResult { exit_code: 0, outputs: BTreeMap::new(), stdout: empty, stderr: empty, log: None };
        let status = Status::Cached { action: digest(name), result };
        Event::Progress(Progress::Done { name: name.to_owned(), status })
    }

    /// A job's output with each kind of workflow command that the renderers show.
    pub(crate) const COMMANDS: [&str; 7] = [
        "::debug::hidden unless debug logging is on",
        "::group::Build",
        "inside",
        "::endgroup::",
        "::error file=a.rs,line=3::bad%0Anext",
        "::warning::careful 100%25",
        "::notice::fyi",
    ];
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    fn note(level: NoteLevel, text: &str) -> Event {
        Event::Note(Note { level, text: text.to_owned() })
    }

    #[test]
    fn the_failure_excerpt_drops_blank_lines_around_the_output_but_not_inside_it() {
        let lines: Vec<String> = ["", "\x1b[39m", "error", "", "detail", " ", "\x1b[0m"].map(str::to_owned).into();
        assert_eq!(failure_excerpt(&lines), ["error", "", "detail"]);
        let long: Vec<String> = (0..FAILURE_TAIL_LINES + 5).map(|i| i.to_string()).chain([String::new()]).collect();
        assert_eq!(failure_excerpt(&long).first().map(String::as_str), Some("5"));
        assert_eq!(failure_excerpt(&long).len(), FAILURE_TAIL_LINES);
    }

    /// The lines that each verbosity shows of a fallback (a normal note), a detail (a diagnostic note), a warning and
    /// a download, through the renderer that `make` gives.
    fn shown(make: impl Fn(Box<dyn Write>) -> Box<dyn Renderer>) -> Vec<(Verbosity, Vec<String>)> {
        [Verbosity::Minimal, Verbosity::Normal, Verbosity::Diagnostic]
            .into_iter()
            .map(|verbosity| {
                let out = Shared::default();
                let mut renderer = Filtered { inner: make(Box::new(out.clone())), verbosity };
                renderer.event(at(0), &note(NoteLevel::Normal, "the count came from GitHub's API"));
                renderer.event(at(0), &note(NoteLevel::Diagnostic, "HEAD named abc"));
                renderer.event(at(0), &Event::Warning("a glob matches no file".to_owned()));
                renderer.event(at(0), &Event::Pull { reference: "base@sha256:1".to_owned() });
                let lines = out.lines();
                let texts = lines.iter().map(|line| line.split_once("] ").map_or(line.as_str(), |(_, l)| l).to_owned());
                (verbosity, texts.collect())
            })
            .collect()
    }

    #[test]
    fn each_verbosity_shows_the_decisions_of_its_level_and_the_levels_below() {
        let plain = shown(|out| Box::new(plain::Plain::new(out, false)));
        assert_eq!(plain[0].1, vec!["warning: a glob matches no file"]);
        assert_eq!(
            plain[1].1,
            vec!["note: the count came from GitHub's API", "warning: a glob matches no file", "pull    base@sha256:1"]
        );
        assert_eq!(plain[2].1.len(), 4);
        assert_eq!(plain[2].1[1], "note: HEAD named abc");

        let github = shown(|out| Box::new(github::Github::new(out, None, false)));
        assert!(github[1].1.contains(&"::notice title=Azad::the count came from GitHub's API".to_owned()));
        assert!(!github[1].1.iter().any(|line| line.contains("HEAD named")));
        assert!(github[2].1.contains(&"note: HEAD named abc".to_owned()));
    }

    #[test]
    fn escapes_are_removed_from_a_line_of_cargo() {
        let line = "\x1b[1m\x1b[92m    Finished\x1b[0m \x1b]8;;https://doc.rust-lang.org/cargo\x1b\\`dev` profile\x1b]8;;\x07 in 1.30s";
        assert_eq!(without_escapes(line), "    Finished `dev` profile in 1.30s");
    }

    #[test]
    fn durations_read_well() {
        assert_eq!(duration(Duration::from_millis(400)), "0.4s");
        assert_eq!(duration(Duration::from_millis(12_340)), "12.3s");
        assert_eq!(duration(Duration::from_secs(125)), "2m05s");
    }
}
