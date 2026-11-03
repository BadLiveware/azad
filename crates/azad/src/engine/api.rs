use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use crate::frontend::starlark::{Context, expand_calls};
use crate::remote::{RemoteBackend, RemoteCache, RemoteConfig, RemoteSummary};
use std::path::Path;

use crate::executor::{ExecutorClient, ExecutorService, ServiceSandbox};
use crate::frontend::gha::GhaFrontend;
use crate::frontend::starlark::StarlarkFrontend;
use crate::frontend::starlark::{has_tasks_file, is_tasks_file};
use crate::frontend::{
    Dependencies, FileSelection, Finding, Frontend, Listed, LoadRequest, Note, NoteLevel, Package, Severity,
    SourceSpec, WorkspaceView, is_at_or_below, label, parse_pattern, split_dependency,
};
use crate::ir::{Action, ActionResult, Canonical, Digest, Graph, Image, Kind};
use crate::local::LocalBackend;
use crate::oci::Puller;
use crate::sandbox::{Bubblewrap, MicroVm, Sandbox};
use crate::scheduler::{Backend, Cause, Explanation, Outcome, Progress, Runner, Status};
use crate::store::{Selection, Store};
use crate::timings::{Step, Timings};

use crate::engine::resolve::Resolver;
use crate::engine::why::{self, KeepActions};

const TOOLS_LOCATION: &str = "tools";
/// Isolated runs of a goal have their own history, because their actions differ from those of host runs.
const ISOLATED_HISTORY: &str = "isolated";
use crate::engine::config::vm_config;
use crate::engine::missing::{Mentions, Workspace};
use crate::engine::tasks::DependencySources;
use crate::engine::{EngineConfig, EngineError, Mode, SandboxKind, TextFrontend};

/// What the engine reports while it works.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// An image is downloaded, because the store does not have it.
    Pull {
        reference: String,
    },
    /// A repository is fetched at a revision, because the store does not have that commit.
    Fetch {
        url: String,
        rev: String,
    },
    Progress(Progress),
    /// What the remote cache did in a run, after the run.
    Remote(RemoteSummary),
    /// The steps of a load and a run that are not commands and took long enough to note (see `timings`), after the run.
    Timings(Vec<Step>),
    /// A goal is loaded. Titles are the names to show for actions whose label does not say enough (see
    /// `Package::titles`).
    Loaded {
        titles: BTreeMap<String, String>,
        /// Where the actions that see the workspace see it, by label (see `Package::workspace_views`).
        workspace_views: BTreeMap<String, WorkspaceView>,
    },
    /// Something that is likely a mistake, but does not stop the work, such as a glob that matches no file.
    Warning(String),
    /// What was decided, and why, such as the source of a fact (decision 0022).
    Note(Note),
}

/// What evaluation can use beyond the workspace's files, from the engine's config.
fn context(config: &EngineConfig) -> Context {
    Context { history: config.history.clone() }
}

/// A loaded goal: its graph, checked, with every source stored.
#[derive(Clone, Debug)]
pub struct Goal {
    pub graph: Graph,
    /// The key of the goal in `graph.goals`.
    pub key: String,
    /// The names to show for actions, by label (see `Package::titles`).
    pub titles: BTreeMap<String, String>,
    scope: Option<String>,
    /// The target that named the goal, as the engine read it.
    target: String,
    /// The tasks that have an image and run with the host's tools in this run.
    host_tools: BTreeSet<String>,
}

impl Goal {
    /// The key of an action: as given, or as a name in the package of the goal.
    pub fn action_key(&self, name: &str) -> Option<String> {
        if self.graph.actions.contains_key(name) {
            return Some(name.to_owned());
        }
        let candidate = label(self.scope.as_deref()?, name.trim_start_matches(':'));
        self.graph.actions.contains_key(&candidate).then_some(candidate)
    }
}

/// A finished run and its number among the runs of its goal.
#[derive(Clone, Debug)]
pub struct RunReport {
    pub outcome: Outcome,
    pub number: u64,
}

/// Explains an action: from a recorded run, or from the next run.
#[derive(Clone, Debug)]
pub struct Explained {
    /// The recorded run, or `None` for the next run.
    pub run: Option<u64>,
    pub explanation: Explanation,
}

pub struct Engine {
    config: EngineConfig,
    store: Store,
    puller: Puller,
    backend: Box<dyn Backend>,
    remote: Option<Arc<RemoteCache>>,
    frontends: Vec<Box<dyn Frontend>>,
    /// How long the steps of runs took that are not commands, shared by the backend, the remote cache and loads.
    timings: Timings,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let frontends: Vec<&str> = self.frontends.iter().map(|f| f.name()).collect();
        f.debug_struct("Engine").field("config", &self.config).field("frontends", &frontends).finish_non_exhaustive()
    }
}

impl Engine {
    /// An engine with the local backend and the built-in frontends: Starlark, the text form of the IR, and GHA.
    pub fn new(config: EngineConfig) -> Result<Self, EngineError> {
        let timings = Timings::default();
        let (store, local) = Engine::local(&config, &timings)?;
        Ok(Engine { timings, ..Engine::with_backend(config, store, local)? })
    }

    /// An engine like `new`, with a remote cache in front of the local backend (decision 0012).
    pub fn with_remote(config: EngineConfig, remote: RemoteConfig) -> Result<Self, EngineError> {
        let timings = Timings::default();
        let (store, local) = Engine::local(&config, &timings)?;
        let cache = Arc::new(RemoteCache::new(remote, store.clone()).with_timings(timings.clone()));
        let engine = Engine::with_backend(config, store, Box::new(RemoteBackend::new(local, cache.clone())))?;
        Ok(Engine { remote: Some(cache), timings, ..engine })
    }

    fn local(config: &EngineConfig, timings: &Timings) -> Result<(Store, Box<dyn Backend>), EngineError> {
        let store = Store::open(&config.store).map_err(EngineError::store(config.store.display().to_string()))?;
        if let Some(socket) = &config.executor {
            return Ok((store.clone(), Box::new(ExecutorClient::new(socket.clone(), store, config.trust))));
        }
        let sandbox: Box<dyn Sandbox> = match config.sandbox {
            SandboxKind::Bubblewrap => Box::new(Bubblewrap::default()),
            SandboxKind::Vm => Box::new(MicroVm::new(vm_config(config)?).with_timings(timings.clone())),
        };
        let mut backend = LocalBackend::new(store.clone(), sandbox, config.limits.clone())
            .map_err(|e| EngineError::Backend(Box::new(e)))?
            .with_timings(timings.clone())
            .with_trust(config.trust)
            .with_withheld(config.withheld.clone());
        if config.color {
            backend = backend.with_color();
        }
        Ok((store, Box::new(backend)))
    }

    /// An executor service with the store and the sandbox of `config`, which runs the actions of other engines.
    pub fn executor_service(config: &EngineConfig) -> Result<ExecutorService, EngineError> {
        let store = Store::open(&config.store).map_err(EngineError::store(config.store.display().to_string()))?;
        let sandbox = match config.sandbox {
            SandboxKind::Bubblewrap => ServiceSandbox::Bubblewrap,
            SandboxKind::Vm => ServiceSandbox::Vm(vm_config(config)?),
        };
        Ok(ExecutorService::new(store, sandbox, config.limits.clone()))
    }

    pub fn with_backend(config: EngineConfig, store: Store, backend: Box<dyn Backend>) -> Result<Self, EngineError> {
        let puller = Puller::new(store.clone())
            .map_err(|source| EngineError::Pull { reference: String::new(), error: source })?;
        let frontends: Vec<Box<dyn Frontend>> = vec![
            Box::new(StarlarkFrontend { context: context(&config) }),
            Box::new(TextFrontend),
            Box::new(GhaFrontend::default()),
        ];
        let backend = match config.force {
            true => Box::new(Forced(backend)),
            false => backend,
        };
        let backend = Box::new(KeepActions { inner: backend, store: store.clone() });
        Ok(Engine { config, store, puller, backend, remote: None, frontends, timings: Timings::default() })
    }

    /// Adds a frontend; frontends added later are asked first.
    pub fn with_frontend(mut self, frontend: Box<dyn Frontend>) -> Self {
        self.frontends.insert(0, frontend);
        self
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    /// Reads a target with the first frontend that accepts it, stores its sources, and checks its graph. A pattern,
    /// `//dir/...:name`, is the goal or task `name` of every package at or below `dir` (decision 0023).
    pub fn load(&self, target: &str, observe: &mut dyn FnMut(Event)) -> Result<Goal, EngineError> {
        if let Some((dir, name)) = parse_pattern(target) {
            return self.load_pattern(target, dir, name, observe);
        }
        let (package, goal, scope) = self.read(target, observe)?;
        let selected = self.select(package, &goal, observe)?;
        self.excluded_notes(target, &selected.excluded, observe)?;
        self.finish(selected.package, goal, scope, target, selected.host_tools, observe)
    }

    /// The package of a target, with its calls expanded, its notes given, and its goal: the target's goal, or a goal
    /// that requires only the target when it names a task or an action.
    fn read(
        &self,
        target: &str,
        observe: &mut dyn FnMut(Event),
    ) -> Result<(Package, String, Option<String>), EngineError> {
        let frontend = self.frontend(target)?;
        let request = LoadRequest { root: &self.config.root, target, platform: &self.config.platform };
        let loaded = frontend
            .load(&request)
            .map_err(|source| EngineError::Frontend { frontend: frontend.name(), error: source })?;
        let mut package =
            expand_calls(&context(&self.config), &self.config.root, &self.config.platform, loaded.package)
                .map_err(|e| EngineError::Frontend { frontend: "starlark", error: Box::new(e) })?;
        for note in std::mem::take(&mut package.notes) {
            observe(Event::Note(note));
        }
        let is_task_or_action = package.tasks.contains_key(&loaded.goal) || package.actions.contains_key(&loaded.goal);
        if !package.goals.contains_key(&loaded.goal) && is_task_or_action {
            package.goals.insert(loaded.goal.clone(), [loaded.goal.clone()].into());
        }
        if !package.goals.contains_key(&loaded.goal) {
            let known = package.goals.keys().cloned().collect();
            return Err(EngineError::UnknownGoal { goal: loaded.goal, known });
        }
        Ok((package, loaded.goal, loaded.scope))
    }

    /// The package with only what its goal needs, after the filter took the tasks and actions that the goal requires
    /// and whose tags it excludes; an error when it excludes all of them.
    fn select(
        &self,
        mut package: Package,
        goal: &str,
        observe: &mut dyn FnMut(Event),
    ) -> Result<Selected, EngineError> {
        let required = package.goals.get(goal).cloned().unwrap_or_default();
        let tags_of = |label: &String| package.tags.get(label).cloned().unwrap_or_default();
        let (kept, excluded): (BTreeSet<String>, BTreeSet<String>) =
            required.into_iter().partition(|label| self.config.filter.admits(&tags_of(label)));
        let excluded: Vec<(String, BTreeSet<String>)> =
            excluded.into_iter().map(|label| (label.clone(), tags_of(&label))).collect();
        if kept.is_empty() {
            return Err(self.all_excluded(goal, &package));
        }
        package.goals.insert(goal.to_owned(), kept);
        let dependencies = self.resolve_dependencies(&mut package, observe)?;
        let mode = self.mode();
        crate::engine::tasks::compile(&mut package, &mode, &dependencies)?;
        keep_only_what_the_goal_needs(&mut package, goal);
        let host_tools = package
            .tasks
            .iter()
            .filter(|(label, task)| package.actions.contains_key(*label) && mode.uses_host_tools_for_image(task))
            .map(|(label, _)| label.clone())
            .collect();
        Ok(Selected { package, excluded, host_tools })
    }

    /// The files that each task with `dependencies` reads, by label, with the tasks whose outputs it needs added to its
    /// `deps`; and, in the package, the tasks of other packages that its tasks need, until no task needs another
    /// (decisions 0026 and 0028).
    fn resolve_dependencies(
        &self,
        package: &mut Package,
        observe: &mut dyn FnMut(Event),
    ) -> Result<DependencySources, EngineError> {
        let mut resolved = DependencySources::new();
        let mut packages = BTreeMap::new();
        loop {
            let pending: Vec<String> = package
                .tasks
                .iter()
                .filter(|(label, task)| task.dependencies != Dependencies::None && !resolved.contains_key(*label))
                .map(|(label, _)| label.clone())
                .collect();
            for label in pending {
                let task = &package.tasks[&label];
                let starts = match &task.dependencies {
                    Dependencies::Package => self.package_at(&task.dir, &mut packages)?.declared.deps.clone(),
                    Dependencies::Only(edges) => edges.clone(),
                    Dependencies::None => Vec::new(),
                };
                let mut walk = Walk::default();
                for edge in &starts {
                    self.walk(edge, &mut walk, &mut packages, &mut Vec::new())?;
                }
                let sources = walk.sources();
                let task = package.tasks.get_mut(&label).expect("a pending task is a task of the package");
                for output in walk.outputs {
                    if !task.deps.contains(&output) {
                        task.deps.push(output);
                    }
                }
                resolved.insert(label, sources);
            }
            let known = package.tasks.len();
            self.add_dependencies(package, observe)?;
            if package.tasks.len() == known {
                return Ok(resolved);
            }
        }
    }

    /// Adds what the dependency `edge` gives to `walk`, with what its own dependencies give: a package's files and
    /// provided tasks, a set of files, or a task's outputs.
    fn walk(
        &self,
        edge: &str,
        walk: &mut Walk,
        packages: &mut BTreeMap<String, Package>,
        path: &mut Vec<String>,
    ) -> Result<(), EngineError> {
        if path.iter().any(|seen| seen == edge) {
            let cycle: Vec<&str> =
                path.iter().skip_while(|seen| *seen != edge).map(String::as_str).chain([edge]).collect();
            return Err(EngineError::Config(format!("the dependencies form a cycle: {}", cycle.join(" -> "))));
        }
        if !walk.seen.insert(edge.to_owned()) {
            return Ok(());
        }
        let (dir, name) = split_dependency(edge);
        let package = self.package_at(dir, packages)?.clone();
        let next = match name {
            None => {
                let include = match package.declared.exports.is_empty() {
                    true => vec![GLOB_EVERYTHING.to_owned()],
                    false => package.declared.exports.clone(),
                };
                walk.add_files(dir, &FileSelection { include, exclude: Vec::new() });
                if let Some(missing) = package.declared.provides.iter().find(|task| !package.tasks.contains_key(*task))
                {
                    return Err(EngineError::Config(format!(
                        "//{dir} provides {missing}, which is not one of its tasks"
                    )));
                }
                walk.outputs.extend(package.declared.provides.iter().cloned());
                package.declared.deps.clone()
            }
            Some(_) => match package.file_sets.get(edge) {
                Some(set) => {
                    walk.add_files(&set.dir, &set.selection);
                    set.deps.clone()
                }
                None if package.tasks.contains_key(edge) => {
                    walk.outputs.push(edge.to_owned());
                    Vec::new()
                }
                None => {
                    return Err(EngineError::Config(format!("{edge} is not a file_set() or a task of its package")));
                }
            },
        };
        path.push(edge.to_owned());
        for edge in &next {
            self.walk(edge, walk, packages, path)?;
        }
        path.pop();
        Ok(())
    }

    /// The package at `dir`, read once for each load.
    fn package_at<'p>(
        &self,
        dir: &str,
        packages: &'p mut BTreeMap<String, Package>,
    ) -> Result<&'p Package, EngineError> {
        if !packages.contains_key(dir) {
            let target = label(dir, PACKAGE_TARGET);
            let frontend = self.frontend(&target)?;
            let request = LoadRequest { root: &self.config.root, target: &target, platform: &self.config.platform };
            let loaded = frontend
                .load(&request)
                .map_err(|source| EngineError::Frontend { frontend: frontend.name(), error: source })?;
            let package =
                expand_calls(&context(&self.config), &self.config.root, &self.config.platform, loaded.package)
                    .map_err(|e| EngineError::Frontend { frontend: "starlark", error: Box::new(e) })?;
            packages.insert(dir.to_owned(), package);
        }
        Ok(&packages[dir])
    }

    /// Adds the tasks of other packages that the package's tasks depend on, with their own dependencies, so that the
    /// package compiles them as its own: each reads its files and writes its outputs in its own directory (decision
    /// 0026).
    fn add_dependencies(&self, package: &mut Package, observe: &mut dyn FnMut(Event)) -> Result<(), EngineError> {
        let mut read = BTreeSet::new();
        loop {
            let missing: BTreeSet<String> = package
                .tasks
                .values()
                .flat_map(|task| &task.deps)
                .filter(|dep| !package.tasks.contains_key(*dep) && !read.contains(*dep))
                .cloned()
                .collect();
            if missing.is_empty() {
                return Ok(());
            }
            for dep in missing {
                let (other, _, _) = self.read(&dep, observe)?;
                for (label, task) in other.tasks {
                    package.tasks.entry(label).or_insert(task);
                }
                read.insert(dep);
            }
        }
    }

    /// The error of a goal whose tasks and actions the filter all excludes.
    fn all_excluded(&self, goal: &str, package: &Package) -> EngineError {
        let required = package.goals.get(goal).cloned().unwrap_or_default();
        EngineError::AllExcluded {
            target: goal.to_owned(),
            excluded: required.into_iter().collect::<Vec<_>>().join(", "),
            origin: self.config.filter.origin.clone(),
        }
    }

    /// The goal or task `name` of every package at or below `dir`, as one goal: the target, `//dir/...:name`.
    fn load_pattern(
        &self,
        target: &str,
        dir: &str,
        name: &str,
        observe: &mut dyn FnMut(Event),
    ) -> Result<Goal, EngineError> {
        let mut merged = Package::default();
        let mut required = BTreeSet::new();
        let mut excluded = Vec::new();
        let mut host_tools = BTreeSet::new();
        let mut defined = false;
        for package in self.packages()?.into_iter().filter(|package| is_at_or_below(package, dir)) {
            let goal = label(&package, name);
            let (read, _, _) = match self.read(&goal, observe) {
                Ok(read) => read,
                Err(EngineError::UnknownGoal { .. }) => continue,
                Err(error) => return Err(error),
            };
            defined = true;
            let Some(selected) = self.select_or_skip(read, &goal, &mut excluded, observe)? else { continue };
            required.extend(selected.package.goals.get(&goal).cloned().unwrap_or_default());
            excluded.extend(selected.excluded);
            host_tools.extend(selected.host_tools);
            merge(&mut merged, selected.package);
        }
        if !defined {
            return Err(EngineError::NoPackageDefines { dir: dir.to_owned(), name: name.to_owned() });
        }
        if required.is_empty() {
            let labels: Vec<&str> = excluded.iter().map(|(label, _)| label.as_str()).collect();
            return Err(EngineError::AllExcluded {
                target: target.to_owned(),
                excluded: labels.join(", "),
                origin: self.config.filter.origin.clone(),
            });
        }
        self.excluded_notes(target, &excluded, observe)?;
        merged.goals = [(target.to_owned(), required)].into();
        self.finish(merged, target.to_owned(), None, target, host_tools, observe)
    }

    /// `select`, where a package whose tasks the filter all excludes is skipped, and its tasks are counted.
    fn select_or_skip(
        &self,
        package: Package,
        goal: &str,
        excluded: &mut Vec<(String, BTreeSet<String>)>,
        observe: &mut dyn FnMut(Event),
    ) -> Result<Option<Selected>, EngineError> {
        let required = package.goals.get(goal).cloned().unwrap_or_default();
        let tags = package.tags.clone();
        match self.select(package, goal, observe) {
            Err(EngineError::AllExcluded { .. }) => {
                let tags_of = |label: &String| tags.get(label).cloned().unwrap_or_default();
                excluded.extend(required.into_iter().map(|label| (label.clone(), tags_of(&label))));
                Ok(None)
            }
            other => other.map(Some),
        }
    }

    /// Notes about what the filter excluded: how many, and by which tags, at the normal level; each one, as a detail.
    fn excluded_notes(
        &self,
        target: &str,
        excluded: &[(String, BTreeSet<String>)],
        observe: &mut dyn FnMut(Event),
    ) -> Result<(), EngineError> {
        if excluded.is_empty() {
            return Ok(());
        }
        let filter = &self.config.filter;
        let matched: BTreeSet<&String> =
            excluded.iter().flat_map(|(_, tags)| tags.iter().filter(|tag| filter.exclude.contains(*tag))).collect();
        let why = match matched.is_empty() {
            true => format!("without a tag of {}", join(&filter.include)),
            false => format!("tagged {}", join(matched.iter().copied())),
        };
        let text = format!("{target}: skipped {} {why} ({})", count(excluded.len(), "task"), filter.origin);
        observe(Event::Note(Note { level: NoteLevel::Normal, text }));
        for (label, tags) in excluded {
            let text = format!("{target}: skipped {label}, tagged {}", join(tags));
            observe(Event::Note(Note { level: NoteLevel::Diagnostic, text }));
        }
        Ok(())
    }

    /// Stores the sources of the package, checks its graph, and gives the goal.
    fn finish(
        &self,
        mut package: Package,
        goal: String,
        scope: Option<String>,
        target: &str,
        host_tools: BTreeSet<String>,
        observe: &mut dyn FnMut(Event),
    ) -> Result<Goal, EngineError> {
        let titles = std::mem::take(&mut package.titles);
        let workspace_views = std::mem::take(&mut package.workspace_views);
        let graph = self.resolve(package, observe)?;
        observe(Event::Loaded { titles: titles.clone(), workspace_views });
        Ok(Goal { graph, key: goal, titles, scope, target: target.to_owned(), host_tools })
    }

    /// The packages of the workspace: the directories that hold a `tasks.star`, relative to the root, `""` for it.
    pub fn packages(&self) -> Result<Vec<String>, EngineError> {
        let files = crate::engine::git::working_tree_files(&self.config.root)?;
        let mut packages: Vec<String> = files
            .iter()
            .filter(|file| file.file_name().is_some_and(is_tasks_file))
            .map(|file| file.parent().map(|dir| dir.display().to_string()).unwrap_or_default())
            .filter(|package| has_tasks_file(&self.config.root.join(package)))
            .collect();
        packages.sort();
        packages.dedup();
        Ok(packages)
    }

    /// The targets that the frontends find in the workspace's files, by frontend.
    pub fn targets(&self) -> Result<Vec<Listed>, EngineError> {
        let files = crate::engine::git::working_tree_files(&self.config.root)?;
        Ok(self
            .frontends
            .iter()
            .flat_map(|frontend| frontend.targets(&self.config.root, &files, &self.config.platform))
            .collect())
    }

    /// What the frontend of a target reports about it, before anything runs.
    pub fn check(&self, target: &str) -> Result<Vec<Finding>, EngineError> {
        let frontend = self.frontend(target)?;
        let request = LoadRequest { root: &self.config.root, target, platform: &self.config.platform };
        let mut findings = frontend
            .check(&request)
            .map_err(|source| EngineError::Frontend { frontend: frontend.name(), error: source })?;
        if let Ok(loaded) = frontend.load(&request) {
            findings.extend(self.missing_tools(&loaded.package));
            findings.extend(unread_dependencies(&loaded.package));
            findings.extend(self.containers(loaded.package)?);
        }
        findings.sort_by(|a, b| a.severity.cmp(&b.severity).then_with(|| a.location.cmp(&b.location)));
        Ok(findings)
    }

    /// The tools that the actions of a package need and that are not found, as findings.
    fn missing_tools(&self, package: &Package) -> Vec<Finding> {
        let needed: BTreeSet<&str> = package
            .actions
            .values()
            .flat_map(|spec| std::iter::once(&spec.root).chain(spec.inputs.values()))
            .filter_map(|source| match source {
                SourceSpec::Tool { name } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        needed
            .into_iter()
            .filter(|name| !self.config.tools.contains_key(*name))
            .map(|name| Finding {
                severity: Severity::Unsupported,
                location: TOOLS_LOCATION.to_owned(),
                message: format!("{name} is not found; {}", crate::engine::config::missing_tool_hint(name)),
            })
            .collect()
    }

    /// The tasks that a local run runs in their images, as findings: each needs this host's container runtime, and
    /// is unsupported when there is none (decision 0024).
    fn containers(&self, mut package: Package) -> Result<Vec<Finding>, EngineError> {
        crate::engine::tasks::compile(&mut package, &self.mode(), &Default::default())?;
        let runtime = crate::sandbox::find_runtime();
        let in_images = package.actions.iter().filter_map(|(label, spec)| match &spec.kind {
            Kind::Host { container: Some(container), .. } => Some((label, container)),
            _ => None,
        });
        Ok(in_images
            .map(|(label, container)| {
                let image = crate::scheduler::changes::container_name(Some(container));
                match &runtime {
                    Some(runtime) => Finding {
                        severity: Severity::Different,
                        location: label.clone(),
                        message: format!(
                            "runs in {image} through {}, on the files of the workspace",
                            runtime.display()
                        ),
                    },
                    None => Finding {
                        severity: Severity::Unsupported,
                        location: label.clone(),
                        message: format!(
                            "runs in {image}, which needs Docker or Podman, and neither is on PATH; install one, or \
                             run it on this host's tools with `azad run --local host`"
                        ),
                    },
                }
            })
            .collect())
    }

    /// Whether a frontend reads the target as it is.
    pub fn accepts(&self, target: &str) -> bool {
        self.frontend(target).is_ok()
    }

    fn frontend(&self, target: &str) -> Result<&dyn Frontend, EngineError> {
        self.frontends
            .iter()
            .find(|f| f.accepts(target))
            .map(AsRef::as_ref)
            .ok_or_else(|| EngineError::NoFrontend(target.to_owned()))
    }

    /// The graph of a file in the text form of the IR, checked, with every source stored.
    pub fn load_text(&self, file: &Path) -> Result<Graph, EngineError> {
        let package = TextFrontend::read(&self.config.root, file)
            .map_err(|source| EngineError::Frontend { frontend: "text", error: source })?;
        self.resolve(package, &mut |_| {})
    }

    fn resolve(&self, package: crate::frontend::Package, observe: &mut dyn FnMut(Event)) -> Result<Graph, EngineError> {
        let mut resolver = Resolver {
            config: &self.config,
            root: &self.config.root,
            store: &self.store,
            puller: &self.puller,
            remote: self.remote.as_deref(),
            timings: &self.timings,
            stored: HashMap::new(),
            images: HashMap::new(),
            declared_at: BTreeMap::new(),
        };
        let graph = resolver.graph(package, observe)?;
        graph.verify().map_err(EngineError::Invalid)?;
        Ok(graph)
    }

    /// `progress`, without the paths that an action left undeclared and that the workspace's git ignores, such as the
    /// `bin/` and `obj/` of a build: they are the expected scratch of the project's tools, not a mistake to report. A
    /// diagnostic note still counts them, because a forgotten output, such as `dist/`, is often ignored too.
    fn without_ignored_scratch(&self, progress: Progress) -> (Progress, Option<String>) {
        let Progress::Done { name, status: Status::Ran { action, result, discarded } } = progress else {
            return (progress, None);
        };
        let ignored = match discarded.is_empty() {
            true => BTreeSet::new(),
            false => crate::engine::git::ignored(&self.config.root, &discarded).unwrap_or_default(),
        };
        let note = ignored.first().map(|first| {
            format!(
                "{name}: discarded {} that git ignores, such as {first}; declare the ones that other tasks read as \
                 outputs",
                count(ignored.len(), "path")
            )
        });
        let discarded = discarded.into_iter().filter(|path| !ignored.contains(path)).collect();
        (Progress::Done { name, status: Status::Ran { action, result, discarded } }, note)
    }

    /// Runs what the goal needs, and records the run.
    pub fn run(
        &self,
        goal: &Goal,
        parallelism: usize,
        observe: &mut dyn FnMut(Event),
    ) -> Result<RunReport, EngineError> {
        let runner = Runner::new(&goal.graph, self.backend.as_ref(), parallelism);
        let mut mentions = Mentions::default();
        let workspace = || {
            Ok(Workspace {
                files: crate::engine::git::working_tree_files(&self.config.root)?,
                packages: self.packages()?,
            })
        };
        let outcome = runner.run(&goal.key, &mut |progress| {
            let (progress, note) = self.without_ignored_scratch(progress);
            mentions.see(&goal.graph, &progress);
            let missing = mentions.missing_packages(&goal.graph, &progress, &workspace).unwrap_or_default();
            let name = match &progress {
                Progress::Done { name, .. } => goal.titles.get(name).unwrap_or(name).clone(),
                _ => String::new(),
            };
            observe(Event::Progress(progress));
            if let Some(text) = note {
                observe(Event::Note(Note { level: NoteLevel::Diagnostic, text }));
            }
            for package in missing {
                observe(Event::Note(Note { level: NoteLevel::Normal, text: missing_package_note(&name, &package) }));
            }
        })?;
        if let Some(text) = missing_host_tools(goal, &outcome) {
            observe(Event::Note(Note { level: NoteLevel::Normal, text }));
        }
        if let Some(remote) = &self.remote {
            remote.store_kept();
            observe(Event::Remote(remote.take_summary()));
        }
        let steps = self.timings.take_noteworthy();
        if !steps.is_empty() {
            observe(Event::Timings(steps));
        }
        let number = self
            .store
            .state()
            .record_run(&self.history_key(goal), &outcome.encode())
            .map_err(EngineError::store("the run record"))?;
        Ok(RunReport { outcome, number })
    }

    /// What a run would do now, from the cache alone; nothing runs.
    pub fn plan(&self, goal: &Goal) -> Result<Outcome, EngineError> {
        Ok(Runner::new(&goal.graph, self.backend.as_ref(), 1).plan(&goal.key)?)
    }

    /// The key of a goal's run history: the target in this workspace, so that two checkouts of a repository, or two
    /// files with a goal of the same name, have their own runs.
    fn history_key(&self, goal: &Goal) -> String {
        let root = std::fs::canonicalize(&self.config.root).unwrap_or_else(|_| self.config.root.clone());
        match self.config.isolated {
            true => format!("{} {} {ISOLATED_HISTORY}", root.display(), goal.target),
            false => format!("{} {}", root.display(), goal.target),
        }
    }

    /// Where the tasks of this engine's runs run.
    pub fn mode(&self) -> Mode {
        Mode::new(&self.config.root, self.config.isolated).with_local(self.config.local)
    }

    /// The last recorded run of a goal.
    pub fn last_run(&self, goal: &Goal) -> Result<Option<(u64, Outcome)>, EngineError> {
        let last =
            self.store.state().last_run(&self.history_key(goal)).map_err(EngineError::store("the run records"))?;
        match last {
            Some((number, record)) => Ok(Some((number, Outcome::decode(&record)?))),
            None => Ok(None),
        }
    }

    /// Explains an action of a goal: from the last recorded run, or from the next run when `next` is set or when the
    /// goal has no recorded run. When the action ran, or must run, because the cache had no result, the explanation
    /// says how the action differs from the newest earlier run of the goal that has it.
    pub fn explain(&self, goal: &Goal, action: &str, next: bool) -> Result<Explained, EngineError> {
        let key = goal.action_key(action).ok_or_else(|| EngineError::UnknownAction(action.to_owned()))?;
        let recorded = match next {
            true => None,
            false => self.last_run(goal)?,
        };
        let (run, outcome) = match recorded {
            Some((number, outcome)) => (Some(number), outcome),
            None => (None, self.plan(goal)?),
        };
        let mut explanation = outcome.explain(&key).ok_or_else(|| EngineError::NotInRun(key.clone()))?;
        explanation.cause = self.cause(goal, &explanation, run)?;
        Ok(Explained { run, explanation })
    }

    /// Why the cache had no result for an action that ran in run `run`, or that must run when `run` is `None`.
    fn cause(&self, goal: &Goal, explanation: &Explanation, run: Option<u64>) -> Result<Option<Cause>, EngineError> {
        let action = match &explanation.status {
            Status::Ran { action, .. } if !explanation.never_cached => action,
            Status::WillRun { action } => action,
            _ => return Ok(None),
        };
        let history = self.history_key(goal);
        let before = match run {
            Some(number) => number,
            None => self.store.state().last_run_number(&history).map_err(EngineError::store("the run records"))? + 1,
        };
        Ok(Some(why::cause(&self.store, &history, &explanation.action, action, before)?))
    }

    /// The result of an action in the last run of a goal, with the run's number: when the action ran, was a cache hit,
    /// or failed after it started.
    pub fn last_result(&self, goal: &Goal, action: &str) -> Result<Option<(u64, ActionResult)>, EngineError> {
        let key = goal.action_key(action).ok_or_else(|| EngineError::UnknownAction(action.to_owned()))?;
        let Some((number, outcome)) = self.last_run(goal)? else { return Ok(None) };
        let result = match outcome.statuses.get(&key) {
            Some(Status::Ran { result, .. } | Status::Cached { result, .. }) => Some(result.clone()),
            Some(Status::Failed { result, .. }) => result.clone(),
            _ => None,
        };
        Ok(result.map(|result| (number, result)))
    }

    /// What an action wrote, as one text: its log, or for a result from before logs were kept, its standard output
    /// and then its standard error, which starts on a line of its own.
    pub fn output_of(&self, result: &ActionResult) -> Result<String, EngineError> {
        let Some(log) = &result.log else {
            let stdout = String::from_utf8_lossy(&self.read_blob(&result.stdout)?).into_owned();
            let stderr = String::from_utf8_lossy(&self.read_blob(&result.stderr)?).into_owned();
            return Ok(joined_lines(stdout, &stderr));
        };
        Ok(String::from_utf8_lossy(&self.read_blob(log)?).into_owned())
    }

    /// The content of a blob, such as a log, from the remote cache when the store does not have it.
    pub fn read_blob(&self, blob: &Digest) -> Result<Vec<u8>, EngineError> {
        if let Some(remote) = &self.remote {
            remote.ensure_blob(blob).map_err(|e| EngineError::Backend(Box::new(e)))?;
        }
        self.store.read(blob).map_err(EngineError::store("a blob"))
    }

    /// Runs one resolved action, unless its result is known. Returns the result and whether it was known.
    pub fn execute(&self, action: &Action) -> Result<(ActionResult, bool), EngineError> {
        if let Some(result) = self.backend.cached(action).map_err(EngineError::Backend)? {
            return Ok((result, true));
        }
        let execution = self.backend.execute(action, &crate::log::Discard).map_err(EngineError::Backend)?;
        Ok((execution.result, false))
    }

    pub fn ingest(&self, dir: &Path) -> Result<Digest, EngineError> {
        self.store.ingest(dir, &Selection::All).map_err(EngineError::store(dir.display().to_string()))
    }

    /// The reference by digest of an image that is named by a tag, such as `python:3.13-slim`; the registry says what
    /// the tag points to now. A reference with a digest stays as it is.
    pub fn pin(&self, reference: &str) -> Result<String, EngineError> {
        self.puller
            .pin(reference)
            .map_err(|source| EngineError::Pull { reference: reference.to_owned(), error: source })
    }

    pub fn pull(&self, reference: &str, platform: &str) -> Result<Image, EngineError> {
        self.puller
            .pull(reference, platform)
            .map_err(|source| EngineError::Pull { reference: reference.to_owned(), error: source })
    }
}

/// Leaves out the goals other than `goal`, and the actions that it does not need, so that loading a goal stores no
/// sources and pulls no images for other goals.
/// A package with only what its goal needs, and what the filter excluded from the goal, with their tags.
/// The name that a package is read by when no goal of it is needed.
const PACKAGE_TARGET: &str = "package";
const GLOB_EVERYTHING: &str = "**";

/// What a task's dependencies give it: the files of directories, by the selections of each, and the tasks whose outputs
/// it reads.
#[derive(Default)]
struct Walk {
    seen: BTreeSet<String>,
    files: BTreeMap<String, Vec<FileSelection>>,
    outputs: Vec<String>,
}

impl Walk {
    fn add_files(&mut self, dir: &str, selection: &FileSelection) {
        for (dir, selection) in crate::engine::tasks::dependency_selections(dir, selection) {
            let selections = self.files.entry(dir).or_default();
            if !selections.contains(&selection) {
                selections.push(selection);
            }
        }
    }

    fn sources(&self) -> Vec<(String, SourceSpec)> {
        self.files
            .iter()
            .map(|(dir, selections)| {
                (dir.clone(), SourceSpec::Tracked { dir: dir.clone(), selections: selections.clone() })
            })
            .collect()
    }
}

struct Selected {
    package: Package,
    excluded: Vec<(String, BTreeSet<String>)>,
    host_tools: BTreeSet<String>,
}

/// `first`, then `second` from the start of a line.
fn joined_lines(mut first: String, second: &str) -> String {
    if !first.is_empty() && !first.ends_with('\n') {
        first.push('\n');
    }
    first + second
}

/// The exit code of a POSIX shell that did not find a command.
const COMMAND_NOT_FOUND: i64 = 127;

/// A note for the tasks with an image that failed because the host lacks a command, which their images have.
/// A package that declares `deps` that no task of it reads (decision 0028): only a task with `dependencies` gets them.
fn unread_dependencies(package: &Package) -> Option<Finding> {
    let deps = &package.declared.deps;
    if deps.is_empty() || package.tasks.values().any(|task| task.dependencies != Dependencies::None) {
        return None;
    }
    let location = package.tasks.keys().next().map(|label| label.split(':').next().unwrap_or(label).to_owned());
    Some(Finding {
        severity: Severity::Ignored,
        location: location.unwrap_or_default(),
        message: format!(
            "no task reads its deps {}: only a task with `dependencies` gets them (decision 0028)",
            join(deps.iter())
        ),
    })
}

/// What to say when a failed isolated action's output named files of a package that its sandbox did not hold.
fn missing_package_note(action: &str, package: &str) -> String {
    format!(
        "{action} named files of {package}, which its sandbox did not hold: add {package} to the deps of its package \
         and give the task `dependencies` (decision 0028), or add the files to its sources"
    )
}

fn missing_host_tools(goal: &Goal, outcome: &Outcome) -> Option<String> {
    let failed: Vec<&String> = outcome
        .statuses
        .iter()
        .filter(|(label, _)| goal.host_tools.contains(*label))
        .filter(
            |(_, status)| matches!(status, Status::Failed { result: Some(r), .. } if r.exit_code == COMMAND_NOT_FOUND),
        )
        .map(|(label, _)| goal.titles.get(label).unwrap_or(label))
        .collect();
    if failed.is_empty() {
        return None;
    }
    Some(format!(
        "{} ran with this machine's tools and exited with {COMMAND_NOT_FOUND}, the code of a shell that does not find a \
         command. `--local image` runs tasks in their images, through Docker or Podman.",
        join(failed)
    ))
}

/// Adds the actions of one package to the package of a pattern; labels name their package, so none collide.
fn merge(into: &mut Package, package: Package) {
    into.actions.extend(package.actions);
    into.titles.extend(package.titles);
    into.workspace_views.extend(package.workspace_views);
    into.declared_at.extend(package.declared_at);
    into.git_refs.extend(package.git_refs);
    into.tags.extend(package.tags);
}

fn join<'a>(items: impl IntoIterator<Item = &'a String>) -> String {
    items.into_iter().map(String::as_str).collect::<Vec<_>>().join(", ")
}

/// `1 task`, `3 tasks`.
fn count(n: usize, noun: &str) -> String {
    match n {
        1 => format!("1 {noun}"),
        n => format!("{n} {noun}s"),
    }
}

fn keep_only_what_the_goal_needs(package: &mut Package, goal: &str) {
    let mut needed: BTreeSet<String> = BTreeSet::new();
    let mut next: Vec<String> = package.goals.get(goal).into_iter().flatten().cloned().collect();
    while let Some(label) = next.pop() {
        let Some(spec) = package.actions.get(&label) else { continue };
        for source in std::iter::once(&spec.root).chain(spec.inputs.values()).chain(spec.work.values()) {
            if let SourceSpec::Output { action, .. } = source {
                next.push(action.clone());
            }
        }
        needed.insert(label);
    }
    package.actions.retain(|label, _| needed.contains(label));
    package.workspace_views.retain(|label, _| needed.contains(label));
    package.goals.retain(|label, _| label == goal);
}

/// A backend that knows no results, so that every action that a goal needs runs again (`azad run --force`). The
/// results of the new runs are still stored.
struct Forced(Box<dyn Backend>);

impl Backend for Forced {
    fn cached(&self, _action: &Action) -> Result<Option<ActionResult>, crate::scheduler::BackendError> {
        Ok(None)
    }

    fn execute(
        &self,
        action: &Action,
        logs: &dyn crate::log::LogSink,
    ) -> Result<crate::scheduler::BackendExecution, crate::scheduler::BackendError> {
        self.0.execute(action, logs)
    }
}

#[cfg(test)]
mod tests {
    use super::joined_lines;

    #[test]
    fn the_second_output_starts_on_a_line_of_its_own() {
        assert_eq!(joined_lines("built".to_owned(), "warning\n"), "built\nwarning\n");
        assert_eq!(joined_lines("built\n".to_owned(), "warning\n"), "built\nwarning\n");
        assert_eq!(joined_lines(String::new(), "warning\n"), "warning\n");
    }
}
