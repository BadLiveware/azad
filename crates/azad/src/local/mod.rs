//! The local backend: runs one resolved action on this machine. It looks up the cache, stages the inputs, runs the
//! sandbox, and collects the outputs.

use crate::log::{Discard, LogSink};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use crate::ir::action::Container;
use crate::ir::{Action, ActionResult, Canonical, Digest, Kind, WORK_DIR};
use crate::sandbox::{
    ContainerRun, Exit, HostSpec, Limits, Mount, RUNTIME_ENV, Sandbox, SandboxError, SandboxSpec, Volume, find_runtime,
    run_on_host,
};
use crate::store::{ActionCache, CacheVolume, Selection, Store, StoreError};
use crate::timings::{Timings, tree_name};
use crate::trust::Trust;

const WORK: &str = "work";
const TMP: &str = "tmp";
const STDOUT: &str = "stdout";
const STDERR: &str = "stderr";
const LOG: &str = "log";

#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Sandbox(#[from] SandboxError),
    #[error("the action ran longer than its limit of {0:?}")]
    TimedOut(std::time::Duration),
    #[error("the action did not make its declared output {0:?}")]
    MissingOutput(String),
    #[error("output {path:?} has the digest {actual}, but the action expects {expected}")]
    OutputMismatch { path: String, expected: Digest, actual: Digest },
    #[error("input path {0:?} exists in the root image; the input would hide it")]
    InputHidesRoot(String),
    #[error("{path}: {error}")]
    Io { path: std::path::PathBuf, error: std::io::Error },
    #[error(
        "the action runs in its image (`local = \"image\"`), which needs Docker or Podman, and neither is on PATH; \
         install one, name it in ${RUNTIME_ENV}, or run on this host's tools with `azad run --local host`"
    )]
    NoContainerRuntime,
    #[error("the action needs the secret {0}, which is not set in the environment of azad")]
    MissingSecret(String),
    #[error("the action names {0} as a secret, which is azad's own and no action gets")]
    WithheldSecret(String),
}

/// The result of an action, and whether it came from the cache.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Execution {
    pub action: Digest,
    pub result: ActionResult,
    pub cached: bool,
    /// Paths that the action left in the work directory but did not declare as outputs. Azad discards them, and
    /// reports them so that the author can declare them or write them elsewhere.
    pub discarded: Vec<String>,
}

pub struct LocalBackend {
    store: Store,
    cache: ActionCache,
    sandbox: Box<dyn Sandbox>,
    limits: Limits,
    presentation: BTreeMap<String, String>,
    timings: Timings,
    trust: Trust,
    /// Variables of this process that host actions do not get, such as Azad's own secrets.
    withheld: BTreeSet<String>,
}

/// Variables that ask tools for colored output. They change only how the output looks, so they are given to each
/// command when it runs, and are not part of the action.
/// The hexadecimal digits of the action's digest in the name of its container: unique among the running ones.
const CONTAINER_NAME_DIGITS: usize = 16;

const COLOR_VARIABLES: [(&str, &str); 3] =
    [("CARGO_TERM_COLOR", "always"), ("FORCE_COLOR", "1"), ("CLICOLOR_FORCE", "1")];

impl std::fmt::Debug for LocalBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalBackend").field("store", &self.store).field("limits", &self.limits).finish_non_exhaustive()
    }
}

impl LocalBackend {
    pub fn new(store: Store, sandbox: Box<dyn Sandbox>, limits: Limits) -> Result<Self, ExecError> {
        let cache = store.state().action_cache(Trust::default());
        Ok(LocalBackend {
            store,
            cache,
            sandbox,
            limits,
            presentation: BTreeMap::new(),
            timings: Timings::default(),
            trust: Trust::default(),
            withheld: BTreeSet::new(),
        })
    }

    /// Asks the tools of each command for colored output, for a terminal that shows it.
    pub fn with_color(mut self) -> Self {
        self.presentation.extend(COLOR_VARIABLES.iter().map(|(name, value)| ((*name).to_owned(), (*value).to_owned())));
        self
    }

    /// Records how long it takes to place the files of each action in `timings`.
    pub fn with_timings(mut self, timings: Timings) -> Self {
        self.timings = timings;
        self
    }

    /// Runs actions at a trust level, which chooses the cache volumes that they use and the action cache that they
    /// read and write.
    /// Keeps the variables `names` of this process from host actions: Azad's own secrets, which their code must not
    /// read.
    pub fn with_withheld(mut self, names: BTreeSet<String>) -> Self {
        self.withheld = names;
        self
    }

    pub fn with_trust(mut self, trust: Trust) -> Self {
        self.trust = trust;
        self.cache = self.store.state().action_cache(trust);
        self
    }

    /// The environment of a command: the action's, with the variables of presentation that the action does not set.
    fn command_env(&self, action: &Action) -> BTreeMap<String, String> {
        let mut env = self.presentation.clone();
        env.extend(action.env.iter().map(|(name, value)| (name.clone(), value.clone())));
        env
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// The cached result of an action, without running it.
    pub fn cached(&self, action: &Action) -> Result<Option<ActionResult>, ExecError> {
        Ok(cached_result(&self.store, &self.cache, action)?)
    }

    /// Runs an action, unless the cache knows its result. A failed run is returned, not cached.
    pub fn execute(&self, action: &Action) -> Result<Execution, ExecError> {
        let digest = action.digest();
        if let Some(result) = self.cached(action)? {
            return Ok(Execution { action: digest, result, cached: true, discarded: Vec::new() });
        }
        self.execute_uncached(action, &Discard)
    }

    /// Runs an action without a cache lookup, gives each line of its output to `logs` while it runs, and caches a
    /// successful result.
    pub fn execute_uncached(&self, action: &Action, logs: &dyn LogSink) -> Result<Execution, ExecError> {
        let execution = self.run_uncached(action, logs)?;
        if execution.result.succeeded() && action.kind.is_cacheable() {
            self.cache.put_result(&execution.action, &execution.result)?;
        }
        Ok(execution)
    }

    /// Runs an action without a cache lookup and without caching its result, which is the client's to decide when an
    /// executor service runs it.
    pub fn run_uncached(&self, action: &Action, logs: &dyn LogSink) -> Result<Execution, ExecError> {
        let (result, discarded) = self.run(action, logs)?;
        Ok(Execution { action: action.digest(), result, cached: false, discarded })
    }

    fn shared_inputs(&self, action: &Action) -> Result<Vec<Mount>, StoreError> {
        action
            .inputs
            .iter()
            .map(|(target, tree)| Ok(Mount { host: self.store.materialize(tree)?, target: target.clone() }))
            .collect()
    }

    /// The inputs of an action with cache volumes, from checkouts in the first volume (see `store::checkout`). When a
    /// volume's last run read other checkouts, each file is written again, so that its time is newer than that
    /// volume's state.
    fn checked_out_inputs<'a>(
        &self,
        action: &Action,
        holder: &CacheVolume,
        volumes: impl Iterator<Item = &'a CacheVolume> + Clone,
    ) -> Result<Vec<Mount>, StoreError> {
        let rewrite = volumes.clone().any(|volume| !volume.had_inputs_from(holder));
        let mut mounts = Vec::new();
        for (target, tree) in &action.inputs {
            let dest = holder.inputs_dir(target);
            if rewrite {
                self.store.forget_checkout(&dest)?;
            }
            self.store.check_out(tree, &dest)?;
            mounts.push(Mount { host: dest, target: target.clone() });
        }
        for volume in volumes {
            volume.set_inputs_from(holder)?;
        }
        Ok(mounts)
    }

    fn run(&self, action: &Action, logs: &dyn LogSink) -> Result<(ActionResult, Vec<String>), ExecError> {
        if let Kind::Host { workspace, .. } = &action.kind {
            return self.run_on_host(action, Path::new(workspace), logs);
        }
        let scratch = self.store.scratch_dir()?;
        let work = scratch.path().join(WORK);
        let tmp = scratch.path().join(TMP);
        for dir in [&work, &tmp] {
            fs::create_dir(dir).map_err(|source| ExecError::Io { path: dir.clone(), error: source })?;
        }
        for (path, tree) in &action.work {
            self.store.merge_into(tree, &work.join(path))?;
        }
        let placed = relative_paths(&work)?;
        let timer = self.timings.start(format!("place the files of the root, {}", tree_name(&action.root)));
        let root = self.store.materialize(&action.root)?;
        timer.stop();
        if let Some(path) = action.inputs.keys().find(|path| exists_in(&root, path)) {
            return Err(ExecError::InputHidesRoot(path.clone()));
        }
        let volumes = action
            .caches
            .iter()
            .map(|(target, name)| Ok((target.clone(), self.store.cache_volume(name, &action.platform, self.trust)?)))
            .collect::<Result<Vec<_>, StoreError>>()?;
        let timer = self.timings.start("place the files of the inputs");
        let mounts = match volumes.first() {
            None => self.shared_inputs(action)?,
            Some((_, holder)) => self.checked_out_inputs(action, holder, volumes.iter().map(|(_, v)| v))?,
        };
        timer.stop();
        let sandbox_volumes = volumes
            .iter()
            .map(|(target, volume)| Volume {
                target: target.clone(),
                dir: volume.path().to_path_buf(),
                disk: volume.disk(),
            })
            .collect();
        let spec = SandboxSpec {
            root,
            mounts,
            work: work.clone(),
            dir: action.dir.clone(),
            tmp,
            volumes: sandbox_volumes,
            command: action.command.clone(),
            env: self.command_env(action).into_iter().chain(self.secrets(action)?).collect(),
            network: action.kind.has_network(),
            limits: self.limits.clone(),
            stdout: scratch.path().join(STDOUT),
            stderr: scratch.path().join(STDERR),
            log: scratch.path().join(LOG),
        };
        let exit = self.sandbox.run(&spec, logs)?;
        let (exit_code, stdout, stderr, log) = self.finished(exit, &spec.stdout, &spec.stderr, &spec.log)?;
        let mut kept = action.outputs.clone();
        kept.extend(
            action.caches.keys().filter_map(|path| path.strip_prefix(&format!("{WORK_DIR}/"))).map(str::to_owned),
        );
        let (outputs, discarded) = match exit_code == 0 {
            true => (self.collect_outputs(action, &work)?, undeclared_paths(&work, &kept, &placed)?),
            false => (BTreeMap::new(), Vec::new()),
        };
        let result = ActionResult { exit_code, outputs, stdout, stderr, log: Some(log) };
        Ok((result, discarded))
    }

    /// Runs a host action (decision 0015) in its workspace, with the host's tools and environment. Its outputs are the
    /// files that it wrote there; nothing is discarded, because the files are the developer's.
    fn run_on_host(
        &self,
        action: &Action,
        workspace: &Path,
        logs: &dyn LogSink,
    ) -> Result<(ActionResult, Vec<String>), ExecError> {
        let scratch = self.store.scratch_dir()?;
        let mut leases = Vec::new();
        let container = match &action.kind {
            Kind::Host { container: Some(container), .. } => {
                Some(self.container_run(action, container, workspace, &mut leases)?)
            }
            _ => None,
        };
        let spec = HostSpec {
            dir: workspace.join(&action.dir),
            command: action.command.clone(),
            env: self.command_env(action),
            withheld: self.withheld.clone(),
            container,
            limits: self.limits.clone(),
            stdout: scratch.path().join(STDOUT),
            stderr: scratch.path().join(STDERR),
            log: scratch.path().join(LOG),
        };
        let exit = run_on_host(&spec, logs)?;
        let (exit_code, stdout, stderr, log) = self.finished(exit, &spec.stdout, &spec.stderr, &spec.log)?;
        let outputs = match exit_code == 0 {
            true => self.collect_outputs(action, workspace)?,
            false => BTreeMap::new(),
        };
        Ok((ActionResult { exit_code, outputs, stdout, stderr, log: Some(log) }, Vec::new()))
    }

    /// How a host action with a container runs: the runtime, its cache volumes, which `leases` keeps while it runs,
    /// and its secrets.
    fn container_run(
        &self,
        action: &Action,
        container: &Container,
        workspace: &Path,
        leases: &mut Vec<CacheVolume>,
    ) -> Result<ContainerRun, ExecError> {
        let runtime = find_runtime().ok_or(ExecError::NoContainerRuntime)?;
        let mut volumes = Vec::new();
        for (path, name) in &container.volumes {
            let volume = self.store.cache_volume(name, &action.platform, self.trust)?;
            volumes.push((volume.path().to_path_buf(), path.clone()));
            leases.push(volume);
        }
        Ok(ContainerRun {
            runtime,
            image: container.image.clone(),
            workspace: workspace.to_path_buf(),
            volumes,
            secrets: self.secrets(action)?,
            name: format!("azad-{}", &action.digest().hex()[..CONTAINER_NAME_DIGITS]),
        })
    }

    /// The values of the action's secrets, from this process's environment; Azad's own secrets are never given.
    fn secrets(&self, action: &Action) -> Result<BTreeMap<String, String>, ExecError> {
        let mut values = BTreeMap::new();
        for name in &action.secrets {
            if self.withheld.contains(name) {
                return Err(ExecError::WithheldSecret(name.clone()));
            }
            let value = std::env::var(name).map_err(|_| ExecError::MissingSecret(name.clone()))?;
            values.insert(name.clone(), value);
        }
        Ok(values)
    }

    /// The exit code of a command that ended, and its stored output.
    fn finished(
        &self,
        exit: Exit,
        stdout: &Path,
        stderr: &Path,
        log: &Path,
    ) -> Result<(i64, Digest, Digest, Digest), ExecError> {
        let exit_code = match exit {
            Exit::Code(code) => i64::from(code),
            Exit::TimedOut => return Err(ExecError::TimedOut(self.limits.timeout)),
        };
        Ok((exit_code, self.store.put_file(stdout)?, self.store.put_file(stderr)?, self.store.put_file(log)?))
    }

    fn collect_outputs(&self, action: &Action, work: &Path) -> Result<BTreeMap<String, Digest>, ExecError> {
        let mut outputs = BTreeMap::new();
        for path in &action.outputs {
            let host = work.join(path);
            if !host.is_dir() {
                return Err(ExecError::MissingOutput(path.clone()));
            }
            outputs.insert(path.clone(), self.store.ingest(&host, &Selection::All)?);
        }
        if let Kind::FixedOutput { expected } = &action.kind {
            for (path, expected) in expected {
                let actual = outputs[path];
                if actual != *expected {
                    return Err(ExecError::OutputMismatch { path: path.clone(), expected: *expected, actual });
                }
            }
        }
        Ok(outputs)
    }
}

/// The cached result of an action, without running it. A host action's result counts only while the workspace still
/// holds what it wrote there; when a file was changed or removed since, the action runs again.
pub fn cached_result(store: &Store, cache: &ActionCache, action: &Action) -> Result<Option<ActionResult>, StoreError> {
    if !action.kind.is_cacheable() {
        return Ok(None);
    }
    let Some(result) = cache.result(&action.digest())? else { return Ok(None) };
    match &action.kind {
        Kind::Host { workspace, .. } if !outputs_in_place(store, Path::new(workspace), &result)? => Ok(None),
        _ => Ok(Some(result)),
    }
}

fn outputs_in_place(store: &Store, workspace: &Path, result: &ActionResult) -> Result<bool, StoreError> {
    for (path, digest) in &result.outputs {
        let host = workspace.join(path);
        if !host.is_dir() || store.ingest(&host, &Selection::All)? != *digest {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The entries of the work directory that are neither a declared output nor a directory on the way to one, and that
/// the action made: entries that `placed` names were there when it started. An undeclared directory is reported once,
/// not with its content.
fn undeclared_paths(
    work: &Path,
    outputs: &BTreeSet<String>,
    placed: &BTreeSet<String>,
) -> Result<Vec<String>, ExecError> {
    let mut found = Vec::new();
    collect_undeclared(work, "", outputs, placed, &mut found)?;
    found.sort();
    Ok(found)
}

/// Every path below `dir`, relative to it.
fn relative_paths(dir: &Path) -> Result<BTreeSet<String>, ExecError> {
    let mut found = BTreeSet::new();
    collect_relative(dir, "", &mut found)?;
    Ok(found)
}

fn collect_relative(dir: &Path, prefix: &str, found: &mut BTreeSet<String>) -> Result<(), ExecError> {
    let io_error = |source| ExecError::Io { path: dir.to_owned(), error: source };
    for entry in fs::read_dir(dir).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        let relative = join_relative(prefix, &entry.file_name().to_string_lossy());
        if entry.file_type().map_err(io_error)?.is_dir() {
            collect_relative(&entry.path(), &relative, found)?;
        }
        found.insert(relative);
    }
    Ok(())
}

fn join_relative(prefix: &str, name: &str) -> String {
    match prefix.is_empty() {
        true => name.to_owned(),
        false => format!("{prefix}/{name}"),
    }
}

fn collect_undeclared(
    dir: &Path,
    prefix: &str,
    outputs: &BTreeSet<String>,
    placed: &BTreeSet<String>,
    found: &mut Vec<String>,
) -> Result<(), ExecError> {
    let io_error = |source| ExecError::Io { path: dir.to_owned(), error: source };
    for entry in fs::read_dir(dir).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        let relative = join_relative(prefix, &entry.file_name().to_string_lossy());
        if outputs.contains(&relative) {
            continue;
        }
        let leads_to_output = outputs.iter().any(|o| o.starts_with(&format!("{relative}/")));
        let is_dir = entry.file_type().map_err(io_error)?.is_dir();
        let was_placed = placed.contains(&relative);
        match (is_dir && (leads_to_output || was_placed), was_placed) {
            (true, _) => collect_undeclared(&entry.path(), &relative, outputs, placed, found)?,
            (false, true) => {}
            (false, false) => found.push(relative),
        }
    }
    Ok(())
}

/// Whether an absolute sandbox path names an entry of the root image, without following its last component.
fn exists_in(root: &Path, path: &str) -> bool {
    fs::symlink_metadata(root.join(path.trim_start_matches('/'))).is_ok()
}

impl crate::scheduler::Backend for LocalBackend {
    fn cached(&self, action: &Action) -> Result<Option<ActionResult>, crate::scheduler::BackendError> {
        Ok(LocalBackend::cached(self, action)?)
    }

    fn execute(
        &self,
        action: &Action,
        logs: &dyn LogSink,
    ) -> Result<crate::scheduler::BackendExecution, crate::scheduler::BackendError> {
        let execution = self.execute_uncached(action, logs)?;
        Ok(crate::scheduler::BackendExecution { result: execution.result, discarded: execution.discarded })
    }
}
