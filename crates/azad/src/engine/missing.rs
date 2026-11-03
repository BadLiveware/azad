//! The packages whose files an isolated action's output names, though its sandbox did not hold them: the usual sign of
//! a dependency that its package does not declare (decision 0028). A tool says only that a file is missing; this says
//! which package to declare.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::engine::EngineError;
use crate::ir::{ActionDef, Graph, Kind};
use crate::scheduler::{Progress, Status};

/// Where the work directory is in the sandbox; an isolated action's files are at their workspace paths below it.
const SANDBOX_WORK: &str = "/work/";
/// The words of an output line that can hold a path end at these characters.
const WORD_ENDS: &[char] = &[' ', '\t', '\'', '"', '`', '(', ')', '[', ']', '{', '}', '<', '>', ',', ';', '='];
/// What a tool puts after a path: a line and column, such as `:12:5` or `(12,5)`, or a colon.
const POSITION: &[char] = &[':', '(', ')', ',', '.'];
/// How many words of one action's output are kept, so a long log does not grow without bound.
const MAX_WORDS: usize = 4096;

/// The path-like words of the output of the running isolated actions, by action.
#[derive(Default)]
pub(crate) struct Mentions {
    words: BTreeMap<String, BTreeSet<String>>,
}

impl Mentions {
    /// Keeps the path-like words of a line of an isolated action's output; host actions see the whole workspace.
    pub(crate) fn see(&mut self, graph: &Graph, progress: &Progress) {
        let Progress::Log { name, line, .. } = progress else { return };
        if graph.actions.get(name).is_none_or(|action| matches!(action.kind, Kind::Host { .. })) {
            return;
        }
        let words = self.words.entry(name.clone()).or_default();
        for word in path_words(line) {
            if words.len() >= MAX_WORDS {
                return;
            }
            words.insert(word);
        }
    }

    /// For an action that failed, the packages whose files its output named and its sandbox did not hold.
    pub(crate) fn missing_packages(
        &mut self,
        graph: &Graph,
        progress: &Progress,
        workspace: &dyn Fn() -> Result<Workspace, EngineError>,
    ) -> Result<BTreeSet<String>, EngineError> {
        let Progress::Done { name, status } = progress else { return Ok(BTreeSet::new()) };
        let words = self.words.remove(name).unwrap_or_default();
        let (Status::Failed { .. }, Some(action)) = (status, graph.actions.get(name)) else {
            return Ok(BTreeSet::new());
        };
        let paths: BTreeSet<String> = words.iter().flat_map(|word| candidates(word, &action.dir)).collect();
        let outside: Vec<&String> = paths.iter().filter(|path| !held_by(action, path)).collect();
        if outside.is_empty() {
            return Ok(BTreeSet::new());
        }
        let workspace = workspace()?;
        Ok(outside
            .into_iter()
            .filter(|path| workspace.files.contains(Path::new(path.as_str())))
            .filter_map(|path| workspace.package_of(path))
            .collect())
    }
}

/// The files of the workspace that git tracks or would track, and its packages, by directory (`""` for the root).
pub(crate) struct Workspace {
    pub(crate) files: BTreeSet<PathBuf>,
    pub(crate) packages: Vec<String>,
}

impl Workspace {
    /// The label of the nearest package at or above `path`.
    fn package_of(&self, path: &str) -> Option<String> {
        self.packages
            .iter()
            .filter(|dir| is_within(path, dir))
            .max_by_key(|dir| dir.len())
            .map(|dir| format!("//{dir}"))
    }
}

/// Whether one of the action's input directories holds `path`.
fn held_by(action: &ActionDef, path: &str) -> bool {
    action.work.keys().chain(action.inputs.keys()).any(|dir| is_within(path, dir))
}

fn is_within(path: &str, dir: &str) -> bool {
    dir.is_empty() || path == dir || path.strip_prefix(dir).is_some_and(|rest| rest.starts_with('/'))
}

/// The words of `line` that look like paths: they hold a `/`, without what a tool puts around a path.
fn path_words(line: &str) -> impl Iterator<Item = String> + '_ {
    line.split(WORD_ENDS)
        .map(|word| word.trim_end_matches(|c: char| c.is_ascii_digit() || POSITION.contains(&c)))
        .filter(|word| word.contains('/') && !word.contains("://"))
        .map(str::to_owned)
}

/// The workspace paths that `word` can name: an absolute path below the sandbox's work directory, or a relative one
/// against the action's directory and each directory above it, because tools print paths relative to directories of
/// their own, such as a solution's. Only a candidate that names a file of the workspace counts.
fn candidates(word: &str, dir: &str) -> Vec<String> {
    let mut dirs = vec![dir];
    let mut above = dir;
    while let Some((parent, _)) = above.rsplit_once('/') {
        dirs.push(parent);
        above = parent;
    }
    if !dir.is_empty() {
        dirs.push("");
    }
    dirs.into_iter().filter_map(|dir| workspace_path(word, dir)).collect()
}

/// The workspace path that `word` names in an action's sandbox: below the work directory when it is absolute, or
/// relative to the action's directory, with `.` and `..` resolved; None when it leaves the workspace.
fn workspace_path(word: &str, dir: &str) -> Option<String> {
    let relative = match word.strip_prefix('/') {
        Some(_) => word.strip_prefix(SANDBOX_WORK)?.to_owned(),
        None => match dir.is_empty() {
            true => word.to_owned(),
            false => format!("{dir}/{word}"),
        },
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in relative.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            name => parts.push(name),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_s_message_gives_the_paths_that_it_names() {
        let line = "CSC : error CS2001: Source file 'app/tests/../../lib/src/Id.cs' could not be found.";
        assert_eq!(path_words(line).collect::<Vec<_>>(), ["app/tests/../../lib/src/Id.cs"]);
        let line = "src/main.rs:12:5: error: /work/lib/src/a.txt(3,1) see https://example.com/x";
        assert_eq!(path_words(line).collect::<Vec<_>>(), ["src/main.rs", "/work/lib/src/a.txt"]);
    }

    #[test]
    fn a_relative_path_is_tried_against_each_directory_above_the_action() {
        let found = candidates("core/Tests/../../legacy/src/Id.cs", "backends/core");
        assert!(found.contains(&"backends/legacy/src/Id.cs".to_owned()), "{found:?}");
        assert_eq!(candidates("/work/lib/a.txt", "app/x"), ["lib/a.txt"; 3]);
    }

    #[test]
    fn a_path_resolves_against_the_sandbox_or_the_action_s_directory() {
        assert_eq!(workspace_path("/work/lib/a.txt", "app").as_deref(), Some("lib/a.txt"));
        assert_eq!(workspace_path("../lib/./a.txt", "app").as_deref(), Some("lib/a.txt"));
        assert_eq!(workspace_path("tests/../../lib/a.txt", "app").as_deref(), Some("lib/a.txt"));
        assert_eq!(workspace_path("../../outside", "app"), None);
        assert_eq!(workspace_path("/usr/lib/libc.so", "app"), None);
    }
}
