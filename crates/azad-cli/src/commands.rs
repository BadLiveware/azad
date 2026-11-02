use std::fs;
use std::io::{self, IsTerminal, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use azad::frontend::starlark::{TASKS_FILE, package_of};
use azad::frontend::{Listed, is_at_or_below, label, pattern};
use azad::frontend::{Note, NoteLevel};
use azad::ir::text;
use azad::log::Stream;
use azad::orchestrator;
use azad::{Engine, EngineError, Event, Goal, Progress, RunReport, Severity, Status, host_platform};

use crate::output::{self, Format, Summary, Verbosity};

const LABEL_PREFIX: &str = "//";
const NO_COLOR_ENV: &str = "NO_COLOR";
const GOAL_SEPARATOR: char = ':';
const PATH_SEPARATOR: char = '/';
const CURRENT_DIR: &str = ".";

/// The target that the engine reads for a target on the command line. The command line is relative to the current
/// directory, and the engine to the workspace root: a label stays as it is, a file becomes absolute, and a short goal
/// name becomes the label of the goal in the package of its directory.
fn target_for_engine(engine: &Engine, target: &str) -> Result<String> {
    if target.starts_with(LABEL_PREFIX) {
        return Ok(target.to_owned());
    }
    if let Some((dir, goal)) = short_goal(engine, target) {
        return goal_in_package(engine, dir, goal);
    }
    Ok(absolute_file(target))
}

/// A short goal name, and the directory whose package has the goal: `:check`, and `check` when it is not a file and
/// no frontend reads it, are in the current directory; `<dir>:check` is in the directory `<dir>`.
fn short_goal<'a>(engine: &Engine, target: &'a str) -> Option<(&'a Path, &'a str)> {
    match target.rsplit_once(GOAL_SEPARATOR) {
        Some(("", goal)) => Some((Path::new(CURRENT_DIR), goal)),
        Some((dir, goal)) if Path::new(dir).is_dir() => Some((Path::new(dir), goal)),
        Some(_) => None,
        None if is_bare_goal_name(engine, target) => Some((Path::new(CURRENT_DIR), target)),
        None => None,
    }
}

fn is_bare_goal_name(engine: &Engine, target: &str) -> bool {
    !target.contains(PATH_SEPARATOR) && !Path::new(target).is_file() && !engine.accepts(target)
}

/// The target of a goal name in a directory (decision 0023): the goal of every package at or below the directory, as
/// the pattern `//<dir>/...:<goal>`; the goal of that one package when it is the only one; and when no package is at or
/// below it, the goal of the package that holds it, the nearest directory above it with a tasks.star.
fn goal_in_package(engine: &Engine, dir: &Path, goal: &str) -> Result<String> {
    let root = &engine.config().root;
    let relative = relative_dir(root, dir);
    let below: Vec<String> = match &relative {
        Some(relative) => engine.packages()?.into_iter().filter(|package| is_at_or_below(package, relative)).collect(),
        None => Vec::new(),
    };
    match (below.as_slice(), &relative) {
        ([only], Some(relative)) if only == relative => return Ok(label(only, goal)),
        ([_, ..], Some(relative)) => return Ok(pattern(relative, goal)),
        _ => {}
    }
    if let Some(package) = package_of(root, dir) {
        return Ok(label(&package, goal));
    }
    let shown = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_owned());
    let problem = format!(
        "the goal {goal:?} has no package: no directory from {} up to the workspace root {} holds a {TASKS_FILE}",
        shown.display(),
        root.display()
    );
    Err(with_listing(engine, problem))
}

/// The directory relative to the workspace root, `""` for the root; `None` when it is not inside the root.
fn relative_dir(root: &Path, dir: &Path) -> Option<String> {
    let root = fs::canonicalize(root).ok()?;
    let dir = fs::canonicalize(dir).ok()?;
    dir.strip_prefix(&root).ok().map(|relative| relative.display().to_string())
}

/// A file target, `<file>` or `<file>:<goal>`, with the file as an absolute path when it exists.
fn absolute_file(target: &str) -> String {
    let (file, goal) = match target.rsplit_once(GOAL_SEPARATOR) {
        Some((file, goal)) if Path::new(file).exists() => (file, Some(goal)),
        _ => (target, None),
    };
    let Ok(absolute) = fs::canonicalize(file) else { return target.to_owned() };
    match goal {
        Some(goal) => format!("{}:{goal}", absolute.display()),
        None => absolute.display().to_string(),
    }
}

fn load(engine: &Engine, target: &str, verbosity: Verbosity) -> Result<Goal> {
    engine
        .load(&target_for_engine(engine, target)?, &mut |event| report_load(&event, verbosity))
        .map_err(|error| with_targets(engine, error))
}

/// An error about a target that no frontend reads, with the goals that exist.
fn with_targets(engine: &Engine, error: EngineError) -> anyhow::Error {
    if !matches!(error, EngineError::NoFrontend(_)) {
        return error.into();
    }
    with_listing(engine, error.to_string())
}

/// A problem with a target, and the goals of the workspace when it has any.
fn with_listing(engine: &Engine, problem: String) -> anyhow::Error {
    match engine.targets() {
        Ok(targets) if !targets.is_empty() => {
            anyhow::anyhow!("{problem}\nThe goals of this workspace:\n{}", listing(&targets))
        }
        _ => anyhow::anyhow!(problem),
    }
}

/// The exit code of a command that was used wrongly, as clap gives for a missing argument.
const USAGE_EXIT_CODE: u8 = 2;

pub(crate) fn list(engine: &Engine) -> Result<ExitCode> {
    let targets = engine.targets()?;
    if targets.is_empty() {
        bail!("{} has no goals: add a tasks.star, or a workflow in .github/workflows", engine.config().root.display());
    }
    print!("{}", listing(&targets));
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn no_target(engine: &Engine) -> Result<ExitCode> {
    eprintln!("azad run needs a goal. The goals of {}:", engine.config().root.display());
    eprint!("{}", listing(&engine.targets()?));
    eprintln!("Run one with: azad run <goal>");
    Ok(ExitCode::from(USAGE_EXIT_CODE))
}

fn listing(targets: &[Listed]) -> String {
    let width = targets.iter().map(|t| t.target.len()).max().unwrap_or_default();
    targets
        .iter()
        .map(|listed| {
            let line = match (&listed.problem, listed.doc.lines().next()) {
                (Some(problem), _) => format!("  {:width$}  cannot be read: {problem}", listed.target),
                (None, Some(doc)) => format!("  {:width$}  {doc}", listed.target),
                (None, None) => format!("  {:width$}", listed.target),
            };
            match listed.tags.is_empty() {
                true => format!("{}\n", line.trim_end()),
                false => format!("{line}  [{}]\n", listed.tags.iter().cloned().collect::<Vec<_>>().join(", ")),
            }
        })
        .collect()
}

pub(crate) fn run(
    engine: &Engine,
    target: &str,
    jobs: usize,
    format: Format,
    verbosity: Verbosity,
    report_file: Option<PathBuf>,
    replay: bool,
) -> Result<ExitCode> {
    let start = Instant::now();
    let mut renderer = output::renderer(format, verbosity, replay);
    let target = target_for_engine(engine, target)?;
    let (goal, report) = output::drive(renderer.as_mut(), start, |emit| -> Result<_> {
        let goal = engine.load(&target, &mut *emit).map_err(|error| with_targets(engine, error))?;
        let mut forward = |event: Event| {
            if replay {
                replay_cached(engine, &event, &mut *emit);
            }
            emit(event);
        };
        let report = engine.run(&goal, jobs, &mut forward)?;
        Ok((goal, report))
    })??;
    let hint = cached_output_hint(&goal.key, &report).filter(|_| !replay && output::is_for_people(format));
    if let Some(text) = hint {
        renderer.event(start.elapsed(), &Event::Note(Note { level: NoteLevel::Normal, text }));
    }
    let summary = Summary::from_report(&goal.key, &report);
    renderer.finish(start.elapsed(), &summary);
    if let Some(path) = report_file {
        fs::write(&path, summary.to_json()).with_context(|| path.display().to_string())?;
    }
    match report.outcome.goal_holds() {
        true => Ok(ExitCode::SUCCESS),
        false => Ok(ExitCode::FAILURE),
    }
}

pub(crate) fn why(
    engine: &Engine,
    target: &str,
    action: Option<&str>,
    next: bool,
    verbosity: Verbosity,
) -> Result<ExitCode> {
    let goal = load(engine, target, verbosity)?;
    let action = chosen_action(engine, &goal, action)?;
    let explained = engine.explain(&goal, &action, next)?;
    match explained.run {
        Some(number) => println!("In run {number} of {}:", goal.key),
        None => println!("If {} ran now:", goal.key),
    }
    println!("{}", explained.explanation);
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn log(engine: &Engine, target: &str, action: Option<&str>, verbosity: Verbosity) -> Result<ExitCode> {
    let goal = load(engine, target, verbosity)?;
    let action = chosen_action(engine, &goal, action)?;
    let Some((number, result)) = engine.last_result(&goal, &action)? else {
        bail!("{action} has no output in the last run of {}: it did not run", goal.key);
    };
    eprintln!("{action} in run {number} of {}, exit {}:", goal.key, result.exit_code);
    let keep_escapes = io::stdout().is_terminal() && std::env::var_os(NO_COLOR_ENV).is_none();
    let shown = |bytes: Vec<u8>| match keep_escapes {
        true => bytes,
        false => output::without_escapes(&String::from_utf8_lossy(&bytes)).into_bytes(),
    };
    match &result.log {
        Some(log) => io::stdout().write_all(&shown(engine.read_blob(log)?))?,
        None => {
            io::stdout().write_all(&shown(engine.read_blob(&result.stdout)?))?;
            io::stderr().write_all(&shown(engine.read_blob(&result.stderr)?))?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Before a cache hit, what its action wrote when it ran, as lines of its output, so that the view shows them as it
/// shows the output of an action that runs.
fn replay_cached(engine: &Engine, event: &Event, emit: &mut dyn FnMut(Event)) {
    let Event::Progress(Progress::Done { name, status: Status::Cached { result, .. } }) = event else { return };
    if !result.printed() {
        return;
    }
    match engine.output_of(result) {
        Ok(output) => {
            for line in output.lines() {
                let line = line.to_owned();
                emit(Event::Progress(Progress::Log { name: name.clone(), stream: Stream::Stdout, line }));
            }
        }
        Err(error) => emit(Event::Warning(format!("the output of {name} cannot be read: {error}"))),
    }
}

/// A note for the cache hits whose actions wrote output when they ran, which the run did not show.
fn cached_output_hint(goal: &str, report: &RunReport) -> Option<String> {
    let printed = report
        .outcome
        .statuses
        .values()
        .filter(|status| matches!(status, Status::Cached { result, .. } if result.printed()))
        .count();
    let (actions, they) = match printed {
        0 => return None,
        1 => ("1 cached action".to_owned(), "it"),
        n => (format!("{n} cached actions"), "they"),
    };
    Some(format!(
        "{actions} wrote output when {they} ran: `--replay` shows it, and `azad log {goal} <action>` shows the \
         output of one"
    ))
}

/// The action that the user named; or, when they named none, the one that failed in the last run, or the goal's only
/// action.
fn chosen_action(engine: &Engine, goal: &Goal, named: Option<&str>) -> Result<String> {
    if let Some(named) = named {
        return Ok(named.to_owned());
    }
    let statuses = engine.last_run(goal)?.map(|(_, outcome)| outcome.statuses).unwrap_or_default();
    let failed: Vec<&String> =
        statuses.iter().filter(|(_, status)| matches!(status, Status::Failed { .. })).map(|(key, _)| key).collect();
    if let [only] = failed.as_slice() {
        return Ok((*only).clone());
    }
    let required =
        goal.graph.goals.get(&goal.key).map(|actions| actions.iter().collect::<Vec<_>>()).unwrap_or_default();
    if let ([only], []) = (required.as_slice(), failed.as_slice()) {
        return Ok((*only).clone());
    }
    let mut lines: Vec<String> =
        statuses.iter().map(|(key, status)| format!("  {key}  {}", status_word(status))).collect();
    if lines.is_empty() {
        lines = goal.graph.actions.keys().map(|key| format!("  {key}")).collect();
    }
    bail!("name an action of {}:\n{}", goal.key, lines.join("\n"))
}

fn status_word(status: &Status) -> &'static str {
    match status {
        Status::NotNeeded => "not needed",
        Status::Ran { .. } => "ran",
        Status::Cached { .. } => "cached",
        Status::Failed { .. } => "failed",
        Status::Skipped => "skipped",
        Status::WillRun { .. } => "will run",
        Status::Waiting => "waiting",
    }
}

pub(crate) fn check(engine: &Engine, target: &str) -> Result<ExitCode> {
    let findings = engine.check(&target_for_engine(engine, target)?)?;
    for finding in &findings {
        println!("{:<11} {}: {}", finding.severity.to_string(), finding.location, finding.message);
    }
    let blocked = findings.iter().filter(|f| f.severity == Severity::Unsupported).count();
    match blocked {
        0 => {
            println!("{target} can run ({} notes)", findings.len());
            Ok(ExitCode::SUCCESS)
        }
        n => {
            println!("{target} cannot run: {n} unsupported");
            Ok(ExitCode::FAILURE)
        }
    }
}

pub(crate) fn ir(engine: &Engine, target: &str, verbosity: Verbosity) -> Result<ExitCode> {
    let goal = load(engine, target, verbosity)?;
    print!("{}", text::print(&goal.graph));
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn exec(engine: &Engine, file: &Path, name: &str) -> Result<ExitCode> {
    let absolute: PathBuf = std::fs::canonicalize(file).with_context(|| format!("cannot read {}", file.display()))?;
    let graph = engine.load_text(&absolute)?;
    let Some(definition) = graph.actions.get(name) else {
        bail!("{} has no action {name}", file.display());
    };
    let action = match definition.resolve(|_, _| None) {
        Ok(action) => action,
        Err(source) => bail!("action {name} reads {source:?}; run a goal for a graph"),
    };
    let (result, cached) = engine.execute(&action)?;
    let keep_escapes = io::stdout().is_terminal() && std::env::var_os(NO_COLOR_ENV).is_none();
    let shown = |bytes: Vec<u8>| match keep_escapes {
        true => bytes,
        false => output::without_escapes(&String::from_utf8_lossy(&bytes)).into_bytes(),
    };
    match &result.log {
        Some(log) => io::stdout().write_all(&shown(engine.read_blob(log)?))?,
        None => {
            io::stdout().write_all(&shown(engine.read_blob(&result.stdout)?))?;
            io::stderr().write_all(&shown(engine.read_blob(&result.stderr)?))?;
        }
    }
    let how = match cached {
        true => "cached",
        false => "ran",
    };
    eprintln!("{name}: exit {} ({how})", result.exit_code);
    for (path, tree) in &result.outputs {
        eprintln!("  output {path}: {tree}");
    }
    match result.succeeded() {
        true => Ok(ExitCode::SUCCESS),
        false => Ok(ExitCode::FAILURE),
    }
}

const MEBIBYTE: u64 = 1024 * 1024;

pub(crate) fn cache_list(engine: &Engine) -> Result<ExitCode> {
    let caches = engine.store().caches()?;
    if caches.is_empty() {
        println!("no cache volumes");
    }
    for cache in caches {
        let age = cache
            .last_used
            .and_then(|t| t.elapsed().ok())
            .map(|d| format!("{}m ago", d.as_secs() / 60))
            .unwrap_or_else(|| "never".to_owned());
        println!("{:<24} {:<14} {:>8} MiB  used {age}", cache.name, cache.platform, cache.bytes / MEBIBYTE);
    }
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn cache_clear(engine: &Engine, name: Option<&str>) -> Result<ExitCode> {
    let removed = engine.store().clear_caches(name)?;
    println!("removed {removed} cache volumes");
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn ingest(engine: &Engine, dir: &Path) -> Result<ExitCode> {
    println!("{}", engine.ingest(dir)?);
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn pull(engine: &Engine, reference: &str, platform: Option<String>) -> Result<ExitCode> {
    let platform = platform.unwrap_or_else(host_platform);
    let pinned = engine.pin(reference)?;
    println!("{pinned}");
    println!("{}", engine.pull(&pinned, &platform)?.tree);
    Ok(ExitCode::SUCCESS)
}

/// The events of loading a goal, for commands that do not run it.
fn report_load(event: &Event, verbosity: Verbosity) {
    if !verbosity.shows(event) {
        return;
    }
    match event {
        Event::Pull { reference } => eprintln!("pull    {reference}"),
        Event::Fetch { url, rev } => eprintln!("fetch   {url}@{rev}"),
        Event::Warning(message) => eprintln!("warning: {message}"),
        Event::Note(note) => eprintln!("note: {}", note.text),
        Event::Progress(_) | Event::Remote(_) | Event::Timings(_) | Event::Loaded { .. } => {}
    }
}

/// Serves the executor on `socket`. A socket file that nothing listens on is left from an executor that stopped, and
/// is replaced.
pub(crate) fn serve_executor(config: &azad::EngineConfig, socket: &Path) -> Result<ExitCode> {
    let service = Engine::executor_service(config)?;
    if socket.exists() {
        if UnixStream::connect(socket).is_ok() {
            bail!("an executor listens on {} already", socket.display());
        }
        fs::remove_file(socket).with_context(|| format!("removing the old socket {}", socket.display()))?;
    }
    let listener = UnixListener::bind(socket).with_context(|| format!("listening on {}", socket.display()))?;
    eprintln!("azad executor: listening on {}", socket.display());
    service.serve(&listener).with_context(|| format!("serving on {}", socket.display()))?;
    Ok(ExitCode::SUCCESS)
}

const WEBHOOK_SECRET_ENV: &str = "AZAD_WEBHOOK_SECRET";

fn webhook_secret() -> Result<Vec<u8>> {
    match std::env::var_os(WEBHOOK_SECRET_ENV).filter(|secret| !secret.is_empty()) {
        Some(secret) => Ok(secret.into_encoded_bytes()),
        None => bail!("set {WEBHOOK_SECRET_ENV} to the secret that signs webhook deliveries"),
    }
}

pub(crate) fn serve(config: azad::EngineConfig, listen: &str) -> Result<ExitCode> {
    let secret = webhook_secret()?;
    let listener = std::net::TcpListener::bind(listen).with_context(|| format!("listening on {listen}"))?;
    eprintln!("azad serve: webhooks to http://{}{}", listener.local_addr()?, orchestrator::WEBHOOK_PATH);
    orchestrator::Orchestrator::new(config, &secret).serve(&listener)?;
    Ok(ExitCode::SUCCESS)
}

fn simulator(config: &azad::EngineConfig, server: &str, default_branch: &str) -> Result<orchestrator::Simulator> {
    Ok(orchestrator::Simulator::new(&config.root, server, &webhook_secret()?, default_branch)?)
}

/// The branch to send: the one named, or the one that is checked out.
fn branch_or_current(config: &azad::EngineConfig, branch: Option<&str>) -> Result<String> {
    if let Some(branch) = branch {
        return Ok(branch.to_owned());
    }
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(&config.root)
        .args(["symbolic-ref", "--short", "HEAD"])
        .output()?;
    if !output.status.success() {
        bail!("no branch is checked out in {}; name one", config.root.display());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

pub(crate) fn simulate_push(
    config: &azad::EngineConfig,
    server: &str,
    default_branch: &str,
    branch: Option<&str>,
) -> Result<ExitCode> {
    let branch = branch_or_current(config, branch)?;
    let answer = simulator(config, server, default_branch)?.push(&branch)?;
    eprintln!("azad: pushed {branch}: {answer}");
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn simulate_pull_request(
    config: &azad::EngineConfig,
    server: &str,
    default_branch: &str,
    number: u64,
    branch: Option<&str>,
) -> Result<ExitCode> {
    let branch = branch_or_current(config, branch)?;
    let answer = simulator(config, server, default_branch)?.pull_request(number, &branch)?;
    eprintln!("azad: pull request {number} from {branch}: {answer}");
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn checks(config: &azad::EngineConfig) -> Result<ExitCode> {
    let runs = orchestrator::fake_github(&config.store).check_runs()?;
    if runs.is_empty() {
        eprintln!("No check runs yet; `azad serve` writes them.");
    }
    for run in runs {
        println!("{run}");
    }
    Ok(ExitCode::SUCCESS)
}
