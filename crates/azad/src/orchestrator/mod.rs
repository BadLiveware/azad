//! The orchestrator (decision 0020): it takes GitHub's webhooks for pushes and pull requests, keeps a run for each
//! event in durable state, runs the goals that the repository names for the event at the event's trust level, and
//! writes a check run for each goal. Check runs go to GitHub through a GitHub App (decision 0032), or locally to a
//! fake GitHub (`checks::FakeGitHub`), to which `Simulator` sends events from a local git repository.
//!
//! Goals run in their images, as in CI, never with the host's tools: an event runs code that anyone who can push or
//! open a pull request wrote.

mod checks;
mod event;
mod git;
mod github;
mod http;
pub mod record;
pub mod report;
mod runs;
mod signature;
mod simulate;

pub use checks::{CheckOutput, CheckRun, CheckRuns, CheckStatus, Conclusion, FakeGitHub};
pub use event::{Event, EventKind, Repository};
pub use github::{GITHUB_API, GitHubApp, GitHubError};
pub use http::Response;
pub use signature::{SIGNATURE_HEADER, sign};
pub use simulate::{SimulateError, Simulator};

use std::collections::BTreeMap;
use std::io;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::engine::{Engine, EngineConfig};
use crate::ir::Kind;

use crate::trust::Trust;

const ORCHESTRATOR_DIR: &str = "orchestrator";
const GITHUB_DIR: &str = "github";
const CHECKOUTS_DIR: &str = "checkouts";
const LOCK_FILE: &str = "lock";
pub const WEBHOOK_PATH: &str = "/webhook";
const EVENT_HEADER: &str = "x-github-event";
const DELIVERY_HEADER: &str = "x-github-delivery";
const POST: &str = "POST";
const TAG_PREFIX: &str = "refs/tags/";
/// How many actions of a goal run at the same time.
const PARALLELISM: usize = 4;
/// How long the worker waits for a new event before it looks at the runs again.
const IDLE_CHECK: Duration = Duration::from_secs(30);
/// How often a running goal's check run shows its progress, within GitHub's rate limit.
const PROGRESS_INTERVAL: Duration = Duration::from_secs(5);

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
    /// The App that writes check runs on GitHub and fetches its repositories; none for the fake GitHub.
    github: Option<Arc<GitHubApp>>,
    /// Where the site shows the runs, such as `http://127.0.0.1:8485`, so that check runs link to their run's page.
    site: Option<String>,
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
            github: None,
            site: None,
            runs: runs::Runs::new(&state),
            state,
            engine,
            secret: secret.to_vec(),
            new_events: (Mutex::new(false), Condvar::new()),
        }
    }

    /// As `new`, with check runs on GitHub through `app`, which also fetches the repositories of its installations.
    pub fn with_github(engine: EngineConfig, secret: &[u8], app: GitHubApp) -> Self {
        let app = Arc::new(app);
        Orchestrator { checks: Box::new(app.clone()), github: Some(app), ..Orchestrator::new(engine, secret) }
    }

    /// Check runs link to their run's page on the site at `url` (decision 0034).
    pub fn with_site(self, url: &str) -> Self {
        Orchestrator { site: Some(url.trim_end_matches('/').to_owned()), ..self }
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
                Err(e) => eprintln!("azad-orchestrator: the runs cannot be read: {e}"),
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
            eprintln!("azad-orchestrator: run {id} failed: {e}");
            let failed =
                record::Entry { at: record::now(), event: record::RecordEvent::RunFailed { reason: e.clone() } };
            if let Err(e) = record::Recorder::open(&self.runs.run_dir(id)).and_then(|mut record| record.append(&failed))
            {
                eprintln!("azad-orchestrator: the failure of run {id} cannot be recorded: {e}");
            }
            if let Ok(Some(checks)) = self.runs.checks(id) {
                for check in checks.values() {
                    let output = CheckOutput::line("The orchestrator could not run the goal", &e);
                    let repository = &event.repository.full_name;
                    let failure = Some(Conclusion::Failure);
                    self.checks.update(repository, *check, CheckStatus::Completed, failure, &output).ok();
                }
            }
        }
        if let Err(e) = self.runs.finish(id) {
            eprintln!("azad-orchestrator: run {id} cannot be marked done: {e}");
        }
    }

    fn try_process(&self, id: u64, event: &Event) -> Result<(), String> {
        let dir = self.state.join(CHECKOUTS_DIR).join(id.to_string());
        std::fs::create_dir_all(dir.parent().unwrap_or(&dir)).map_err(|e| e.to_string())?;
        let repository = &event.repository.full_name;
        let header = match &self.github {
            Some(app) => Some(app.git_header(repository).map_err(|e| e.to_string())?),
            None => None,
        };
        let fetch = git::Fetch {
            url: &event.repository.clone_url,
            git_ref: &event.git_ref,
            sha: &event.head_sha,
            default_branch: event.git_ref.starts_with(TAG_PREFIX).then_some(event.repository.default_branch.as_str()),
            header,
        };
        let checkout = git::check_out(&self.state, &fetch, &dir).map_err(|e| e.to_string())?;
        let config = EngineConfig {
            root: checkout.path().to_path_buf(),
            isolated: true,
            trust: trust(event, &checkout),
            color: false,
            force: false,
            ..self.engine.clone()
        };
        let mut recorder = record::Recorder::open(&self.runs.run_dir(id)).map_err(|e| e.to_string())?;
        let started = record::RecordEvent::RunStarted { trust: config.trust };
        recorder.append(&record::Entry { at: record::now(), event: started }).map_err(|e| e.to_string())?;
        let engine = Engine::new(config).map_err(|e| e.to_string())?;
        let checks = self.check_runs(id, event, &engine)?;
        for (goal, check) in &checks {
            let page = self.run_page(id);
            let target = GoalTarget { name: goal, repository, check: *check, page: page.as_deref() };
            let (conclusion, output) = self.run_goal(&engine, &target, &mut recorder)?;
            self.checks
                .update(repository, *check, CheckStatus::Completed, Some(conclusion), &linked(&target, output))
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Runs a goal, records what happens, shows its steps on its check run while it runs, and says how it ended.
    fn run_goal(
        &self,
        engine: &Engine,
        target: &GoalTarget<'_>,
        recorder: &mut record::Recorder,
    ) -> Result<(Conclusion, CheckOutput), String> {
        let started = CheckOutput::line("Starting", "The goal is loaded and its steps are found.");
        self.show(target, &started);
        let loaded = engine.load(target.name, &mut |_| {});
        let actions: Vec<String> = match &loaded {
            Ok(goal) => goal.graph.actions.keys().cloned().collect(),
            Err(_) => Vec::new(),
        };
        let mut goal = record::Goal::new(target.name, &actions, record::now());
        let begun = record::RecordEvent::GoalStarted { goal: target.name.to_owned(), steps: actions };
        append(recorder, &begun);
        let result = match loaded {
            Ok(loaded) => self.run_loaded(engine, &loaded, target, recorder, &mut goal),
            Err(e) => (Conclusion::Failure, CheckOutput::line("The goal cannot be loaded", &e.to_string())),
        };
        let (conclusion, output) = result;
        let ended =
            record::RecordEvent::GoalEnded { goal: target.name.to_owned(), conclusion, title: output.title.clone() };
        append(recorder, &ended);
        Ok((conclusion, output))
    }

    fn run_loaded(
        &self,
        engine: &Engine,
        loaded: &crate::engine::Goal,
        target: &GoalTarget<'_>,
        recorder: &mut record::Recorder,
        goal: &mut record::Goal,
    ) -> (Conclusion, CheckOutput) {
        if let Some(label) = loaded.graph.actions.iter().find(|(_, action)| matches!(action.kind, Kind::Host { .. })) {
            let title = format!("{} would run with the host's tools", label.0);
            let reason = "The orchestrator runs actions only in sandboxes, because an event runs code that anyone who can \
                          push or open a pull request wrote. Give the task an image.";
            return (Conclusion::Failure, CheckOutput::line(&title, reason));
        }
        let mut shown = Instant::now();
        let ran = engine.run(loaded, PARALLELISM, &mut |event| {
            let crate::engine::Event::Progress(progress) = &event else { return };
            let entry = record::Entry { at: record::now(), event: record_event(target.name, progress) };
            append(recorder, &entry.event);
            goal.apply(entry.at, &entry.event);
            if shown.elapsed() >= PROGRESS_INTERVAL {
                shown = Instant::now();
                self.show(target, &report::output(goal, record::now()));
            }
        });
        let outcome = match ran {
            Ok(ran) => ran.outcome,
            Err(e) => return (Conclusion::Failure, CheckOutput::line("The goal could not run", &e.to_string())),
        };
        let conclusion = match outcome.goal_holds() {
            true => Conclusion::Success,
            false => Conclusion::Failure,
        };
        (conclusion, report::output(goal, record::now()))
    }

    /// Shows a running goal's output on its check run; a failure to show it does not stop the goal.
    fn show(&self, target: &GoalTarget<'_>, output: &CheckOutput) {
        let output = linked(target, output.clone());
        if let Err(e) = self.checks.update(target.repository, target.check, CheckStatus::InProgress, None, &output) {
            eprintln!("azad-orchestrator: the progress of {} cannot be shown: {e}", target.name);
        }
    }

    /// The page of run `id` on the site, when there is a site.
    fn run_page(&self, id: u64) -> Option<String> {
        self.site.as_ref().map(|site| format!("{site}/runs/{id}"))
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
            let details = self.run_page(id);
            let check = self
                .checks
                .create(&event.repository.full_name, &event.head_sha, &target.target, details.as_deref())
                .map_err(|e| e.to_string())?;
            checks.insert(target.target, check);
        }
        self.runs.set_checks(id, &checks).map_err(|e| e.to_string())?;
        Ok(checks)
    }
}

/// A goal of a run, and its check run.
struct GoalTarget<'a> {
    name: &'a str,
    repository: &'a str,
    check: u64,
    /// The run's page on the site.
    page: Option<&'a str>,
}

/// The output with a link to the run's page at the start of its summary. GitHub's page of a check run does not show a
/// `details_url` of plain HTTP, such as a site on a host's loopback address, but it shows a link in the summary.
fn linked(target: &GoalTarget<'_>, output: CheckOutput) -> CheckOutput {
    let Some(page) = target.page else { return output };
    CheckOutput {
        summary: format!("[See the run and its whole output on Azad's site]({page})\n\n{}", output.summary),
        ..output
    }
}

/// The event of the record that a run's progress gives.
fn record_event(goal: &str, progress: &crate::scheduler::Progress) -> record::RecordEvent {
    use crate::scheduler::Progress;
    let goal = goal.to_owned();
    match progress {
        Progress::Started { name, .. } => record::RecordEvent::StepStarted { goal, step: name.clone() },
        Progress::Log { name, line, .. } => record::RecordEvent::Line { goal, step: name.clone(), text: line.clone() },
        Progress::Done { name, status } => {
            let (state, reason) = record::StepState::of(status);
            record::RecordEvent::StepEnded { goal, step: name.clone(), state, reason }
        }
    }
}

/// Appends an event to the run's record; a record that cannot be written does not stop the run.
fn append(recorder: &mut record::Recorder, event: &record::RecordEvent) {
    let entry = record::Entry { at: record::now(), event: event.clone() };
    if let Err(e) = recorder.append(&entry) {
        eprintln!("azad-orchestrator: the run's record cannot be written: {e}");
    }
}

/// The trust level of the event's runs: the event's own, but a tag whose commit the default branch holds runs as
/// `main`, because `main` already ran that code.
fn trust(event: &Event, checkout: &git::Checkout) -> Trust {
    match event.trust() {
        Trust::None if event.git_ref.starts_with(TAG_PREFIX) && checkout.is_on_default_branch(&event.head_sha) => {
            Trust::Main
        }
        trust => trust,
    }
}
