//! A live view for an interactive terminal, drawn with indicatif: finished actions scroll up as lines, and a live
//! area at the bottom shows each running action with a spinner and its time. Below each, it shows the last lines of
//! the action's output, as many as fit; when the actions nearly fill the live area, only the last line, beside the
//! time.

use std::collections::BTreeMap;
use std::time::Duration;

use azad::{Event, Progress, Status};
use console::{Style, Term, strip_ansi_codes, truncate_str};
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressState, ProgressStyle};

use super::echo::EchoOff;
use super::{
    Names, Renderer, Summary, discarded_note, duration, failure_excerpt, is_blank, reason, shared_note,
    without_blank_edges, wrap,
};

/// The frames of a running action's spinner; indicatif shows the last one when the action is done.
const SPINNER: [&str; 11] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", " "];
const LIVE_TEMPLATE: &str = "{spinner:.cyan} {prefix:.cyan} {took:>7.cyan}  {msg}";
/// The share of the terminal's rows that the live area uses for its actions and their output, so that finished
/// actions still scroll above it.
const LIVE_ROWS_NUMERATOR: usize = 2;
const LIVE_ROWS_DENOMINATOR: usize = 3;
const MAX_TAIL_LINES: usize = 10;
const CUT: &str = "…";
const REFRESH_HZ: u8 = 20;
const NAME_WIDTH: usize = 40;
const MIN_NAME_WIDTH: usize = 12;
/// What a row needs beside the names: the mark, the time, a short detail such as `skipped: a dependency failed`, and
/// the last column, which a line leaves free.
const DETAIL_ROOM: usize = 41;
const TOOK_WIDTH: usize = 7;
/// Where the name starts in a row: after the mark and a space.
const NAME_AT: usize = 2;
const OUTPUT_BAR: &str = "  │ ";
const TAB_STOP: &str = "    ";

struct Running {
    since: Duration,
    lines: Vec<String>,
    bar: ProgressBar,
}

pub(crate) struct Terminal {
    multi: MultiProgress,
    live: ProgressStyle,
    color: bool,
    running: BTreeMap<String, Running>,
    names: Names,
    /// The longest name of the goal's actions, so far.
    longest_name: usize,
    /// The terminal's rows and columns, when they are not the terminal's own.
    size: Option<(usize, usize)>,
    /// Whether the view turns the terminal's echo off while actions run.
    quiets_echo: bool,
    echo: Option<EchoOff>,
    /// Whether every action's output shows, not only a failure's.
    replay: bool,
    /// The output of cache hits, which `--replay` sends before their result.
    replayed: BTreeMap<String, Vec<String>>,
}

impl Terminal {
    /// The view on standard error. It draws even when standard error is not a terminal, because the command line
    /// chose this view.
    pub(crate) fn stderr(color: bool, debug: bool, replay: bool) -> Self {
        let target = ProgressDrawTarget::term_like_with_hz(Box::new(Term::buffered_stderr()), REFRESH_HZ);
        Terminal { quiets_echo: true, replay, ..Terminal::new(target, None, color, debug) }
    }

    fn new(target: ProgressDrawTarget, size: Option<(usize, usize)>, color: bool, debug: bool) -> Self {
        let live = ProgressStyle::with_template(LIVE_TEMPLATE)
            .expect("the live template is valid")
            .tick_strings(&SPINNER)
            .with_key("took", |state: &ProgressState, out: &mut dyn std::fmt::Write| {
                write!(out, "{}", duration(state.elapsed())).ok();
            });
        Terminal {
            multi: MultiProgress::with_draw_target(target),
            live,
            color,
            running: BTreeMap::new(),
            names: Names::new(debug),
            longest_name: 0,
            size,
            quiets_echo: false,
            echo: None,
            replay: false,
            replayed: BTreeMap::new(),
        }
    }

    fn paint(&self, style: Style, text: &str) -> String {
        match self.color {
            true => style.force_styling(true).apply_to(text).to_string(),
            false => text.to_owned(),
        }
    }

    /// Writes lines above the live area.
    fn print(&self, lines: &[String]) {
        self.multi.println(lines.join("\n")).ok();
    }

    /// The lines of a row in the columns of the live area: mark, name, time, detail. A name too long for its column
    /// has a line of its own when the row does not fit the terminal, and a long detail continues below itself.
    fn row(&self, style: Style, mark: &str, name: &str, took: &str, detail: &str) -> Vec<String> {
        let column = self.name_column();
        let shown = name.chars().count().max(column);
        let fits = NAME_AT + shown + 1 + TOOK_WIDTH + 2 + detail.chars().count() < self.columns();
        if shown == column || fits {
            let head = format!("{} {name:<shown$}", self.paint(style.clone(), mark));
            return self.timed(style, head, NAME_AT + shown, took, detail);
        }
        let mut lines = self.block(style.clone(), &format!("{mark} "), name);
        lines.extend(self.timed(style, " ".repeat(NAME_AT + column), NAME_AT + column, took, detail));
        lines
    }

    /// `head`, which is `head_width` wide, then the time and the detail, which continues below itself.
    fn timed(&self, style: Style, head: String, head_width: usize, took: &str, detail: &str) -> Vec<String> {
        let detail_at = head_width + 1 + TOOK_WIDTH + 2;
        let mut details = wrap(detail, self.columns().saturating_sub(detail_at + 1)).into_iter();
        let first = details.next().unwrap_or_default();
        let head = format!("{head} {}", self.paint(Style::new().dim(), &format!("{took:>TOOK_WIDTH$}")));
        let mut lines = match first.is_empty() {
            true => vec![head.trim_end().to_owned()],
            false => vec![format!("{head}  {}", self.paint(style.clone(), &first))],
        };
        lines.extend(details.map(|rest| format!("{}{}", " ".repeat(detail_at), self.paint(style.clone(), &rest))));
        lines
    }

    /// `text` after `prefix`, wrapped to the terminal, with its later lines under the text.
    fn block(&self, style: Style, prefix: &str, text: &str) -> Vec<String> {
        let indent = prefix.chars().count();
        wrap(text, self.columns().saturating_sub(indent + 1))
            .into_iter()
            .enumerate()
            .map(|(i, line)| match i {
                0 => self.paint(style.clone(), &format!("{prefix}{line}")),
                _ => self.paint(style.clone(), &format!("{}{line}", " ".repeat(indent))),
            })
            .collect()
    }

    /// Output lines, wrapped to the terminal, with the bar on every line. They are painted as one, so the output's
    /// own colors go.
    fn barred(&self, style: Style, output: &[String]) -> Vec<String> {
        let room = self.columns().saturating_sub(OUTPUT_BAR.chars().count() + 1);
        output
            .iter()
            .flat_map(|line| wrap(&strip_ansi_codes(line).replace('\t', TAB_STOP), room))
            .map(|line| self.paint(style.clone(), format!("{OUTPUT_BAR}{line}").trim_end()))
            .collect()
    }

    /// The width of the names' column: wide enough for the goal's names, and narrow enough for the details.
    fn name_column(&self) -> usize {
        let widest = self.columns().saturating_sub(DETAIL_ROOM).max(MIN_NAME_WIDTH);
        self.longest_name.max(NAME_WIDTH).min(widest)
    }

    /// The name of a running action, cut to its column from the left, so that its package and name stay.
    fn live_name(&self, label: &str) -> String {
        let column = self.name_column();
        let title = self.names.title(label);
        let count = title.chars().count();
        if count <= column {
            return format!("{title:<column$}");
        }
        let kept: String = title.chars().skip(count - column.saturating_sub(1)).collect();
        format!("…{kept}")
    }

    fn see_name(&mut self, name: &str) {
        let before = self.name_column();
        self.longest_name = self.longest_name.max(name.chars().count());
        if self.name_column() == before {
            return;
        }
        for (label, running) in &self.running {
            running.bar.set_prefix(self.live_name(label));
        }
        self.refresh_live();
    }

    /// The process exits after an interrupt without dropping the view, so the echo comes back here.
    fn restore_echo(&mut self) {
        if let Some(echo) = self.echo.as_mut() {
            echo.restore();
        }
    }

    fn columns(&self) -> usize {
        self.size.map_or_else(|| usize::from(Term::stderr().size().1), |(_, columns)| columns)
    }

    fn rows(&self) -> usize {
        self.size.map_or_else(|| usize::from(Term::stderr().size().0), |(rows, _)| rows)
    }

    /// How many lines of output each running action shows below its row: as many as its share of the live area
    /// holds, and none when the actions alone nearly fill it.
    fn tail_lines(&self) -> usize {
        let actions = self.running.len().max(1);
        let room = self.rows() * LIVE_ROWS_NUMERATOR / LIVE_ROWS_DENOMINATOR;
        (room.saturating_sub(actions) / actions).min(MAX_TAIL_LINES)
    }

    /// What a running action shows of its output: the last `tail` lines below its row, or with no tail, the last
    /// line beside its time. Each line is cut to the terminal, so that the live area has the height it counts.
    fn live_output(&self, lines: &[String], tail: usize) -> String {
        let shown: Vec<String> = lines
            .iter()
            .rev()
            .filter(|line| !is_blank(line))
            .take(tail.max(1))
            .map(|line| strip_ansi_codes(line).replace('\t', TAB_STOP))
            .collect();
        if tail == 0 {
            let room = self.columns().saturating_sub(NAME_AT + self.name_column() + 1 + TOOK_WIDTH + 2 + 1);
            let last = shown.first().map(String::as_str).unwrap_or_default();
            return self.paint(Style::new().dim(), &truncate_str(last, room, CUT));
        }
        let room = self.columns().saturating_sub(OUTPUT_BAR.chars().count() + 1);
        shown
            .iter()
            .rev()
            .map(|line| {
                format!(
                    "\n{}",
                    self.paint(Style::new().dim(), &format!("{OUTPUT_BAR}{}", truncate_str(line, room, CUT)))
                )
            })
            .collect()
    }

    /// Shows the output of every running action again, for a new number of actions or a new width of names.
    fn refresh_live(&self) {
        let tail = self.tail_lines();
        for running in self.running.values() {
            running.bar.set_message(self.live_output(&running.lines, tail));
        }
    }

    /// The first action turns the echo off: actions start only after the run handles signals, so an interrupt always
    /// turns it back on.
    fn start(&mut self, at: Duration, label: &str) {
        if self.quiets_echo && self.echo.is_none() {
            self.echo = Some(EchoOff::new());
        }
        let title = self.names.title(label).to_owned();
        self.see_name(&title);
        let bar = ProgressBar::new_spinner().with_style(self.live.clone()).with_prefix(self.live_name(label));
        let bar = self.multi.add(bar);
        bar.tick();
        self.running.insert(label.to_owned(), Running { since: at, lines: Vec::new(), bar });
        self.refresh_live();
    }

    /// Takes an action out of the live area, and gives its output and how long it ran.
    fn stop(&mut self, at: Duration, label: &str) -> (Vec<String>, String) {
        let Some(running) = self.running.remove(label) else { return (Vec::new(), String::new()) };
        running.bar.finish_and_clear();
        self.multi.remove(&running.bar);
        self.refresh_live();
        (running.lines, duration(at.saturating_sub(running.since)))
    }

    fn done(&mut self, at: Duration, label: &str, status: &Status) {
        let (output, took) = self.stop(at, label);
        let shared = self.names.shared_with(label, status).map(shared_note).unwrap_or_default();
        let name = self.names.title(label).to_owned();
        let mut lines = match status {
            Status::Ran { .. } => {
                let mut lines = self.row(Style::new().green(), "✓", &name, &took, &shared);
                if self.replay {
                    lines.extend(self.barred(Style::new().dim(), without_blank_edges(&output)));
                }
                lines
            }
            Status::Cached { .. } => {
                let mut lines = self.row(Style::new().dim(), "•", &name, "", "cached");
                let replayed = self.replayed.remove(label).unwrap_or_default();
                lines.extend(self.barred(Style::new().dim(), without_blank_edges(&replayed)));
                lines
            }
            Status::Skipped => self.row(Style::new().yellow(), "↷", &name, "", "skipped: a dependency failed"),
            Status::Failed { .. } => {
                let mut lines = self.row(Style::new().red(), "✗", &name, &took, &reason(status));
                lines.extend(self.barred(Style::new().red(), failure_excerpt(&output)));
                lines
            }
            Status::NotNeeded | Status::WillRun { .. } | Status::Waiting => return,
        };
        if let Some(note) = discarded_note(status) {
            lines.extend(self.block(Style::new().yellow(), "  note: ", &note));
        }
        self.print(&lines);
    }
}

impl Renderer for Terminal {
    fn event(&mut self, at: Duration, event: &Event) {
        self.names.event(event);
        let lines = match event {
            Event::Pull { reference } => self.block(Style::new().dim(), "↓ pull  ", reference),
            Event::Remote(summary) => self.block(Style::new().dim(), "☁ ", &summary.to_string()),
            Event::Timings(steps) => {
                steps.iter().flat_map(|step| self.block(Style::new().dim(), "⏱ ", &step.to_string())).collect()
            }
            Event::Fetch { url, rev } => self.block(Style::new().dim(), "↓ fetch ", &format!("{url}@{rev}")),
            Event::Warning(message) => self.block(Style::new().yellow(), "! warning: ", message),
            Event::Note(note) => self.block(Style::new().dim(), "· note: ", &note.text),
            Event::Loaded { workspace_views, .. } => {
                let names: Vec<String> =
                    workspace_views.keys().map(|label| self.names.title(label).to_owned()).collect();
                names.iter().for_each(|name| self.see_name(name));
                return;
            }
            Event::Progress(Progress::Started { name, .. }) => return self.start(at, name),
            Event::Progress(Progress::Log { name, line, .. }) => {
                let shown = self.names.show(line);
                let tail = self.tail_lines();
                let Some(running) = self.running.get_mut(name) else {
                    self.replayed.entry(name.clone()).or_default().extend(shown);
                    return;
                };
                running.lines.extend(shown);
                let running = &self.running[name];
                running.bar.set_message(self.live_output(&running.lines, tail));
                return;
            }
            Event::Progress(Progress::Done { name, status }) => return self.done(at, name, status),
        };
        self.print(&lines);
    }

    fn interrupt(&mut self, at: Duration) {
        let labels: Vec<String> = self.running.keys().cloned().collect();
        let mut lines = Vec::new();
        for label in labels {
            let (_, took) = self.stop(at, &label);
            let title = self.names.title(&label).to_owned();
            lines.extend(self.row(Style::new().yellow(), "■", &title, &took, "stopped"));
        }
        self.print(&lines);
        self.restore_echo();
    }

    fn tick(&mut self, _at: Duration) {
        for running in self.running.values() {
            running.bar.tick();
        }
    }

    fn finish(&mut self, at: Duration, summary: &Summary) {
        let (mark, verdict, style) = match summary.holds {
            true => ("✓", "holds", Style::new().green()),
            false => ("✗", "does not hold", Style::new().red()),
        };
        let text = format!(
            "{} {verdict} · {} · {} · run {}",
            summary.goal,
            summary.counts(),
            duration(at),
            summary.run_number
        );
        let lines = self.block(style.bold(), &format!("{mark} "), &text);
        self.print(&lines);
        self.restore_echo();
    }
}

#[cfg(test)]
mod tests {
    use indicatif::InMemoryTerm;

    use super::super::testing::*;
    use super::*;

    const ROWS: u16 = 60;

    /// A view on an in-memory terminal of `width` columns, and the terminal, whose screen the tests read.
    fn screen(width: u16) -> (Terminal, InMemoryTerm) {
        screen_of(ROWS, width)
    }

    fn screen_of(rows: u16, width: u16) -> (Terminal, InMemoryTerm) {
        let term = InMemoryTerm::new(rows, width);
        let target = ProgressDrawTarget::term_like(Box::new(term.clone()));
        (Terminal::new(target, Some((usize::from(rows), usize::from(width))), false, false), term)
    }

    /// The screen after a redraw of the live area: indicatif limits how often updates draw, and a line above the live
    /// area always draws.
    fn drawn(terminal: &Terminal, term: &InMemoryTerm) -> Vec<String> {
        terminal.multi.println("").ok();
        lines(term)
    }

    fn lines(term: &InMemoryTerm) -> Vec<String> {
        term.contents().lines().filter(|l| !l.trim().is_empty()).map(str::to_owned).collect()
    }

    /// With `--replay`, an action that succeeds shows its output, and a cache hit shows the output that comes before
    /// its result; without it, only the rows show.
    #[test]
    fn replay_shows_the_output_of_actions_that_succeed_and_of_cache_hits() {
        for replay in [false, true] {
            let (terminal, term) = screen(80);
            let mut terminal = Terminal { replay, ..terminal };
            terminal.event(at(0), &started("//a:build"));
            terminal.event(at(1), &log("//a:build", "built 3 files"));
            terminal.event(at(1), &ran("//a:build"));
            if replay {
                terminal.event(at(1), &log("//b:lint", "1 warning"));
            }
            terminal.event(at(1), &cached("//b:lint"));

            let shown = lines(&term);
            let a = shown.iter().position(|l| l.starts_with("✓ //a:build")).expect("the row of a");
            let b = shown.iter().position(|l| l.starts_with("• //b:lint")).expect("the row of b");
            let expected = match replay {
                true => vec![format!("{OUTPUT_BAR}built 3 files"), format!("{OUTPUT_BAR}1 warning")],
                false => Vec::new(),
            };
            let output: Vec<String> =
                [shown.get(a + 1..b), shown.get(b + 1..)].into_iter().flatten().flatten().cloned().collect();
            assert_eq!(output, expected, "replay {replay}: {shown:#?}");
        }
    }

    #[test]
    fn a_warning_stays_above_the_running_actions() {
        let (mut terminal, term) = screen(100);
        terminal.event(at(0), &started("a"));
        terminal.event(at(1), &Event::Warning("glob([\"scr/*.py\"]) in //app matches no file".to_owned()));
        let shown = lines(&term);
        let warning = shown.iter().position(|l| l.contains("! warning: glob([\"scr/*.py\"]) in //app matches no file"));
        let running = shown.iter().position(|l| l.contains(" a "));
        assert!(warning.is_some() && warning < running, "{shown:#?}");
    }

    /// The failure of an action with a long name and long output, in a narrow terminal: no line wraps on the screen,
    /// the whole name is there once, every line of the output has the bar, all the output's words are there, and the
    /// blank lines that end the output and the live area are not.
    #[test]
    fn a_failure_wraps_inside_the_terminal() {
        let name = "//services/legacy/notifications/src/Example.Notifications.Frontend/Frontend:fmt";
        let message = "The following Gradle properties are missing for 'maps' credentials, which the build \
                       reads when it configures every project";
        let (mut terminal, term) = screen(60);
        terminal.event(at(0), &started(name));
        for line in [message, "\t- mapsAccessKey", "", "  "] {
            terminal.event(at(1), &log(name, line));
        }
        terminal.event(at(2), &failed(name, "exit code 1"));

        let shown = lines(&term);
        let status = shown.iter().position(|l| l.ends_with("2.0s  exit code 1")).expect("the status");
        let shown_name: String = shown[..status].iter().map(|l| l.trim_start_matches("✗ ").trim()).collect();
        assert_eq!(shown_name, name);
        assert!(shown[0].starts_with("✗ ") && !shown[status].contains('✗'), "{shown:#?}");
        let output: Vec<&str> =
            shown[status + 1..].iter().map(|l| l.strip_prefix(OUTPUT_BAR).expect("the bar on every line")).collect();
        assert_eq!(output.last(), Some(&"    - mapsAccessKey"));
        assert_eq!(output[..output.len() - 1].join(" "), message.split_whitespace().collect::<Vec<_>>().join(" "));
    }

    /// A name wider than its column, in a row that still fits the terminal, stays on the row's line.
    #[test]
    fn a_row_that_fits_stays_on_one_line() {
        let name = "//services/legacy/shipping:fmt-check";
        let (mut terminal, term) = screen(60);
        terminal.event(at(0), &started(name));
        terminal.event(at(1), &ran(name));
        let shown = lines(&term);
        assert_eq!(shown.len(), 1, "{shown:#?}");
        assert!(shown[0].starts_with(&format!("✓ {name} ")) && shown[0].ends_with("1.0s"), "{shown:#?}");
    }

    /// More running actions than the terminal has rows: the screen keeps the lines above the live area, and has no
    /// copies of the live area when the actions are done.
    #[test]
    fn a_live_area_taller_than_the_terminal_leaves_nothing_behind() {
        const SMALL_ROWS: u16 = 8;
        let (mut terminal, term) = screen_of(SMALL_ROWS, 80);
        terminal.event(at(0), &Event::Warning("before".to_owned()));
        let names: Vec<String> = (0..20).map(|i| format!("//pkg:task-{i}")).collect();
        names.iter().for_each(|name| terminal.event(at(0), &started(name)));
        names.iter().for_each(|name| terminal.event(at(1), &ran(name)));
        let shown = lines(&term);
        assert!(shown.iter().all(|l| l.starts_with("✓ //pkg:task-")), "{shown:#?}");
        assert_eq!(shown.last().map(|l| l.starts_with("✓ //pkg:task-19 ")), Some(true), "{shown:#?}");
    }

    /// Few running actions: each shows the last lines of its output below its row, the newest last, without blank
    /// lines, and no more than its share.
    #[test]
    fn few_actions_show_the_tail_of_their_output_below_them() {
        let (mut terminal, term) = screen_of(40, 80);
        terminal.event(at(0), &started("//a:build"));
        terminal.event(at(0), &started("//b:test"));
        for i in 0..MAX_TAIL_LINES + 2 {
            terminal.event(at(1), &log("//a:build", &format!("compiling {i}")));
            terminal.event(at(1), &log("//a:build", ""));
        }
        terminal.event(at(1), &log("//b:test", "running 3 tests"));

        let shown = drawn(&terminal, &term);
        let a = shown.iter().position(|l| l.contains("//a:build")).expect("the row of a");
        let b = shown.iter().position(|l| l.contains("//b:test")).expect("the row of b");
        assert!(!shown[a].contains("compiling"), "{shown:#?}");
        let tail: Vec<&str> = shown[a + 1..b].iter().map(|l| l.strip_prefix(OUTPUT_BAR).expect("the bar")).collect();
        let expected: Vec<String> = (2..MAX_TAIL_LINES + 2).map(|i| format!("compiling {i}")).collect();
        assert_eq!(tail, expected);
        assert_eq!(shown.get(b + 1).map(String::as_str), Some("  │ running 3 tests"), "{shown:#?}");
    }

    /// When the running actions nearly fill the live area, each shows only its last line, beside its time.
    #[test]
    fn many_actions_show_their_last_line_beside_the_time() {
        let (mut terminal, term) = screen_of(12, 80);
        let names: Vec<String> = (0..5).map(|i| format!("//pkg:task-{i}")).collect();
        for name in &names {
            terminal.event(at(0), &started(name));
            terminal.event(at(1), &log(name, "first"));
            terminal.event(at(1), &log(name, &format!("last of {name}")));
        }
        let shown = drawn(&terminal, &term);
        assert!(!shown.iter().any(|l| l.starts_with(OUTPUT_BAR.trim_end())), "{shown:#?}");
        for name in &names {
            let row = shown.iter().find(|l| l.contains(&format!("{name} "))).expect("the row");
            assert!(row.ends_with(&format!("last of {name}")), "{shown:#?}");
        }
    }
}
