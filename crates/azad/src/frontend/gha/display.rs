//! How a line of a job's output reads to a person.
//!
//! GitHub's runner and log viewer act on the workflow commands in a step's output: they hide debug messages unless
//! debug logging is on, fold groups, and mark errors and warnings. Azad shows the output in a terminal or a log, so
//! it shows these commands as text. The runner in the sandbox does not do this: the stored log keeps the commands, and
//! whether debug logging is on is a choice of the person who reads the log, not an input of the job.

use azad_gha_runner::workflow_command;

const DEBUG: &str = "debug";
const NOTICE: &str = "notice";
const WARNING: &str = "warning";
const ERROR: &str = "error";
const GROUP: &str = "group";
const END_GROUP: &str = "endgroup";
const FILE_PROPERTY: &str = "file";
const LINE_PROPERTY: &str = "line";
const COLUMN_PROPERTY: &str = "col";
const TITLE_PROPERTY: &str = "title";
const GROUP_MARK: &str = "▸ ";
/// The variables that turn on debug logging, as GitHub sets them when a run is re-run with debug logging.
pub const DEBUG_VARIABLES: [&str; 2] = ["RUNNER_DEBUG", "ACTIONS_STEP_DEBUG"];
const DEBUG_VALUES: [&str; 2] = ["1", "true"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Debug,
    Notice,
    Warning,
    Error,
}

impl Level {
    pub(crate) fn from_command(name: &str) -> Option<Self> {
        match name {
            DEBUG => Some(Level::Debug),
            NOTICE => Some(Level::Notice),
            WARNING => Some(Level::Warning),
            ERROR => Some(Level::Error),
            _ => None,
        }
    }

    /// The name of the workflow command of this level.
    pub fn command(self) -> &'static str {
        match self {
            Level::Debug => DEBUG,
            Level::Notice => NOTICE,
            Level::Warning => WARNING,
            Level::Error => ERROR,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Level::Debug => "Debug",
            Level::Notice => "Notice",
            Level::Warning => "Warning",
            Level::Error => "Error",
        }
    }
}

/// What a line of output shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shown {
    /// Nothing: the end of a group, or a debug message when debug logging is off.
    Hidden,
    /// The line as it is.
    Text(String),
    /// The start of a group, with its title.
    Group(String),
    /// A message, such as an error, with its location and title in front of the message.
    Message(Level, String),
}

impl Shown {
    /// The lines to print: a decoded line break in a message or a title gives more than one.
    pub fn lines(&self) -> Vec<String> {
        match self {
            Shown::Hidden => Vec::new(),
            Shown::Text(text) => vec![text.clone()],
            Shown::Group(title) => with_prefix(GROUP_MARK, title),
            Shown::Message(level, text) => with_prefix(&format!("{}: ", level.label()), text),
        }
    }
}

/// The lines of a text, with a prefix on the first.
fn with_prefix(prefix: &str, text: &str) -> Vec<String> {
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    match lines.first_mut() {
        Some(first) => *first = format!("{prefix}{first}"),
        None => lines.push(prefix.trim_end().to_owned()),
    }
    lines
}

/// How a line of output shows, with debug messages when `debug` holds.
pub fn show(line: &str, debug: bool) -> Shown {
    let Some(command) = workflow_command(line) else {
        return Shown::Text(line.to_owned());
    };
    if command.name == GROUP {
        return Shown::Group(command.message);
    }
    if command.name == END_GROUP {
        return Shown::Hidden;
    }
    match Level::from_command(&command.name) {
        Some(Level::Debug) if !debug => Shown::Hidden,
        Some(level) => {
            let location = location(&command.properties);
            let title = command.properties.get(TITLE_PROPERTY).map(|title| format!("{title}: "));
            Shown::Message(level, format!("{location}{}{}", title.unwrap_or_default(), command.message))
        }
        None => Shown::Text(line.to_owned()),
    }
}

/// `file:line:col: `, as far as the properties give it.
fn location(properties: &std::collections::BTreeMap<String, String>) -> String {
    let Some(file) = properties.get(FILE_PROPERTY) else {
        return String::new();
    };
    let position = [LINE_PROPERTY, COLUMN_PROPERTY].iter().map_while(|key| properties.get(*key));
    let parts: Vec<&str> = std::iter::once(file).chain(position).map(String::as_str).collect();
    format!("{}: ", parts.join(":"))
}

/// Whether debug logging is on, from the value of each of `DEBUG_VARIABLES`.
pub fn debug_logging(variable: impl Fn(&str) -> Option<String>) -> bool {
    DEBUG_VARIABLES
        .iter()
        .filter_map(|name| variable(name))
        .any(|value| DEBUG_VALUES.iter().any(|on| value.trim().eq_ignore_ascii_case(on)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(line: &str, debug: bool) -> Vec<String> {
        show(line, debug).lines()
    }

    #[test]
    fn debug_messages_show_only_with_debug_logging() {
        assert!(lines("::debug::setup-go: found in cache", false).is_empty());
        assert_eq!(lines("::debug::setup-go: found in cache", true), ["Debug: setup-go: found in cache"]);
    }

    #[test]
    fn a_group_shows_its_title_and_its_end_shows_nothing() {
        assert_eq!(lines("::group::Run tests", false), ["▸ Run tests"]);
        assert_eq!(lines("::group::one%0Atwo", false), ["▸ one", "two"]);
        assert!(lines("::endgroup::", false).is_empty());
    }

    #[test]
    fn messages_are_marked_and_decoded() {
        assert_eq!(lines("::error file=a.rs,line=3,col=7::bad%0Anext", false), ["Error: a.rs:3:7: bad", "next"]);
        assert_eq!(lines("::warning title=Lint::100%25 careful", false), ["Warning: Lint: 100% careful"]);
        assert_eq!(lines("::notice::fyi", false), ["Notice: fyi"]);
        assert_eq!(lines("::error::", false), ["Error:"]);
    }

    #[test]
    fn other_lines_and_commands_show_as_they_are() {
        assert_eq!(lines("plain text", false), ["plain text"]);
        assert_eq!(lines("::stop-commands::token", false), ["::stop-commands::token"]);
    }

    #[test]
    fn debug_logging_follows_github_s_variables() {
        assert!(debug_logging(|name| (name == "RUNNER_DEBUG").then(|| "1".to_owned())));
        assert!(debug_logging(|name| (name == "ACTIONS_STEP_DEBUG").then(|| "True".to_owned())));
        assert!(!debug_logging(|name| (name == "RUNNER_DEBUG").then(|| "0".to_owned())));
        assert!(!debug_logging(|_| None));
    }
}
