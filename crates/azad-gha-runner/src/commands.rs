//! Workflow commands that a step writes to its standard output (`::name key=value::message`), and masks.

use std::collections::BTreeMap;

const COMMAND_PREFIX: &str = "::";
const COMMAND_SEPARATOR: &str = "::";
const SET_OUTPUT: &str = "set-output";
const SAVE_STATE: &str = "save-state";
const ADD_MASK: &str = "add-mask";
const NAME_PROPERTY: &str = "name";
const MASK: &str = "***";

/// Escapes of a command message; `%25` is last so that `%250A` becomes `%0A`.
const MESSAGE_ESCAPES: [(&str, &str); 3] = [("%0D", "\r"), ("%0A", "\n"), ("%25", "%")];
/// Escapes of a command property.
const PROPERTY_ESCAPES: [(&str, &str); 5] = [("%0D", "\r"), ("%0A", "\n"), ("%3A", ":"), ("%2C", ","), ("%25", "%")];

/// A workflow command on a line of output: `::name key=value,key=value::message`, with its escapes decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowCommand {
    pub name: String,
    pub properties: BTreeMap<String, String>,
    pub message: String,
}

/// The workflow command on a line, if the line is one.
pub fn workflow_command(line: &str) -> Option<WorkflowCommand> {
    let body = line.trim_start().strip_prefix(COMMAND_PREFIX)?;
    let (header, message) = body.split_once(COMMAND_SEPARATOR)?;
    let (name, properties) = header.split_once(' ').unwrap_or((header, ""));
    if name.is_empty() {
        return None;
    }
    let properties = properties
        .split(',')
        .filter_map(|property| property.split_once('='))
        .map(|(key, value)| (key.trim().to_owned(), unescape(value, &PROPERTY_ESCAPES)))
        .collect();
    Some(WorkflowCommand { name: name.to_owned(), properties, message: unescape(message, &MESSAGE_ESCAPES) })
}

/// A workflow command that changes the state of the runner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    SetOutput { name: String, value: String },
    SaveState { name: String, value: String },
    AddMask(String),
}

/// The command on a line, when it is one that the runner acts on.
/// Other commands, such as `::error::`, are not the runner's business; they go to the log unchanged.
pub(crate) fn parse_command(line: &str) -> Option<Command> {
    let WorkflowCommand { name, properties, message } = workflow_command(line)?;
    let name_property = || properties.get(NAME_PROPERTY).filter(|name| !name.is_empty()).cloned();
    match name.as_str() {
        SET_OUTPUT => Some(Command::SetOutput { name: name_property()?, value: message }),
        SAVE_STATE => Some(Command::SaveState { name: name_property()?, value: message }),
        ADD_MASK => Some(Command::AddMask(message)),
        _ => None,
    }
}

fn unescape(text: &str, escapes: &[(&str, &str)]) -> String {
    escapes.iter().fold(text.to_string(), |result, (escaped, plain)| result.replace(escaped, plain))
}

/// The values that the log must not show.
#[derive(Clone, Debug, Default)]
pub(crate) struct Masker {
    secrets: Vec<String>,
}

impl Masker {
    /// Masks the value, and each of its lines, in all later output.
    pub(crate) fn add(&mut self, value: &str) {
        let parts = std::iter::once(value).chain(value.lines());
        for part in parts.map(str::trim).filter(|part| !part.is_empty()) {
            if !self.secrets.iter().any(|secret| secret == part) {
                self.secrets.push(part.to_string());
            }
        }
        self.secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
    }

    pub(crate) fn mask(&self, text: &str) -> String {
        self.secrets.iter().fold(text.to_string(), |result, secret| result.replace(secret.as_str(), MASK))
    }
}

/// What the commands on a step's standard output set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Captured {
    pub(crate) outputs: BTreeMap<String, String>,
    pub(crate) state: BTreeMap<String, String>,
}

/// Acts on one line of a step's standard output, and gives the line to show in the log, if any.
/// The lines of `set-output`, `save-state` and `add-mask` are not shown, as on GitHub.
pub(crate) fn process_line(line: &str, masker: &mut Masker, captured: &mut Captured) -> Option<String> {
    let Some(command) = parse_command(line) else {
        return Some(masker.mask(line));
    };
    match command {
        Command::SetOutput { name, value } => {
            captured.outputs.insert(name, value);
        }
        Command::SaveState { name, value } => {
            captured.state.insert(name, value);
        }
        Command::AddMask(value) => masker.add(&value),
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_set_output() {
        assert_eq!(
            parse_command("::set-output name=version::1.2.3"),
            Some(Command::SetOutput { name: "version".to_string(), value: "1.2.3".to_string() })
        );
    }

    #[test]
    fn unescapes_messages_and_properties() {
        assert_eq!(
            parse_command("::save-state name=a%3Ab::one%0Atwo%25"),
            Some(Command::SaveState { name: "a:b".to_string(), value: "one\ntwo%".to_string() })
        );
    }

    #[test]
    fn reads_any_command_with_its_properties() {
        let command = workflow_command("::error file=a.rs,line=3,title=x%3Ay::bad%0Anext 100%25").unwrap();
        assert_eq!(command.name, "error");
        assert_eq!(command.properties.get("file").map(String::as_str), Some("a.rs"));
        assert_eq!(command.properties.get("title").map(String::as_str), Some("x:y"));
        assert_eq!(command.message, "bad\nnext 100%");
        assert_eq!(workflow_command("plain :: text"), None);
    }

    #[test]
    fn reads_add_mask() {
        assert_eq!(parse_command("::add-mask::hunter2"), Some(Command::AddMask("hunter2".to_string())));
    }

    #[test]
    fn other_commands_and_text_are_not_runner_commands() {
        for line in [
            "::error file=a.rs,line=1::bad",
            "::warning::careful",
            "::group::Build",
            "::endgroup::",
            "plain text",
            "::set-output::no name",
        ] {
            assert_eq!(parse_command(line), None, "{line}");
        }
    }

    #[test]
    fn masks_later_output() {
        let mut masker = Masker::default();
        let mut captured = Captured::default();
        assert_eq!(process_line("before s3cret", &mut masker, &mut captured).as_deref(), Some("before s3cret"));
        assert_eq!(process_line("::add-mask::s3cret", &mut masker, &mut captured), None);
        assert_eq!(process_line("after s3cret!", &mut masker, &mut captured).as_deref(), Some("after ***!"));
    }

    #[test]
    fn masks_the_longest_value_first() {
        let mut masker = Masker::default();
        masker.add("abc");
        masker.add("abcdef");
        assert_eq!(masker.mask("x abcdef y abc"), "x *** y ***");
    }

    #[test]
    fn masks_each_line_of_a_multi_line_value() {
        let mut masker = Masker::default();
        masker.add("first\nsecond");
        assert_eq!(masker.mask("second line"), "*** line");
    }

    #[test]
    fn an_empty_mask_changes_nothing() {
        let mut masker = Masker::default();
        masker.add("   ");
        assert_eq!(masker.mask("text"), "text");
    }

    #[test]
    fn captures_outputs_and_state_and_hides_their_lines() {
        let mut masker = Masker::default();
        let mut captured = Captured::default();
        assert_eq!(process_line("::set-output name=a::1", &mut masker, &mut captured), None);
        assert_eq!(process_line("::save-state name=b::2", &mut masker, &mut captured), None);
        assert_eq!(process_line("::notice::kept", &mut masker, &mut captured).as_deref(), Some("::notice::kept"));
        assert_eq!(captured.outputs.get("a").map(String::as_str), Some("1"));
        assert_eq!(captured.state.get("b").map(String::as_str), Some("2"));
    }
}
