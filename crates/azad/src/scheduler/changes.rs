//! Why the cache had no result for an action: how the action differs from the same action in an earlier run of its
//! goal. `azad why` shows it, so that "it ran" comes with the files and values that changed.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::ir::action::{Container, ContainerImage};
use crate::ir::{Action, Digest, Kind};
use crate::store::{ChangedPath, PathChange, Store, StoreError};

/// The most changed paths that an explanation lists; it counts the others.
const SHOWN_PATHS: usize = 10;
/// The most characters of a value that an explanation shows.
const SHOWN_VALUE_CHARS: usize = 80;
const ELLIPSIS: &str = "...";
const INDENT: &str = "  ";
const SEPARATOR: &str = "/";
const WORK_DIR_SHOWN: &str = ".";
const ENV: &str = "env";
const CACHE_VOLUME: &str = "cache volume";
const COMMAND: &str = "command";
const KIND: &str = "kind";
const WORKSPACE: &str = "workspace";
const CONTAINER: &str = "container";
const ON_THE_HOST: &str = "the host's tools";
const PLATFORM: &str = "platform";
const DIRECTORY: &str = "directory";

/// Why the cache had no result for an action, from the newest earlier run of the goal that has the action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cause {
    /// No earlier run of the goal has the action.
    FirstRun,
    /// The action changed since an earlier run.
    Changed { run: u64, changes: Changes },
    /// The action is the same as in an earlier run, in which it failed; the cache keeps no failure.
    FailedBefore { run: u64 },
    /// The action is the same as in an earlier run, in which it succeeded. A host action runs again when the
    /// workspace no longer holds the outputs that it made; `host_outputs` names them.
    SameAsBefore { run: u64, host_outputs: Vec<String> },
    /// The action changed since an earlier run, but the store does not have both actions to compare them.
    NotStored { run: u64 },
}

/// How an action differs from an earlier one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    /// The files that differ, by their path relative to the work directory (for a task, the workspace), or below the
    /// path of an input.
    pub paths: Vec<ChangedPath>,
    pub parts: Vec<Part>,
}

/// A part of an action, other than its files, that differs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Part {
    /// A named value, such as an environment variable; its values are as they are shown.
    Value {
        name: String,
        change: ValueChange,
    },
    Output {
        path: String,
        change: PathChange,
    },
    /// The tree of the root file system, which is the image.
    Root,
    /// The digests that the outputs of a fixed-output action must have.
    Expected,
    /// The files of a tree changed, but the store does not have both trees to compare them.
    UnknownFiles {
        path: String,
    },
    /// No part differs: the two actions are encoded in different ways, for example by two versions of Azad.
    Encoding,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValueChange {
    Added(String),
    Removed(String),
    Changed { old: String, new: String },
}

/// Whether an explanation is about a run that happened or about the next run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tense {
    Past,
    Next,
}

impl Changes {
    /// How `new` differs from `old`. The trees of both must be in the store; a tree that is missing is reported as
    /// files that are not known.
    pub fn between(store: &Store, old: &Action, new: &Action) -> Result<Changes, StoreError> {
        let mut changes = Changes::default();
        changes.compare_trees(store, &old.work, &new.work)?;
        changes.compare_trees(store, &old.inputs, &new.inputs)?;
        changes.compare_values(ENV, &old.env, &new.env);
        changes.compare_values(CACHE_VOLUME, &old.caches, &new.caches);
        changes.compare(COMMAND, shell_words(&old.command), shell_words(&new.command));
        changes.compare_kinds(&old.kind, &new.kind);
        changes.compare(PLATFORM, old.platform.clone(), new.platform.clone());
        changes.compare(DIRECTORY, work_dir_path(&old.dir), work_dir_path(&new.dir));
        if old.root != new.root {
            changes.parts.push(Part::Root);
        }
        for path in old.outputs.difference(&new.outputs) {
            changes.parts.push(Part::Output { path: path.clone(), change: PathChange::Removed });
        }
        for path in new.outputs.difference(&old.outputs) {
            changes.parts.push(Part::Output { path: path.clone(), change: PathChange::Added });
        }
        if changes.paths.is_empty() && changes.parts.is_empty() {
            changes.parts.push(Part::Encoding);
        }
        Ok(changes)
    }

    fn compare_trees(
        &mut self,
        store: &Store,
        old: &BTreeMap<String, Digest>,
        new: &BTreeMap<String, Digest>,
    ) -> Result<(), StoreError> {
        let places: BTreeSet<&String> = old.keys().chain(new.keys()).collect();
        for place in places {
            let whole = format!("{}{SEPARATOR}", place_path(place));
            match (old.get(place), new.get(place)) {
                (Some(a), Some(b)) => self.compare_tree(store, place, a, b)?,
                (Some(_), None) => self.paths.push(ChangedPath { path: whole, change: PathChange::Removed }),
                (None, Some(_)) => self.paths.push(ChangedPath { path: whole, change: PathChange::Added }),
                (None, None) => unreachable!("each place comes from one of the two maps"),
            }
        }
        Ok(())
    }

    fn compare_tree(&mut self, store: &Store, place: &str, old: &Digest, new: &Digest) -> Result<(), StoreError> {
        let changed = match store.compare_trees(old, new) {
            Ok(changed) => changed,
            Err(StoreError::Missing(_)) => {
                self.parts.push(Part::UnknownFiles { path: place_path(place) });
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        let below = |path: String| match place.is_empty() {
            true => path,
            false => format!("{}{SEPARATOR}{path}", place.trim_end_matches(SEPARATOR)),
        };
        self.paths.extend(changed.into_iter().map(|c| ChangedPath { path: below(c.path), change: c.change }));
        Ok(())
    }

    fn compare_values(&mut self, what: &str, old: &BTreeMap<String, String>, new: &BTreeMap<String, String>) {
        let names: BTreeSet<&String> = old.keys().chain(new.keys()).collect();
        for name in names {
            let change = match (old.get(name), new.get(name)) {
                (Some(a), Some(b)) if a == b => continue,
                (Some(a), Some(b)) => ValueChange::Changed { old: quoted(a), new: quoted(b) },
                (Some(a), None) => ValueChange::Removed(quoted(a)),
                (None, Some(b)) => ValueChange::Added(quoted(b)),
                (None, None) => unreachable!("each name comes from one of the two maps"),
            };
            self.parts.push(Part::Value { name: format!("{what} {name}"), change });
        }
    }

    fn compare_kinds(&mut self, old: &Kind, new: &Kind) {
        match (old, new) {
            (Kind::Host { workspace: a, container: x }, Kind::Host { workspace: b, container: y }) => {
                self.compare(WORKSPACE, a.clone(), b.clone());
                self.compare(CONTAINER, container_name(x.as_ref()), container_name(y.as_ref()));
            }
            (Kind::FixedOutput { expected: a }, Kind::FixedOutput { expected: b }) if a != b => {
                self.parts.push(Part::Expected);
            }
            _ => self.compare(KIND, old.name().to_owned(), new.name().to_owned()),
        }
    }

    fn compare(&mut self, name: &str, old: String, new: String) {
        if old != new {
            self.parts.push(Part::Value { name: name.to_owned(), change: ValueChange::Changed { old, new } });
        }
    }

    /// One line for each change, with at most `SHOWN_PATHS` paths.
    fn lines(&self) -> Vec<String> {
        let mut lines: Vec<String> =
            self.paths.iter().take(SHOWN_PATHS).map(|p| format!("{} {}", p.path, change_words(p.change))).collect();
        if self.paths.len() > SHOWN_PATHS {
            lines.push(format!("and {} more paths", self.paths.len() - SHOWN_PATHS));
        }
        lines.extend(self.parts.iter().map(Part::to_string));
        lines
    }
}

impl fmt::Display for Part {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Part::Value { name, change: ValueChange::Changed { old, new } } => write!(f, "{name}: {old} -> {new}"),
            Part::Value { name, change: ValueChange::Added(new) } => write!(f, "{name} was added: {new}"),
            Part::Value { name, change: ValueChange::Removed(old) } => write!(f, "{name} was removed (it was {old})"),
            Part::Output { path, change } => write!(f, "output {path} {}", change_words(*change)),
            Part::Root => write!(f, "the image changed"),
            Part::Expected => write!(f, "the expected digests of the outputs changed"),
            Part::UnknownFiles { path } => {
                write!(f, "the files of {path} changed; the store does not have both trees, so it cannot name them")
            }
            Part::Encoding => write!(
                f,
                "no part changed: the actions are encoded in different ways, for example by two versions of Azad"
            ),
        }
    }
}

/// Writes why an action ran, or must run, after "It ran" or "It must run".
pub(crate) fn write_cause(
    f: &mut fmt::Formatter<'_>,
    tense: Tense,
    cause: &Cause,
    action: &Digest,
    goal: &str,
) -> fmt::Result {
    let (lead, cache) = match tense {
        Tense::Past => ("It ran", "the cache had no result"),
        Tense::Next => ("It must run", "the cache has no result"),
    };
    match cause {
        Cause::FirstRun => {
            write!(f, "{lead}, because {cache} for action {action}; no earlier run of {goal} has this action.")
        }
        Cause::Changed { run, changes } => {
            write!(f, "{lead}, because it changed since run {run}:")?;
            for line in changes.lines() {
                write!(f, "\n{INDENT}{line}")?;
            }
            Ok(())
        }
        Cause::FailedBefore { run } => {
            write!(f, "{lead}, because it failed in run {run} with the same inputs, and the cache keeps no failure.")
        }
        Cause::SameAsBefore { run, host_outputs } if !host_outputs.is_empty() => write!(
            f,
            "{lead}, because its outputs in the workspace ({}) changed or were removed after run {run}; its inputs are the same.",
            host_outputs.join(", ")
        ),
        Cause::SameAsBefore { run, .. } => {
            write!(f, "{lead}, because the cache no longer has its result from run {run}; its inputs are the same.")
        }
        Cause::NotStored { run } => write!(
            f,
            "{lead}, because it changed since run {run}; the store does not have both actions, so it cannot say what changed."
        ),
    }
}

fn change_words(change: PathChange) -> &'static str {
    match change {
        PathChange::Added => "was added",
        PathChange::Removed => "was removed",
        PathChange::Changed => "changed",
    }
}

/// The path of a tree as it is shown: the work directory itself is `.`.
fn place_path(place: &str) -> String {
    match place.is_empty() {
        true => WORK_DIR_SHOWN.to_owned(),
        false => place.trim_end_matches(SEPARATOR).to_owned(),
    }
}

fn work_dir_path(dir: &str) -> String {
    match dir.is_empty() {
        true => WORK_DIR_SHOWN.to_owned(),
        false => dir.to_owned(),
    }
}

/// A value in quotes, shortened when it is long.
fn quoted(value: &str) -> String {
    format!("{:?}", shortened(value))
}

/// A command as a shell shows it: a word with a space, a quote or nothing in it is in quotes.
fn shell_words(command: &[String]) -> String {
    let words: Vec<String> = command
        .iter()
        .map(|word| {
            let plain = !word.is_empty() && !word.contains(|c: char| c.is_whitespace() || c == '"' || c == '\'');
            match plain {
                true => word.clone(),
                false => format!("{word:?}"),
            }
        })
        .collect();
    shortened(&words.join(" "))
}

fn shortened(value: &str) -> String {
    if value.chars().count() <= SHOWN_VALUE_CHARS {
        return value.to_owned();
    }
    let kept: String = value.chars().take(SHOWN_VALUE_CHARS).collect();
    format!("{kept}{ELLIPSIS}")
}

/// Where a host action runs, for an explanation: the host's tools, or the image of its container.
pub(crate) fn container_name(container: Option<&Container>) -> String {
    match container.map(|container| &container.image) {
        None => ON_THE_HOST.to_owned(),
        Some(ContainerImage::Reference(reference)) => reference.clone(),
        Some(ContainerImage::Dockerfile { file, target, .. }) if target.is_empty() => file.clone(),
        Some(ContainerImage::Dockerfile { file, target, .. }) => format!("{file} ({target})"),
    }
}
