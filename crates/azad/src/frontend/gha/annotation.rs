//! Annotations: the `::error::`, `::warning::` and `::notice::` commands in an action's output, made again as commands
//! of the GitHub job that runs Azad, so that they show on the lines of a pull request.
//!
//! The output of an action can come from code that the job does not trust, so a line of it never goes to GitHub as it
//! is. Azad reads the command's level, message, title, file and positions, maps the file from where the action sees
//! the workspace to the path in the repository, and writes a new command from these values only, with GitHub's
//! escapes. Every other command, such as `::set-env::` or `::add-mask::`, is not an annotation and gives nothing here.

use azad_gha_runner::workflow_command;

use crate::frontend::WorkspaceView;
use crate::frontend::gha::display::Level;

const FILE_PROPERTY: &str = "file";
const TITLE_PROPERTY: &str = "title";
/// The properties that give a position in the file, in GitHub's order.
const POSITION_PROPERTIES: [&str; 4] = ["line", "endLine", "col", "endColumn"];

/// An annotation that an action's output asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Annotation {
    pub level: Level,
    /// The path in the workspace, when the action named a file in it.
    pub file: Option<String>,
    /// The positions in `file`, by property.
    pub positions: Vec<(&'static str, u64)>,
    pub title: Option<String>,
    pub message: String,
}

impl Annotation {
    /// The annotation that a line of output asks for, if it is an error, a warning or a notice. The file is read with
    /// `view`; a file outside the workspace, or of an action without a view, is dropped with its positions, so that
    /// the annotation is kept but points to no line.
    pub fn of_line(line: &str, view: Option<&WorkspaceView>) -> Option<Annotation> {
        let command = workflow_command(line)?;
        let level = Level::from_command(&command.name).filter(|level| *level != Level::Debug)?;
        let file = command.properties.get(FILE_PROPERTY).zip(view).and_then(|(file, view)| view.workspace_path(file));
        let positions = match file {
            Some(_) => positions(&command.properties),
            None => Vec::new(),
        };
        let title = command.properties.get(TITLE_PROPERTY).filter(|title| !title.is_empty()).cloned();
        Some(Annotation { level, file, positions, title, message: command.message })
    }

    /// The workflow command that makes this annotation, with GitHub's escapes.
    pub fn command(&self) -> String {
        let mut properties = Vec::new();
        if let Some(file) = &self.file {
            properties.push(format!("{FILE_PROPERTY}={}", escape_property(file)));
        }
        properties.extend(self.positions.iter().map(|(name, value)| format!("{name}={value}")));
        if let Some(title) = &self.title {
            properties.push(format!("{TITLE_PROPERTY}={}", escape_property(title)));
        }
        let properties = match properties.is_empty() {
            true => String::new(),
            false => format!(" {}", properties.join(",")),
        };
        format!("::{}{properties}::{}", self.level.command(), escape_data(&self.message))
    }
}

/// The position properties whose values are whole numbers.
fn positions(properties: &std::collections::BTreeMap<String, String>) -> Vec<(&'static str, u64)> {
    POSITION_PROPERTIES.iter().filter_map(|name| Some((*name, properties.get(*name)?.trim().parse().ok()?))).collect()
}

/// GitHub's escaping of a workflow command's message.
pub fn escape_data(text: &str) -> String {
    text.replace('%', "%25").replace('\r', "%0D").replace('\n', "%0A")
}

/// GitHub's escaping of a workflow command's property value.
pub fn escape_property(text: &str) -> String {
    escape_data(text).replace(':', "%3A").replace(',', "%2C")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn isolated(start: &str) -> WorkspaceView {
        WorkspaceView { root: "/work".to_owned(), start: start.to_owned() }
    }

    fn command(line: &str, view: Option<&WorkspaceView>) -> Option<String> {
        Annotation::of_line(line, view).map(|annotation| annotation.command())
    }

    #[test]
    fn an_annotation_gets_the_path_in_the_workspace_and_keeps_its_positions_and_title() {
        let line = "::error file=/work/crates/a.rs,line=3,endLine=4,col=7,endColumn=9,title=E0308::mismatched types";
        assert_eq!(
            command(line, Some(&isolated("crates"))).as_deref(),
            Some("::error file=crates/a.rs,line=3,endLine=4,col=7,endColumn=9,title=E0308::mismatched types")
        );
        assert_eq!(
            command("::warning file=src/main.go,line=12::unused", Some(&isolated("services/orders"))).as_deref(),
            Some("::warning file=services/orders/src/main.go,line=12::unused")
        );
        assert_eq!(command("::notice::fyi", None).as_deref(), Some("::notice::fyi"));
    }

    #[test]
    fn a_file_outside_the_workspace_is_dropped_with_its_positions() {
        let view = isolated("");
        assert_eq!(
            command("::error file=/usr/include/stdio.h,line=3,col=1::bad", Some(&view)).as_deref(),
            Some("::error::bad")
        );
        assert_eq!(command("::error file=../../etc/passwd,line=1::bad", Some(&view)).as_deref(), Some("::error::bad"));
        assert_eq!(command("::error file=a.rs,line=3::bad", None).as_deref(), Some("::error::bad"));
    }

    #[test]
    fn only_known_properties_with_valid_values_are_kept() {
        let line = "::error file=/work/a.rs,line=three,col=2,repo=other/repo,code=x::bad";
        assert_eq!(command(line, Some(&isolated(""))).as_deref(), Some("::error file=a.rs,col=2::bad"));
    }

    #[test]
    fn values_are_escaped_so_that_they_cannot_end_the_command() {
        let line = "::error file=/work/a%2Cb%3A.rs,title=t%3A%2C%0A::100%25 bad%0A::set-env name=X::1";
        assert_eq!(
            command(line, Some(&isolated(""))).as_deref(),
            Some("::error file=a%2Cb%3A.rs,title=t%3A%2C%0A::100%25 bad%0A::set-env name=X::1")
        );
    }

    #[test]
    fn commands_that_change_the_job_are_not_annotations() {
        for line in [
            "::set-env name=PATH::/evil",
            "::add-path::/evil",
            "::set-output name=x::y",
            "::save-state name=x::y",
            "::add-mask::secret",
            "::stop-commands::token",
            "::echo::on",
            "::add-matcher::/tmp/matcher.json",
            "::remove-matcher owner=rust::",
            "::debug::detail",
            "::group::title",
            "error: plain text",
        ] {
            assert_eq!(Annotation::of_line(line, Some(&isolated(""))), None, "{line}");
        }
    }
}
