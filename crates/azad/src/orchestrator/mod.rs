//! The orchestrator (decision 0020): it takes GitHub's webhooks for pushes and pull requests, keeps a run for each
//! event in durable state, runs the goals that the repository names for the event at the event's trust level, and
//! writes a check run for each goal. Check runs go to GitHub through a GitHub App (decision 0032), or locally to a
//! fake GitHub (`checks::FakeGitHub`), to which `Simulator` sends events from a local git repository.
//!
//! Runs run at the same time, and so do the goals of a run; their actions share one pool (`engine::Pool`), so that the
//! same action runs once. A newer commit of a branch or a pull request cancels the runs of its older commits, and one
//! commit gets one check run for each goal at a trust level, also when a push and a pull request both name it. Someone
//! can run a check run again from GitHub (decision 0035).
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
pub use event::{Event, EventKind, Repository, Rerun};
pub use github::{GITHUB_API, GitHub, GitHubError};
pub use http::Response;
pub use signature::{SIGNATURE_HEADER, sign};
pub use simulate::{SimulateError, Simulator};

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::engine::{Engine, EngineConfig, Pool};
use crate::ir::Kind;
use crate::remote::RemoteConfig;

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
/// How many actions run at the same time, for all runs together, unless the orchestrator is told otherwise.
pub const DEFAULT_PARALLELISM: usize = 4;
/// How many runs run at the same time; their actions share the pool.
const RUNS_AT_A_TIME: usize = 8;
/// How long the worker waits for a new event before it looks at the runs again.
const IDLE_CHECK: Duration = Duration::from_secs(30);
/// How often a running goal's check run shows its progress, within GitHub's rate limit.
const PROGRESS_INTERVAL: Duration = Duration::from_secs(5);
const SHORT_SHA: usize = 12;

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
    github: Option<Arc<GitHub>>,
    /// Where the site shows the runs, such as `http://127.0.0.1:8485`, so that check runs link to their run's page.
    site: Option<String>,
    parallelism: usize,
    pool: Pool,
    /// The remote cache of each run, as `azad --cache` names it, such as `gha`.
    cache: Option<String>,
    /// The runs that run now.
    active: Mutex<BTreeMap<u64, Arc<Active>>>,
    new_events: (Mutex<bool>, Condvar),
}

/// A run that runs now.
#[derive(Debug, Default)]
struct Active {
    cancelled: AtomicBool,
    /// The check runs that completed, which change no more.
    completed: Mutex<BTreeSet<u64>>,
}

impl Active {
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    /// Updates a check run of the run, unless it completed already: a run that was cancelled while a goal ran keeps
    /// its conclusion when the goal ends.
    fn update(
        &self,
        checks: &dyn CheckRuns,
        repository: &str,
        check: u64,
        conclusion: Option<Conclusion>,
        output: &CheckOutput,
    ) -> io::Result<()> {
        let mut completed = self.completed.lock().expect("the lock of the completed check runs");
        if completed.contains(&check) {
            return Ok(());
        }
        let status = match conclusion {
            Some(_) => CheckStatus::Completed,
            None => CheckStatus::InProgress,
        };
        checks.update(repository, check, status, conclusion, output)?;
        if conclusion.is_some() {
            completed.insert(check);
        }
        Ok(())
    }
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
            parallelism: DEFAULT_PARALLELISM,
            pool: Pool::new(DEFAULT_PARALLELISM),
            cache: None,
            active: Mutex::default(),
            new_events: (Mutex::new(false), Condvar::new()),
        }
    }

    /// As `new`, with check runs on GitHub through `app`, which also fetches the repositories of its installations.
    pub fn with_github(engine: EngineConfig, secret: &[u8], app: GitHub) -> Self {
        let app = Arc::new(app);
        Orchestrator { checks: Box::new(app.clone()), github: Some(app), ..Orchestrator::new(engine, secret) }
    }

    /// Check runs link to their run's page on the site at `url` (decision 0034).
    pub fn with_site(self, url: &str) -> Self {
        Orchestrator { site: Some(url.trim_end_matches('/').to_owned()), ..self }
    }

    /// At most `parallelism` actions run at the same time, for all runs together.
    pub fn with_parallelism(self, parallelism: usize) -> Self {
        Orchestrator { parallelism, pool: Pool::new(parallelism), ..self }
    }

    /// Each run reads and writes the remote cache that `spec` names, as `azad --cache` takes it.
    pub fn with_cache(self, spec: &str) -> Self {
        Orchestrator { cache: Some(spec.to_owned()), ..self }
    }

    /// Runs one event now, as in a GHA job that the event started (decision 0036), and says how each of its goals
    /// ended. Its check runs show the same; the run's record stays in the state, as a served run's does.
    pub fn run_once(&self, event: &Event) -> io::Result<BTreeMap<String, Conclusion>> {
        std::fs::create_dir_all(&self.state)?;
        let id = self.runs.add(event)?;
        self.process(id, event, &Active::default());
        let run = record::Run::read(&self.runs.run_dir(id))?;
        if let Some(failure) = run.failure {
            return Err(io::Error::other(failure));
        }
        Ok(run.goals.into_iter().filter_map(|goal| Some((goal.name, goal.conclusion?))).collect())
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
            scope.spawn(|| self.work(scope));
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
    /// run, which the worker starts, and cancels the runs that it makes outdated; a request to run check runs again
    /// becomes a run for each run that it names.
    pub fn receive(&self, headers: &BTreeMap<String, String>, payload: &[u8]) -> Response {
        let signed = headers.get(SIGNATURE_HEADER).is_some_and(|value| signature::verify(&self.secret, payload, value));
        if !signed {
            return Response::new(401, "the signature does not match the payload");
        }
        let name = headers.get(EVENT_HEADER).map(String::as_str).unwrap_or_default();
        let events = match self.events_of(name, payload) {
            Ok(events) if events.is_empty() => return Response::new(200, format!("nothing to check in {name:?}")),
            Ok(events) => events,
            Err(response) => return response,
        };
        if let Some(delivery) = headers.get(DELIVERY_HEADER) {
            match self.runs.first_delivery(delivery) {
                Ok(true) => {}
                Ok(false) => return Response::new(200, "a delivery that came before"),
                Err(e) => return Response::new(500, format!("the delivery could not be kept: {e}")),
            }
        }
        let mut ids = Vec::new();
        for event in events {
            match self.runs.add(&event) {
                Ok(id) => {
                    self.cancel_outdated(id, &event);
                    ids.push(id.to_string());
                }
                Err(e) => return Response::new(500, format!("the run could not be kept: {e}")),
            }
        }
        self.wake();
        Response::new(202, format!("run {}", ids.join(", ")))
    }

    /// The events that a delivery asks to run.
    fn events_of(&self, name: &str, payload: &[u8]) -> Result<Vec<Event>, Response> {
        let bad = |e: serde_json::Error| Response::new(400, format!("a bad {name} payload: {e}"));
        if let Some(rerun) = Rerun::from_webhook(name, payload).map_err(bad)? {
            return self
                .reruns(&rerun)
                .map_err(|e| Response::new(500, format!("the runs to run again cannot be read: {e}")));
        }
        Ok(Event::from_webhook(name, payload).map_err(bad)?.into_iter().collect())
    }

    /// The events that run the goals of a request again: for each goal, the event of the newest run that has a check
    /// run of it that the request names, which runs only the goals that it names.
    fn reruns(&self, rerun: &Rerun) -> io::Result<Vec<Event>> {
        let mut newest: BTreeMap<String, (u64, Event)> = BTreeMap::new();
        for (id, event) in self.runs.all()? {
            if event.repository.full_name != rerun.repository() {
                continue;
            }
            let Some(checks) = self.runs.checks(id)? else { continue };
            for (goal, check) in checks {
                let named = match rerun {
                    Rerun::Check { id: wanted, .. } => check == *wanted,
                    Rerun::Suite { head_sha, .. } => event.head_sha == *head_sha,
                };
                if named {
                    newest.insert(goal, (id, event.clone()));
                }
            }
        }
        let mut by_run: BTreeMap<u64, Event> = BTreeMap::new();
        for (goal, (id, event)) in newest {
            let event = by_run.entry(id).or_insert(Event { rerun: Some(Vec::new()), ..event });
            event.rerun.get_or_insert_default().push(goal);
        }
        Ok(by_run.into_values().collect())
    }

    /// Cancels the runs of older commits of the event's branch or pull request, which its run makes outdated.
    fn cancel_outdated(&self, id: u64, event: &Event) {
        if event.rerun.is_some() || !event.is_outdated_by_newer_commits() {
            return;
        }
        let pending = match self.runs.pending() {
            Ok(pending) => pending,
            Err(e) => return eprintln!("azad-orchestrator: the runs cannot be read to cancel outdated ones: {e}"),
        };
        let outdated = pending.into_iter().filter(|(older, other)| {
            *older < id
                && other.repository.full_name == event.repository.full_name
                && other.git_ref == event.git_ref
                && other.head_sha != event.head_sha
        });
        let short_sha = event.head_sha.get(..SHORT_SHA).unwrap_or(&event.head_sha);
        let reason = format!("A newer commit of {}, {short_sha}, runs in run {id}.", event.git_ref);
        for (older, other) in outdated {
            self.cancel(older, &other, &reason);
        }
    }

    /// Cancels a run: it starts no more steps, and its check runs complete as cancelled now. A run that does not run
    /// now is cancelled when the worker takes it.
    fn cancel(&self, id: u64, event: &Event, reason: &str) {
        if let Err(e) = self.runs.cancel(id, reason) {
            return eprintln!("azad-orchestrator: run {id} cannot be cancelled: {e}");
        }
        let cancelled = record::RecordEvent::RunCancelled { reason: reason.to_owned() };
        match record::Recorder::open(&self.runs.run_dir(id)) {
            Ok(mut recorder) => append(&mut recorder, &cancelled),
            Err(e) => eprintln!("azad-orchestrator: the cancel of run {id} cannot be recorded: {e}"),
        }
        let active = self.active.lock().expect("the lock of the active runs").get(&id).cloned();
        if let Some(run) = active {
            run.cancelled.store(true, Ordering::Relaxed);
            self.complete_cancelled(id, event, &run, reason);
        }
    }

    fn complete_cancelled(&self, id: u64, event: &Event, run: &Active, reason: &str) {
        let checks = match self.runs.checks(id) {
            Ok(checks) => checks.unwrap_or_default(),
            Err(e) => return eprintln!("azad-orchestrator: the check runs of run {id} cannot be read: {e}"),
        };
        let page = self.run_page(id);
        for (goal, check) in checks {
            let target =
                GoalTarget { name: &goal, repository: &event.repository.full_name, check, page: page.as_deref() };
            let output = linked(&target, CheckOutput::line("Cancelled: a newer commit came", reason));
            self.complete(run, &target, Conclusion::Cancelled, &output);
        }
    }

    fn wake(&self) {
        let (lock, condvar) = &self.new_events;
        if let Ok(mut new) = lock.lock() {
            *new = true;
            condvar.notify_all();
        }
    }

    /// Starts the runs that are not done, oldest first, as far as there is room, and then waits for the next event or
    /// the end of a run.
    fn work<'scope, 'env>(&'env self, scope: &'scope thread::Scope<'scope, 'env>) {
        loop {
            match self.runs.pending() {
                Ok(pending) => self.start(scope, pending),
                Err(e) => eprintln!("azad-orchestrator: the runs cannot be read: {e}"),
            }
            let (lock, condvar) = &self.new_events;
            let Ok(new) = lock.lock() else { return };
            let Ok((mut new, _)) = condvar.wait_timeout_while(new, IDLE_CHECK, |new| !*new) else { return };
            *new = false;
        }
    }

    fn start<'scope, 'env>(&'env self, scope: &'scope thread::Scope<'scope, 'env>, pending: Vec<(u64, Event)>) {
        let mut active = self.active.lock().expect("the lock of the active runs");
        for (id, event) in pending {
            if active.len() >= RUNS_AT_A_TIME {
                return;
            }
            if active.contains_key(&id) {
                continue;
            }
            let run = Arc::new(Active::default());
            active.insert(id, run.clone());
            scope.spawn(move || {
                self.process(id, &event, &run);
                self.active.lock().expect("the lock of the active runs").remove(&id);
                self.wake();
            });
        }
    }

    /// Runs one event to its end: its goals run, and their check runs complete. A run that cannot finish is reported
    /// on its check runs and is done; it does not run again.
    fn process(&self, id: u64, event: &Event, run: &Active) {
        match self.runs.cancelled(id) {
            Ok(Some(reason)) => {
                run.cancelled.store(true, Ordering::Relaxed);
                self.complete_cancelled(id, event, run, &reason);
            }
            Ok(None) => {
                if let Err(e) = self.try_process(id, event, run) {
                    self.fail(id, event, run, &e);
                }
            }
            Err(e) => self.fail(id, event, run, &e.to_string()),
        }
        if let Err(e) = self.runs.finish(id) {
            eprintln!("azad-orchestrator: run {id} cannot be marked done: {e}");
        }
    }

    fn fail(&self, id: u64, event: &Event, run: &Active, reason: &str) {
        eprintln!("azad-orchestrator: run {id} failed: {reason}");
        let failed = record::RecordEvent::RunFailed { reason: reason.to_owned() };
        match record::Recorder::open(&self.runs.run_dir(id)) {
            Ok(mut recorder) => append(&mut recorder, &failed),
            Err(e) => eprintln!("azad-orchestrator: the failure of run {id} cannot be recorded: {e}"),
        }
        let Ok(Some(checks)) = self.runs.checks(id) else { return };
        let page = self.run_page(id);
        for (goal, check) in checks {
            let target =
                GoalTarget { name: &goal, repository: &event.repository.full_name, check, page: page.as_deref() };
            let output = linked(&target, CheckOutput::line("The orchestrator could not run the goal", reason));
            self.complete(run, &target, Conclusion::Failure, &output);
        }
    }

    fn try_process(&self, id: u64, event: &Event, run: &Active) -> Result<(), String> {
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
        append(&mut recorder, &record::RecordEvent::RunStarted { trust: config.trust });
        let recorder = Mutex::new(recorder);
        let trust = config.trust;
        let engine = self.engine_for(config)?.with_pool(self.pool.clone());
        let checks = self.check_runs(id, event, &engine, trust)?;
        let page = self.run_page(id);
        thread::scope(|scope| {
            let goals: Vec<_> = checks
                .iter()
                .map(|(goal, check)| {
                    let target = GoalTarget { name: goal, repository, check: *check, page: page.as_deref() };
                    let (engine, recorder) = (&engine, &recorder);
                    scope.spawn(move || {
                        let (conclusion, output) = self.run_goal(engine, &target, recorder, run);
                        self.complete(run, &target, conclusion, &linked(&target, output));
                    })
                })
                .collect();
            for goal in goals {
                goal.join().map_err(|_| "a goal's thread panicked".to_owned())?;
            }
            Ok(())
        })
    }

    /// The engine of a run, with the remote cache of the orchestrator, when it has one.
    fn engine_for(&self, config: EngineConfig) -> Result<Engine, String> {
        let remote = match &self.cache {
            Some(spec) => RemoteConfig::from_spec(spec, &config.root).map_err(|e| e.to_string())?,
            None => None,
        };
        let engine = match remote {
            Some(remote) => Engine::with_remote(config, remote),
            None => Engine::new(config),
        };
        engine.map_err(|e| e.to_string())
    }

    /// Completes a goal's check run, unless it completed already.
    fn complete(&self, run: &Active, target: &GoalTarget<'_>, conclusion: Conclusion, output: &CheckOutput) {
        if let Err(e) = run.update(self.checks.as_ref(), target.repository, target.check, Some(conclusion), output) {
            eprintln!("azad-orchestrator: the check run of {} cannot be completed: {e}", target.name);
        }
    }

    /// Runs a goal, records what happens, shows its steps on its check run while it runs, and says how it ended.
    fn run_goal(
        &self,
        engine: &Engine,
        target: &GoalTarget<'_>,
        recorder: &Mutex<record::Recorder>,
        run: &Active,
    ) -> (Conclusion, CheckOutput) {
        let started = CheckOutput::line("Starting", "The goal is loaded and its steps are found.");
        self.show(run, target, &started);
        let loaded = engine.load(target.name, &mut |_| {});
        let actions: Vec<String> = match &loaded {
            Ok(goal) => goal.graph.actions.keys().cloned().collect(),
            Err(_) => Vec::new(),
        };
        let mut goal = record::Goal::new(target.name, &actions, record::now());
        let begun = record::RecordEvent::GoalStarted { goal: target.name.to_owned(), steps: actions };
        append_shared(recorder, &begun);
        let (conclusion, output) = match loaded {
            Ok(loaded) => self.run_loaded(engine, &loaded, target, recorder, &mut goal, run),
            Err(e) => (Conclusion::Failure, CheckOutput::line("The goal cannot be loaded", &e.to_string())),
        };
        let ended =
            record::RecordEvent::GoalEnded { goal: target.name.to_owned(), conclusion, title: output.title.clone() };
        append_shared(recorder, &ended);
        (conclusion, output)
    }

    fn run_loaded(
        &self,
        engine: &Engine,
        loaded: &crate::engine::Goal,
        target: &GoalTarget<'_>,
        recorder: &Mutex<record::Recorder>,
        goal: &mut record::Goal,
        run: &Active,
    ) -> (Conclusion, CheckOutput) {
        if let Some(label) = loaded.graph.actions.iter().find(|(_, action)| matches!(action.kind, Kind::Host { .. })) {
            let title = format!("{} would run with the host's tools", label.0);
            let reason = "The orchestrator runs actions only in sandboxes, because an event runs code that anyone who can \
                          push or open a pull request wrote. Give the task an image.";
            return (Conclusion::Failure, CheckOutput::line(&title, reason));
        }
        let mut shown = Instant::now();
        let ran = engine.run_until_stopped(loaded, self.parallelism, &run.cancelled, &mut |event| {
            let crate::engine::Event::Progress(progress) = &event else { return };
            let entry = record::Entry { at: record::now(), event: record_event(target.name, progress) };
            append_shared(recorder, &entry.event);
            goal.apply(entry.at, &entry.event);
            if shown.elapsed() >= PROGRESS_INTERVAL {
                shown = Instant::now();
                self.show(run, target, &report::output(goal, record::now()));
            }
        });
        let outcome = match ran {
            Ok(ran) => ran.outcome,
            Err(e) => return (Conclusion::Failure, CheckOutput::line("The goal could not run", &e.to_string())),
        };
        let conclusion = match (run.is_cancelled(), outcome.goal_holds()) {
            (true, _) => Conclusion::Cancelled,
            (false, true) => Conclusion::Success,
            (false, false) => Conclusion::Failure,
        };
        (conclusion, report::output(goal, record::now()))
    }

    /// Shows a running goal's output on its check run; a failure to show it does not stop the goal.
    fn show(&self, run: &Active, target: &GoalTarget<'_>, output: &CheckOutput) {
        let output = linked(target, output.clone());
        if let Err(e) = run.update(self.checks.as_ref(), target.repository, target.check, None, &output) {
            eprintln!("azad-orchestrator: the progress of {} cannot be shown: {e}", target.name);
        }
    }

    /// The page of run `id` on the site, when there is a site.
    fn run_page(&self, id: u64) -> Option<String> {
        self.site.as_ref().map(|site| format!("{site}/runs/{id}"))
    }

    /// The check run of each goal that the run checks: made once, and kept with the run, so that a run that starts
    /// again after a stop uses the same check runs. A run checks the goals that someone asked to run again, or else
    /// each goal that names the event and that no other run checks on the same commit at the same trust level.
    fn check_runs(
        &self,
        id: u64,
        event: &Event,
        engine: &Engine,
        trust: Trust,
    ) -> Result<BTreeMap<String, u64>, String> {
        if let Some(checks) = self.runs.checks(id).map_err(|e| e.to_string())? {
            return Ok(checks);
        }
        let goals = match &event.rerun {
            Some(goals) => goals.clone(),
            None => self.claimed_goals(id, event, engine, trust)?,
        };
        let mut checks = BTreeMap::new();
        for goal in goals {
            let details = self.run_page(id);
            let check = self
                .checks
                .create(&event.repository.full_name, &event.head_sha, &goal, details.as_deref())
                .map_err(|e| e.to_string())?;
            checks.insert(goal, check);
        }
        self.runs.set_checks(id, &checks).map_err(|e| e.to_string())?;
        Ok(checks)
    }

    /// The goals that name the event, and whose check on the commit at the trust level the run claims.
    fn claimed_goals(&self, id: u64, event: &Event, engine: &Engine, trust: Trust) -> Result<Vec<String>, String> {
        let mut goals = Vec::new();
        for target in engine.targets().map_err(|e| e.to_string())? {
            if !target.events.contains(event.kind.name()) {
                continue;
            }
            if self.runs.claim(event, trust, &target.target, id).map_err(|e| e.to_string())? {
                goals.push(target.target);
            }
        }
        Ok(goals)
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

/// Appends an event to the record of a run whose goals run at the same time.
fn append_shared(recorder: &Mutex<record::Recorder>, event: &record::RecordEvent) {
    append(&mut recorder.lock().expect("the lock of the run's record"), event);
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
