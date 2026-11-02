//! A graph of named actions and named goals, and the verifier that checks it before anything runs.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::ir::action::{
    ActionDef, Kind, Source, WORK_DIR, source_from_value, source_to_value, spec_fields, spec_from_fields,
};
use crate::ir::canonical::{Canonical, CanonicalError, Fields, text_list};
use crate::ir::cbor::Value;

const KEY_ACTIONS: &str = "a";
const KEY_GOALS: &str = "g";

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Graph {
    pub actions: BTreeMap<String, ActionDef>,
    /// Each goal names the actions that must succeed for it to hold.
    pub goals: BTreeMap<String, BTreeSet<String>>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct VerifyError {
    pub subject: String,
    pub message: String,
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.subject, self.message)
    }
}

impl Graph {
    /// Checks the graph, and returns every problem that it finds.
    pub fn verify(&self) -> Result<(), Vec<VerifyError>> {
        let mut errors = Vec::new();
        for (name, action) in &self.actions {
            let mut report = |message: String| errors.push(VerifyError { subject: format!("action {name}"), message });
            if !is_valid_name(name) {
                report("the name may hold only letters, digits, '_', '-', '.', '/' and ':'".to_owned());
            }
            self.verify_action(action, &mut report);
        }
        for (goal, actions) in &self.goals {
            let mut report = |message: String| errors.push(VerifyError { subject: format!("goal {goal}"), message });
            if !is_valid_name(goal) {
                report("the name may hold only letters, digits, '_', '-', '.', '/' and ':'".to_owned());
            }
            if actions.is_empty() {
                report("a goal must name at least one action".to_owned());
            }
            for action in actions.iter().filter(|a| !self.actions.contains_key(*a)) {
                report(format!("there is no action {action}"));
            }
        }
        if let Err(cycle) = self.topological_order() {
            errors
                .push(VerifyError { subject: format!("action {cycle}"), message: "it is part of a cycle".to_owned() });
        }
        match errors.is_empty() {
            true => Ok(()),
            false => Err(errors),
        }
    }

    fn verify_action(&self, action: &ActionDef, report: &mut impl FnMut(String)) {
        if action.command.is_empty() {
            report("the command is empty".to_owned());
        }
        if action.platform.is_empty() {
            report("the platform is empty".to_owned());
        }
        for key in action.env.keys().filter(|k| k.is_empty() || k.contains(['=', '\0'])) {
            report(format!("the environment variable name {key:?} is not valid"));
        }
        for path in action.inputs.keys() {
            if !is_absolute_normal(path) || path == "/" {
                report(format!("input path {path:?} must be absolute, normalized and not \"/\""));
            }
            if overlaps(path, WORK_DIR) {
                report(format!("input path {path:?} overlaps the work directory {WORK_DIR}"));
            }
        }
        report_overlaps(action.inputs.keys(), "input", report);
        let is_host = matches!(action.kind, Kind::Host { .. });
        for (path, name) in &action.caches {
            match is_host {
                true if !is_relative_normal(path) => {
                    report(format!("cache path {path:?} of a host action must be relative to its workspace"));
                }
                false if !is_absolute_normal(path) || path == "/" || path == WORK_DIR => {
                    report(format!("cache path {path:?} must be absolute, normalized, and not \"/\" or {WORK_DIR}"));
                }
                _ => {}
            }
            let below_work = |relative: &String| format!("{WORK_DIR}/{relative}");
            if !is_host && action.outputs.iter().map(below_work).any(|output| overlaps(&output, path)) {
                report(format!("cache path {path:?} overlaps an output"));
            }
            if action.inputs.keys().any(|input| overlaps(input, path)) {
                report(format!("cache path {path:?} overlaps an input"));
            }
            if name.contains(['/', ':']) || !is_valid_name(name) {
                report(format!("cache name {name:?} may hold only letters, digits, '_', '-' and '.'"));
            }
        }
        report_overlaps(action.caches.keys(), "cache", report);
        for path in &action.outputs {
            if !is_relative_normal(path) {
                report(format!("output path {path:?} must be relative to {WORK_DIR} and normalized"));
            }
        }
        report_overlaps(action.outputs.iter(), "output", report);
        if let Kind::FixedOutput { expected } = &action.kind {
            let expected: BTreeSet<&String> = expected.keys().collect();
            let declared: BTreeSet<&String> = action.outputs.iter().collect();
            if expected != declared {
                report("a fixed-output action must expect a digest for exactly its declared outputs".to_owned());
            }
        }
        for path in action.work.keys().filter(|path| !path.is_empty() && !is_relative_normal(path)) {
            report(format!("work path {path:?} must be relative to {WORK_DIR} and normalized"));
        }
        if !action.dir.is_empty() && !is_relative_normal(&action.dir) {
            report(format!("dir {:?} must be relative to {WORK_DIR} and normalized", action.dir));
        }
        if let Kind::Host { workspace, .. } = &action.kind {
            if !is_absolute_normal(workspace) {
                report(format!("the workspace {workspace:?} of a host action must be absolute and normalized"));
            }
            if !action.inputs.is_empty() {
                report("a host action reads the files of its workspace, so it has no inputs".to_owned());
            }
        }
        for source in std::iter::once(&action.root).chain(action.inputs.values()).chain(action.work.values()) {
            self.verify_source(source, report);
        }
    }

    fn verify_source(&self, source: &Source, report: &mut impl FnMut(String)) {
        let Source::Output { action, path } = source else {
            return;
        };
        match self.actions.get(action) {
            None => report(format!("it reads an output of {action}, which does not exist")),
            Some(producer) if !producer.outputs.contains(path) => {
                report(format!("it reads output {path:?} of {action}, which does not declare it"));
            }
            Some(_) => {}
        }
    }

    /// The actions in an order in which each action comes after the actions that it depends on, or the name of an
    /// action in a cycle.
    pub fn topological_order(&self) -> Result<Vec<&str>, String> {
        let mut remaining: BTreeMap<&str, BTreeSet<&str>> = self
            .actions
            .iter()
            .map(|(name, action)| {
                let known = action.dependencies().into_iter().filter(|d| self.actions.contains_key(*d)).collect();
                (name.as_str(), known)
            })
            .collect();
        let mut order = Vec::with_capacity(remaining.len());
        while !remaining.is_empty() {
            let ready: Vec<&str> = remaining.iter().filter(|(_, deps)| deps.is_empty()).map(|(n, _)| *n).collect();
            if ready.is_empty() {
                let stuck = remaining.keys().next().expect("the map is not empty");
                return Err((*stuck).to_owned());
            }
            for name in &ready {
                remaining.remove(name);
            }
            for deps in remaining.values_mut() {
                deps.retain(|d| !ready.contains(d));
            }
            order.extend(ready);
        }
        Ok(order)
    }
}

impl Canonical for Graph {
    const TYPE: &'static str = "graph";

    fn fields(&self) -> BTreeMap<String, Value> {
        let actions = self
            .actions
            .iter()
            .map(|(name, action)| (name.clone(), Value::Map(spec_fields(action, source_to_value))))
            .collect();
        let goals = self
            .goals
            .iter()
            .map(|(name, actions)| (name.clone(), Value::Array(actions.iter().cloned().map(Value::Text).collect())))
            .collect();
        BTreeMap::from([(KEY_ACTIONS.to_owned(), Value::Map(actions)), (KEY_GOALS.to_owned(), Value::Map(goals))])
    }

    fn from_fields(fields: &mut Fields) -> Result<Self, CanonicalError> {
        let mut actions = BTreeMap::new();
        for (name, value) in fields.map(KEY_ACTIONS)? {
            let Value::Map(map) = value else {
                return Err(CanonicalError::Field(KEY_ACTIONS));
            };
            let mut action_fields = Fields::new(map);
            let action = spec_from_fields(&mut action_fields, source_from_value)?;
            action_fields.finish()?;
            actions.insert(name, action);
        }
        let mut goals = BTreeMap::new();
        for (name, value) in fields.map(KEY_GOALS)? {
            let Value::Array(items) = value else {
                return Err(CanonicalError::Field(KEY_GOALS));
            };
            goals.insert(name, text_list(items, KEY_GOALS)?.into_iter().collect());
        }
        Ok(Graph { actions, goals })
    }
}

/// A name is a word, or a label such as `//services/orders:test`.
pub fn is_valid_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'/' | b':'))
}

fn is_relative_normal(path: &str) -> bool {
    !path.is_empty() && path.split('/').all(|part| !part.is_empty() && part != "." && part != "..")
}

fn is_absolute_normal(path: &str) -> bool {
    path == "/" || path.strip_prefix('/').is_some_and(is_relative_normal)
}

fn overlaps(a: &str, b: &str) -> bool {
    let within = |inner: &str, outer: &str| inner == outer || inner.starts_with(&format!("{outer}/"));
    within(a, b) || within(b, a)
}

fn report_overlaps<'a>(paths: impl Iterator<Item = &'a String>, what: &str, report: &mut impl FnMut(String)) {
    let paths: Vec<&String> = paths.collect();
    for (i, a) in paths.iter().enumerate() {
        for b in paths.iter().skip(i + 1).filter(|b| overlaps(a, b)) {
            report(format!("{what} paths {a:?} and {b:?} overlap"));
        }
    }
}
