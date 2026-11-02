//! Plain lines for logs and pipes: each event on its own line, with the time since the start; output lines carry the
//! name of their action, so a log can be read and searched.

use std::collections::HashMap;
use std::io::Write;
use std::time::Duration;

use azad::{Event, Progress, Status};

use super::{Names, Renderer, Summary, discarded_note, duration, reason, shared_note};

pub(crate) struct Plain {
    out: Box<dyn Write>,
    started: HashMap<String, Duration>,
    names: Names,
}

impl Plain {
    pub(crate) fn new(out: Box<dyn Write>, debug: bool) -> Self {
        Plain { out, started: HashMap::new(), names: Names::new(debug) }
    }

    fn line(&mut self, at: Duration, text: &str) {
        writeln!(self.out, "[{:>7}] {text}", duration(at)).ok();
    }

    /// The time an action took, or what it shared when it did not start.
    fn took(&self, name: &str, status: &Status, at: Duration) -> String {
        if let Some(start) = self.started.get(name) {
            return duration(at.saturating_sub(*start));
        }
        self.names.shared_with(name, status).map(shared_note).unwrap_or_default()
    }

    fn done(&mut self, at: Duration, name: &str, status: &Status) {
        let took = self.took(name, status, at);
        let title = self.names.title(name);
        let text = match status {
            Status::Ran { .. } => format!("ok      {title}  {took}"),
            Status::Cached { .. } => format!("cached  {title}"),
            Status::Skipped => format!("skipped {title}"),
            Status::Failed { .. } => format!("FAILED  {title}: {}  {took}", reason(status)),
            Status::NotNeeded | Status::WillRun { .. } | Status::Waiting => return,
        };
        self.line(at, text.trim_end());
        if let Some(note) = discarded_note(status) {
            writeln!(self.out, "          note: {note}").ok();
        }
    }
}

impl Renderer for Plain {
    fn event(&mut self, at: Duration, event: &Event) {
        self.names.event(event);
        match event {
            Event::Pull { reference } => self.line(at, &format!("pull    {reference}")),
            Event::Fetch { url, rev } => self.line(at, &format!("fetch   {url}@{rev}")),
            Event::Remote(summary) => self.line(at, &summary.to_string()),
            Event::Timings(steps) => {
                for step in steps {
                    self.line(at, &format!("took    {step}"));
                }
            }
            Event::Warning(message) => self.line(at, &format!("warning: {message}")),
            Event::Note(note) => self.line(at, &format!("note: {}", note.text)),
            Event::Loaded { .. } => {}
            Event::Progress(Progress::Started { name, .. }) => {
                self.started.insert(name.clone(), at);
                let text = format!("run     {}", self.names.title(name));
                self.line(at, &text);
            }
            Event::Progress(Progress::Log { name, line, .. }) => {
                let title = self.names.title(name);
                for shown in self.names.show(line) {
                    writeln!(self.out, "          {title} | {shown}").ok();
                }
            }
            Event::Progress(Progress::Done { name, status }) => self.done(at, name, status),
        }
    }

    fn finish(&mut self, at: Duration, summary: &Summary) {
        let verdict = match summary.holds {
            true => "holds",
            false => "does not hold",
        };
        let text = format!("goal {} {verdict}: {} (run {})", summary.goal, summary.counts(), summary.run_number);
        self.line(at, &text);
        self.out.flush().ok();
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::*;
    use super::*;

    fn renderer(debug: bool) -> (Plain, Shared) {
        let out = Shared::default();
        (Plain::new(Box::new(out.clone()), debug), out)
    }

    /// The lines without the time in front.
    fn texts(out: &Shared) -> Vec<String> {
        out.lines().iter().map(|line| line.split_once("] ").map_or(line.trim(), |(_, text)| text).to_owned()).collect()
    }

    #[test]
    fn workflow_commands_show_as_github_shows_them() {
        for (debug, first) in [(false, None), (true, Some("a | Debug: hidden unless debug logging is on"))] {
            let (mut plain, out) = renderer(debug);
            plain.event(at(0), &started("a"));
            for line in COMMANDS {
                plain.event(at(1), &log("a", line));
            }
            let mut expected: Vec<&str> = vec!["run     a"];
            expected.extend(first);
            expected.extend(["a | ▸ Build", "a | inside", "a | Error: a.rs:3: bad", "a | next"]);
            expected.extend(["a | Warning: careful 100%", "a | Notice: fyi"]);
            assert_eq!(texts(&out), expected, "debug: {debug}");
        }
    }

    #[test]
    fn a_warning_shows_as_a_line() {
        let (mut plain, out) = renderer(false);
        plain.event(at(0), &Event::Warning("glob([\"scr/*.py\"]) in //app matches no file".to_owned()));
        assert_eq!(texts(&out), ["warning: glob([\"scr/*.py\"]) in //app matches no file"]);
    }

    #[test]
    fn a_matrix_job_shows_its_title() {
        let (mut plain, out) = renderer(false);
        plain.event(at(0), &loaded(&[("//ci.yml:test.1", "//ci.yml:test.1 (3.12, slow)")]));
        plain.event(at(0), &started("//ci.yml:test.1"));
        plain.event(at(1), &log("//ci.yml:test.1", "hello"));
        plain.event(at(2), &ran("//ci.yml:test.1"));
        assert_eq!(
            texts(&out),
            [
                "run     //ci.yml:test.1 (3.12, slow)",
                "//ci.yml:test.1 (3.12, slow) | hello",
                "ok      //ci.yml:test.1 (3.12, slow)  2.0s"
            ]
        );
    }

    #[test]
    fn each_action_of_a_shared_execution_has_its_row() {
        let (mut plain, out) = renderer(false);
        plain.event(at(0), &started_as("one", "same"));
        plain.event(at(1), &ran_as("one", "same"));
        plain.event(at(1), &ran_as("two", "same"));
        plain.event(at(2), &cached("three"));
        assert_eq!(
            texts(&out),
            ["run     one", "ok      one  1.0s", "ok      two  same action as one", "cached  three"]
        );
    }
}
