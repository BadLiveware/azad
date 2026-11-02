//! The orchestrator (decision 0020): it takes GitHub's webhooks for pushes and pull requests, keeps a run for each
//! event in durable state, runs the goals that the repository names for the event at the event's trust level, and
//! writes a check run for each goal. Locally, check runs go to a fake GitHub (`checks::FakeGitHub`), and `Simulator`
//! sends events from a local git repository.
//!
//! Goals run in their images, as in CI, never with the host's tools: an event runs code that anyone who can push or
//! open a pull request wrote.

mod checks;
mod event;
mod git;
mod http;
mod runs;
mod signature;
mod simulate;

pub use checks::{CheckRun, CheckRuns, CheckStatus, Conclusion, FakeGitHub};
pub use event::{Event, EventKind, Repository};
pub use http::Response;
pub use signature::{SIGNATURE_HEADER, sign};
pub use simulate::{SimulateError, Simulator};

use std::collections::BTreeMap;
use std::io;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};
use std::thread;
use std::time::Duration;

use crate::engine::{Engine, EngineConfig, Status};
use crate::ir::Kind;
use crate::scheduler::Outcome;

const ORCHESTRATOR_DIR: &str = "orchestrator";
const GITHUB_DIR: &str = "github";
const CHECKOUTS_DIR: &str = "checkouts";
const LOCK_FILE: &str = "lock";
pub const WEBHOOK_PATH: &str = "/webhook";
const EVENT_HEADER: &str = "x-github-event";
const DELIVERY_HEADER: &str = "x-github-delivery";
const POST: &str = "POST";
/// How many actions of a goal run at the same time.
const PARALLELISM: usize = 4;
/// How long the worker waits for a new event before it looks at the runs again.
const IDLE_CHECK: Duration = Duration::from_secs(30);

/// Where the orchestrator of a store keeps its state.
pub fn state_dir(store: &Path) -> PathBuf {
    store.join(ORCHESTRATOR_DIR)
}

/// The fake GitHub of the orchestrator of a store, which holds its check runs.
pub fn fake_github(store: &Path) -> FakeGitHub {
    FakeGitHub::new(state_dir(store).join(GITHUB_DIR))
}

#[derive(Debug)]
pub struct Orchestrator {
    engine: EngineConfig,
    state: PathBuf,
    secret: Vec<u8>,
    runs: runs::Runs,
    checks: Box<dyn CheckRuns>,
    new_events: (Mutex<bool>, Condvar),
}

impl Orchestrator {
    /// An orchestrator that runs goals with the store, the sandbox and the executor of `engine`, checks deliveries
    /// with `secret`, and writes check runs to the fake GitHub of the store.
    pub fn new(engine: EngineConfig, secret: &[u8]) -> Self {
        let store = std::path::absolute(&engine.store).unwrap_or_else(|_| engine.store.clone());
        let state = state_dir(&store);
        Orchestrator {
            checks: Box::new(fake_github(&store)),
            runs: runs::Runs::new(&state),
            state,
            engine,
            secret: secret.to_vec(),
            new_events: (Mutex::new(false), Condvar::new()),
        }
    }

    /// Takes deliveries on `listener`, and runs what they ask for, until the listener fails. Runs that were not done
    /// when the orchestrator stopped run first. One orchestrator at a time serves a store, so that no run runs twice.
    pub fn serve(&self, listener: &TcpListener) -> io::Result<()> {
        std::fs::create_dir_all(&self.state)?;
        let lock =
            std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(self.state.join(LOCK_FILE))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(io::Error::other(format!("another orchestrator serves {}", self.state.display())));
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(e),
        }
        thread::scope(|scope| {
            scope.spawn(|| self.work());
            for connection in listener.incoming() {
                self.answer(&connection?);
            }
            Ok(())
        })
    }

    fn answer(&self, stream: &TcpStream) {
        let response = match http::read_request(stream) {
            Ok(request) if request.method != POST => Response::new(405, "only POST"),
            Ok(request) if request.path != WEBHOOK_PATH => {
                Response::new(404, format!("deliveries go to {WEBHOOK_PATH}"))
            }
            Ok(request) => self.receive(&request.headers, &request.body),
            Err(e) => Response::new(400, e.to_string()),
        };
        http::write_response(stream, &response).ok();
    }

    /// Takes one delivery: its headers by lower-case name, and its payload. An event with a commit to check becomes a
    /// run, which the worker starts.
    pub fn receive(&self, headers: &BTreeMap<String, String>, payload: &[u8]) -> Response {
        let signed = headers.get(SIGNATURE_HEADER).is_some_and(|value| signature::verify(&self.secret, payload, value));
        if !signed {
            return Response::new(401, "the signature does not match the payload");
        }
        let name = headers.get(EVENT_HEADER).map(String::as_str).unwrap_or_default();
        let event = match Event::from_webhook(name, payload) {
            Ok(Some(event)) => event,
            Ok(None) => return Response::new(200, format!("nothing to check in {name:?}")),
            Err(e) => return Response::new(400, format!("a bad {name} payload: {e}")),
        };
        match self.runs.add(&event, headers.get(DELIVERY_HEADER).map(String::as_str)) {
            Ok(Some(id)) => {
                self.wake();
                Response::new(202, format!("run {id}"))
            }
            Ok(None) => Response::new(200, "a delivery that came before"),
            Err(e) => Response::new(500, format!("the run could not be kept: {e}")),
        }
    }

    fn wake(&self) {
        let (lock, condvar) = &self.new_events;
        if let Ok(mut new) = lock.lock() {
            *new = true;
            condvar.notify_all();
        }
    }

    /// Runs every run that is not done, oldest first, then waits for the next event.
    fn work(&self) {
        loop {
            match self.runs.pending() {
                Ok(pending) => {
                    for (id, event) in pending {
                        self.process(id, &event);
                    }
                }
                Err(e) => eprintln!("azad serve: the runs cannot be read: {e}"),
            }
            let (lock, condvar) = &self.new_events;
            let Ok(new) = lock.lock() else { return };
            let Ok((mut new, _)) = condvar.wait_timeout_while(new, IDLE_CHECK, |new| !*new) else { return };
            *new = false;
        }
    }

    /// Runs one event to its end: its goals run, and their check runs complete. A run that cannot finish is reported
    /// on its check runs and is done; it does not run again.
    fn process(&self, id: u64, event: &Event) {
        if let Err(e) = self.try_process(id, event) {
            eprintln!("azad serve: run {id} failed: {e}");
            if let Ok(Some(checks)) = self.runs.checks(id) {
                for check in checks.values() {
                    let summary = format!("the orchestrator could not run the goal: {e}");
                    self.checks.update(*check, CheckStatus::Completed, Some(Conclusion::Failure), &summary).ok();
                }
            }
        }
        if let Err(e) = self.runs.finish(id) {
            eprintln!("azad serve: run {id} cannot be marked done: {e}");
        }
    }

    fn try_process(&self, id: u64, event: &Event) -> Result<(), String> {
        let dir = self.state.join(CHECKOUTS_DIR).join(id.to_string());
        std::fs::create_dir_all(dir.parent().unwrap_or(&dir)).map_err(|e| e.to_string())?;
        let checkout = git::check_out(&self.state, &event.repository.clone_url, &event.git_ref, &event.head_sha, &dir)
            .map_err(|e| e.to_string())?;
        let config = EngineConfig {
            root: checkout.path().to_path_buf(),
            isolated: true,
            trust: event.trust(),
            color: false,
            force: false,
            ..self.engine.clone()
        };
        let engine = Engine::new(config).map_err(|e| e.to_string())?;
        let checks = self.check_runs(id, event, &engine)?;
        for (goal, check) in &checks {
            self.checks.update(*check, CheckStatus::InProgress, None, "").map_err(|e| e.to_string())?;
            let (conclusion, summary) = run_goal(&engine, goal);
            self.checks
                .update(*check, CheckStatus::Completed, Some(conclusion), &summary)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// The check run of each goal that the event runs: made once, and kept with the run, so that a run that starts
    /// again after a stop uses the same check runs.
    fn check_runs(&self, id: u64, event: &Event, engine: &Engine) -> Result<BTreeMap<String, u64>, String> {
        if let Some(checks) = self.runs.checks(id).map_err(|e| e.to_string())? {
            return Ok(checks);
        }
        let targets = engine.targets().map_err(|e| e.to_string())?;
        let mut checks = BTreeMap::new();
        for target in targets.into_iter().filter(|target| target.events.contains(event.kind.name())) {
            let check = self
                .checks
                .create(&event.repository.full_name, &event.head_sha, &target.target)
                .map_err(|e| e.to_string())?;
            checks.insert(target.target, check);
        }
        self.runs.set_checks(id, &checks).map_err(|e| e.to_string())?;
        Ok(checks)
    }
}

/// Runs a goal, and says how it ended.
fn run_goal(engine: &Engine, target: &str) -> (Conclusion, String) {
    let goal = match engine.load(target, &mut |_| {}) {
        Ok(goal) => goal,
        Err(e) => return (Conclusion::Failure, format!("the goal cannot be loaded: {e}")),
    };
    if let Some(label) = goal.graph.actions.iter().find(|(_, action)| matches!(action.kind, Kind::Host { .. })) {
        let reason =
            format!("{} would run with the host's tools; the orchestrator runs actions only in sandboxes", label.0);
        return (Conclusion::Failure, reason);
    }
    match engine.run(&goal, PARALLELISM, &mut |_| {}) {
        Ok(report) if report.outcome.goal_holds() => (Conclusion::Success, summary(&report.outcome)),
        Ok(report) => (Conclusion::Failure, summary(&report.outcome)),
        Err(e) => (Conclusion::Failure, e.to_string()),
    }
}

/// How many actions ran, came from the cache and failed, and why each failure happened.
fn summary(outcome: &Outcome) -> String {
    let count = |matches: fn(&Status) -> bool| outcome.statuses.values().filter(|status| matches(status)).count();
    let ran = count(|status| matches!(status, Status::Ran { .. }));
    let cached = count(|status| matches!(status, Status::Cached { .. }));
    let failed = count(|status| matches!(status, Status::Failed { .. }));
    let mut text = format!("{ran} ran, {cached} cached, {failed} failed");
    for (label, status) in &outcome.statuses {
        if let Status::Failed { reason, .. } = status {
            text.push_str(&format!("\n{label}: {reason}"));
        }
    }
    text
}
