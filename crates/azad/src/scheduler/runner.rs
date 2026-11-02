//! The run loop: derive what must run, start executions through `decide`, turn their outcomes into facts, and derive
//! again, until nothing more can run.

use crate::log::{LogSink, Stream};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::sync::mpsc;
use std::thread;

use crate::ir::{Action, ActionResult, Canonical, Digest, Graph};

use crate::scheduler::backend::{Backend, BackendError, BackendExecution};
use crate::scheduler::changes::{Cause, Tense, write_cause};
use crate::scheduler::decide::{Effect, Event, State, decide};
use crate::scheduler::rules::{self, Facts, Id};

/// What happened to one action in a run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    NotNeeded,
    /// `discarded` names the paths that the action left in its work directory without declaring them as outputs.
    Ran {
        action: Digest,
        result: ActionResult,
        discarded: Vec<String>,
    },
    Cached {
        action: Digest,
        result: ActionResult,
    },
    Failed {
        action: Digest,
        reason: String,
        result: Option<ActionResult>,
    },
    Skipped,
    /// Only in a plan: the action must run, because the cache has no result for it.
    WillRun {
        action: Digest,
    },
    /// Only in a plan: the action waits for outputs that do not exist yet.
    Waiting,
}

impl Status {
    /// The digest of the action, when it was resolved.
    pub fn action(&self) -> Option<&Digest> {
        match self {
            Status::Ran { action, .. }
            | Status::Cached { action, .. }
            | Status::Failed { action, .. }
            | Status::WillRun { action } => Some(action),
            Status::NotNeeded | Status::Skipped | Status::Waiting => None,
        }
    }
}

/// What the run loop reports while it works.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Progress {
    Started {
        name: String,
        action: Digest,
    },
    /// A line of a running action's output.
    Log {
        name: String,
        stream: Stream,
        line: String,
    },
    Done {
        name: String,
        status: Status,
    },
}

/// The outcome of a run or a plan: the status of each action, and the facts that explain it.
#[derive(Clone, Debug)]
pub struct Outcome {
    pub goal: String,
    pub statuses: BTreeMap<String, Status>,
    pub(crate) names: Vec<String>,
    pub(crate) facts: Facts,
    /// The actions that are never cached (impure), so that an explanation does not blame the cache.
    pub(crate) never_cached: BTreeSet<String>,
}

impl Outcome {
    pub(crate) fn from_parts(
        goal: String,
        statuses: BTreeMap<String, Status>,
        names: Vec<String>,
        facts: Facts,
        never_cached: BTreeSet<String>,
    ) -> Self {
        Outcome { goal, statuses, names, facts, never_cached }
    }

    /// The goal holds when each of its actions succeeded.
    pub fn goal_holds(&self) -> bool {
        self.facts.requested.iter().all(|id| self.facts.succeeded.contains(id))
    }

    /// Why an action ran, did not run, or was skipped: a derivation from the facts.
    pub fn explain(&self, name: &str) -> Option<Explanation> {
        let id = self.names.iter().position(|n| n == name)? as Id;
        let label = |ids: Vec<Id>| ids.into_iter().map(|i| self.names[i as usize].clone()).collect::<Vec<_>>();
        Some(Explanation {
            goal: self.goal.clone(),
            action: name.to_owned(),
            needed_by: rules::needed_chain(&self.facts, id).map(label),
            failed_chain: rules::failure_chain(&self.facts, id).map(label).filter(|c| c.len() > 1),
            waiting_on: label(rules::waiting_on(&self.facts, id)),
            status: self.statuses[name].clone(),
            never_cached: self.never_cached.contains(name),
            cause: None,
        })
    }
}

#[derive(Clone, Debug)]
pub struct Explanation {
    pub goal: String,
    pub action: String,
    pub needed_by: Option<Vec<String>>,
    pub failed_chain: Option<Vec<String>>,
    pub waiting_on: Vec<String>,
    pub status: Status,
    /// The action is impure, so it runs every time.
    pub never_cached: bool,
    /// Why the cache had no result, when the action ran or must run and it is known; see `Cause`.
    pub cause: Option<Cause>,
}

impl fmt::Display for Explanation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Some(chain) = &self.needed_by else {
            return write!(f, "{} is not needed: goal {} does not depend on it.", self.action, self.goal);
        };
        write!(f, "{} is needed: goal {} requires {}", self.action, self.goal, chain[0])?;
        for pair in chain.windows(2) {
            write!(f, ", {} reads an output of {}", pair[0], pair[1])?;
        }
        writeln!(f, ".")?;
        match &self.status {
            Status::NotNeeded => write!(f, "It was not run."),
            Status::Ran { action, result, .. } => match (self.never_cached, &self.cause) {
                (true, _) => write!(f, "It ran: it is impure, so it runs every time; it exited {}.", result.exit_code),
                (false, Some(cause)) => {
                    write_cause(f, Tense::Past, cause, action, &self.goal)?;
                    write!(f, "\nIt exited {}.", result.exit_code)
                }
                (false, None) => write!(
                    f,
                    "It ran, because the cache had no result for action {action}; it exited {}.",
                    result.exit_code
                ),
            },
            Status::Cached { action, .. } => {
                write!(f, "It did not run: the cache has the result of action {action}, which has the same inputs.")
            }
            Status::Failed { reason, .. } => write!(f, "It failed: {reason}."),
            Status::Skipped => match &self.failed_chain {
                Some(chain) => write!(f, "It was skipped: it depends on {}, which failed.", chain.join(" -> ")),
                None => write!(f, "It was skipped."),
            },
            Status::WillRun { action } => match &self.cause {
                Some(cause) => write_cause(f, Tense::Next, cause, action, &self.goal),
                None => write!(f, "It must run: the cache has no result for action {action}."),
            },
            Status::Waiting => {
                write!(
                    f,
                    "It must run after {}, which must run first; its inputs are not known yet.",
                    self.waiting_on.join(", ")
                )
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("there is no goal {0}")]
    UnknownGoal(String),
    #[error("the backend failed: {0}")]
    Backend(BackendError),
}

struct Execution {
    state: State,
    action: Action,
    attached: Vec<Id>,
    discarded: Vec<String>,
}

pub struct Runner<'a> {
    graph: &'a Graph,
    backend: &'a dyn Backend,
    parallelism: usize,
}

impl fmt::Debug for Runner<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Runner").field("parallelism", &self.parallelism).finish_non_exhaustive()
    }
}

/// The state of one run or plan.
struct Run<'a> {
    graph: &'a Graph,
    names: Vec<String>,
    facts: Facts,
    outputs: HashMap<(String, String), Digest>,
    executions: Vec<Execution>,
    by_digest: HashMap<Digest, usize>,
    statuses: BTreeMap<String, Status>,
}

impl<'a> Run<'a> {
    fn new(graph: &'a Graph, goal: &str) -> Result<Self, RunError> {
        let requested = graph.goals.get(goal).ok_or_else(|| RunError::UnknownGoal(goal.to_owned()))?;
        let names: Vec<String> = graph.actions.keys().cloned().collect();
        let id = |name: &str| names.iter().position(|n| n == name).expect("the graph is verified") as Id;
        let depends_on = graph
            .actions
            .iter()
            .flat_map(|(name, def)| def.dependencies().into_iter().map(move |d| (name.as_str(), d)))
            .map(|(a, d)| (id(a), id(d)))
            .collect();
        let facts = Facts { requested: requested.iter().map(|a| id(a)).collect(), depends_on, ..Facts::default() };
        Ok(Run {
            graph,
            names,
            facts,
            outputs: HashMap::new(),
            executions: Vec::new(),
            by_digest: HashMap::new(),
            statuses: BTreeMap::new(),
        })
    }

    fn resolve(&self, id: Id) -> Action {
        let def = &self.graph.actions[&self.names[id as usize]];
        def.resolve(|action, path| self.outputs.get(&(action.to_owned(), path.to_owned())).copied())
            .expect("the rules start an action only when its dependencies succeeded")
    }

    /// Starts an action: joins an execution with the same digest, or makes a new one. Returns the execution.
    fn start(&mut self, id: Id) -> (usize, bool) {
        self.facts.started.insert(id);
        let action = self.resolve(id);
        let digest = action.digest();
        if let Some(&index) = self.by_digest.get(&digest) {
            self.executions[index].attached.push(id);
            return (index, false);
        }
        let index = self.executions.len();
        self.executions.push(Execution { state: State::Queued, action, attached: vec![id], discarded: Vec::new() });
        self.by_digest.insert(digest, index);
        (index, true)
    }

    fn apply(&mut self, index: usize, event: Event) -> Vec<Effect> {
        let (next, effects) = decide(&self.executions[index].state, event).expect("the run loop sends valid events");
        self.executions[index].state = next;
        if effects.contains(&Effect::Publish) {
            self.publish(index);
        }
        effects
    }

    /// Turns the terminal state of an execution into facts for every action that shares it.
    fn publish(&mut self, index: usize) {
        let execution = &self.executions[index];
        let digest = execution.action.digest();
        for &id in &execution.attached {
            let name = self.names[id as usize].clone();
            let status = match &execution.state {
                State::Succeeded { result, cached: true } => Status::Cached { action: digest, result: result.clone() },
                State::Succeeded { result, cached: false } => {
                    Status::Ran { action: digest, result: result.clone(), discarded: execution.discarded.clone() }
                }
                State::Failed { reason, result } => {
                    Status::Failed { action: digest, reason: reason.clone(), result: result.clone() }
                }
                _ => continue,
            };
            match &status {
                Status::Cached { result, .. } | Status::Ran { result, .. } => {
                    self.facts.succeeded.insert(id);
                    for (path, tree) in &result.outputs {
                        self.outputs.insert((name.clone(), path.clone()), *tree);
                    }
                }
                _ => {
                    self.facts.failed.insert(id);
                }
            }
            self.statuses.insert(name, status);
        }
    }

    /// A late joiner of an execution that is already done gets its outcome at once.
    fn publish_if_done(&mut self, index: usize) {
        if self.executions[index].state.is_terminal() {
            self.publish(index);
        }
    }

    fn finish(mut self, goal: &str) -> Outcome {
        let derived = rules::derive(&self.facts);
        for (id, name) in self.names.iter().enumerate() {
            if let Some(status) = status_without_execution(&derived, id as Id) {
                self.statuses.entry(name.clone()).or_insert(status);
            }
        }
        for name in &self.names {
            self.statuses.entry(name.clone()).or_insert(Status::Waiting);
        }
        let never_cached = self
            .graph
            .actions
            .iter()
            .filter(|(_, action)| !action.kind.is_cacheable())
            .map(|(name, _)| name.clone())
            .collect();
        Outcome { goal: goal.to_owned(), statuses: self.statuses, names: self.names, facts: self.facts, never_cached }
    }
}

impl<'a> Runner<'a> {
    pub fn new(graph: &'a Graph, backend: &'a dyn Backend, parallelism: usize) -> Self {
        Runner { graph, backend, parallelism: parallelism.max(1) }
    }

    /// Runs what the goal needs, and reports progress to `observe`.
    pub fn run(&self, goal: &str, observe: &mut dyn FnMut(Progress)) -> Result<Outcome, RunError> {
        let mut run = Run::new(self.graph, goal)?;
        thread::scope(|scope| {
            let (sender, receiver) = mpsc::channel::<Message>();
            let mut running = 0usize;
            loop {
                let ready = rules::derive(&run.facts).must_run;
                let mut dispatched = false;
                for id in ready {
                    if running >= self.parallelism {
                        break;
                    }
                    dispatched = true;
                    let (index, is_new) = run.start(id);
                    if !is_new {
                        run.publish_if_done(index);
                        report_joined(&run, index, id, observe);
                        continue;
                    }
                    let effects = run.apply(index, Event::Dispatch);
                    debug_assert_eq!(effects, vec![Effect::LookUpCache]);
                    let lookup = self.backend.cached(&run.executions[index].action);
                    let event = match lookup {
                        Ok(Some(result)) => Event::CacheHit(result),
                        Ok(None) => Event::CacheMiss,
                        Err(error) => Event::Errored(error.to_string()),
                    };
                    if run.apply(index, event).contains(&Effect::Run) {
                        let action = run.executions[index].action.clone();
                        let name = run.names[id as usize].clone();
                        observe(Progress::Started { name, action: action.digest() });
                        let sender = sender.clone();
                        let backend = self.backend;
                        running += 1;
                        scope.spawn(move || {
                            let logs = ChannelSink { index, sender: sender.clone() };
                            let outcome = backend.execute(&action, &logs);
                            sender.send(Message::Done { index, outcome }).ok();
                        });
                    }
                    report_done(&run, index, observe);
                }
                if dispatched && running < self.parallelism {
                    continue;
                }
                if running == 0 {
                    break;
                }
                let (index, outcome) = loop {
                    match receiver.recv().expect("a worker sends its outcome") {
                        Message::Done { index, outcome } => break (index, outcome),
                        Message::Log { index, stream, line } => report_log(&run, index, stream, line, observe),
                    }
                };
                running -= 1;
                let event = match outcome {
                    Ok(execution) => {
                        run.executions[index].discarded = execution.discarded;
                        Event::Finished(execution.result)
                    }
                    Err(error) => Event::Errored(error.to_string()),
                };
                run.apply(index, event);
                report_done(&run, index, observe);
            }
        });
        let outcome = run.finish(goal);
        for (name, status) in &outcome.statuses {
            if matches!(status, Status::Skipped) {
                observe(Progress::Done { name: name.clone(), status: status.clone() });
            }
        }
        Ok(outcome)
    }

    /// What a run would do, from the cache alone: nothing runs.
    pub fn plan(&self, goal: &str) -> Result<Outcome, RunError> {
        let mut run = Run::new(self.graph, goal)?;
        let mut will_run = BTreeMap::new();
        loop {
            let ready = rules::derive(&run.facts).must_run;
            if ready.is_empty() {
                break;
            }
            for id in ready {
                let (index, is_new) = run.start(id);
                if !is_new {
                    run.publish_if_done(index);
                    continue;
                }
                run.apply(index, Event::Dispatch);
                match self.backend.cached(&run.executions[index].action).map_err(RunError::Backend)? {
                    Some(result) => {
                        run.apply(index, Event::CacheHit(result));
                    }
                    None => {
                        will_run.insert(run.names[id as usize].clone(), run.executions[index].action.digest());
                    }
                }
            }
        }
        for (name, action) in will_run {
            run.statuses.insert(name, Status::WillRun { action });
        }
        Ok(run.finish(goal))
    }
}

fn status_without_execution(derived: &rules::Derived, id: Id) -> Option<Status> {
    if !derived.needed.contains(&id) {
        return Some(Status::NotNeeded);
    }
    if derived.skipped.contains(&id) {
        return Some(Status::Skipped);
    }
    None
}

/// What a worker tells the run loop: a line of output while the action runs, and then its outcome.
enum Message {
    Log { index: usize, stream: Stream, line: String },
    Done { index: usize, outcome: Result<BackendExecution, BackendError> },
}

struct ChannelSink {
    index: usize,
    sender: mpsc::Sender<Message>,
}

impl LogSink for ChannelSink {
    fn line(&self, stream: Stream, line: &str) {
        self.sender.send(Message::Log { index: self.index, stream, line: line.to_owned() }).ok();
    }
}

fn report_log(run: &Run<'_>, index: usize, stream: Stream, line: String, observe: &mut dyn FnMut(Progress)) {
    for &id in &run.executions[index].attached {
        observe(Progress::Log { name: run.names[id as usize].clone(), stream, line: line.clone() });
    }
}

/// An action that joins an execution with the same digest is reported as the execution is: it starts with a running
/// execution, and is done at once with a finished one.
fn report_joined(run: &Run<'_>, index: usize, id: Id, observe: &mut dyn FnMut(Progress)) {
    let execution = &run.executions[index];
    let name = &run.names[id as usize];
    if matches!(execution.state, State::Running) {
        observe(Progress::Started { name: name.clone(), action: execution.action.digest() });
        return;
    }
    if let Some(status) = run.statuses.get(name).filter(|_| execution.state.is_terminal()) {
        observe(Progress::Done { name: name.clone(), status: status.clone() });
    }
}

fn report_done(run: &Run<'_>, index: usize, observe: &mut dyn FnMut(Progress)) {
    let execution = &run.executions[index];
    if !execution.state.is_terminal() {
        return;
    }
    for &id in &execution.attached {
        let name = &run.names[id as usize];
        if let Some(status) = run.statuses.get(name) {
            observe(Progress::Done { name: name.clone(), status: status.clone() });
        }
    }
}
