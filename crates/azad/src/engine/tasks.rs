//! Tasks become actions (decision 0015): on the host for a local run, or hermetic in their image for an isolated run.
//! Both forms read the same files and make the same outputs, at the same paths relative to the workspace. A task
//! without an image runs on the host in both, as it does in CI, on the runner. A local run can also run a task in its
//! image, through the host's container runtime, on the files of the workspace (decision 0024).

use std::collections::BTreeMap;
use std::path::Path;

use crate::engine::EngineError;
use crate::frontend::{FileSelection, LocalRun, Package, SourceSpec, Task, WorkspaceView, split_workspace_pattern};
use crate::ir::action::{Container, ContainerImage};
use crate::ir::{Kind, Spec, WORK_DIR};

const GLOB_ALL_BELOW: &str = "/**";
const ROOT: &str = "/";

/// Where the tasks of a run run, and the workspace, which tasks without an image always use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mode {
    pub workspace: String,
    /// Whether tasks with an image run in it, in a sandbox.
    pub isolated: bool,
    /// Where a local run runs every task with an image, whatever the task says; `None` to follow each task.
    pub local: Option<LocalRun>,
}

impl Mode {
    pub fn new(workspace: &Path, isolated: bool) -> Mode {
        let workspace = std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
        Mode { workspace: workspace.display().to_string(), isolated, local: None }
    }

    pub fn with_local(self, local: Option<LocalRun>) -> Mode {
        Mode { local, ..self }
    }

    /// Whether a task that has an image runs with the host's tools in this mode, as `compile_task` chooses.
    pub(crate) fn uses_host_tools_for_image(&self, task: &Task) -> bool {
        task.image.is_some() && !self.isolated && self.local.unwrap_or(task.local) == LocalRun::Host
    }
}

/// The files that each task reads from its dependencies, by label, as directories and their sources (decision 0028).
pub(crate) type DependencySources = BTreeMap<String, Vec<(String, SourceSpec)>>;

/// Adds an action for each task of the package.
pub(crate) fn compile(package: &mut Package, mode: &Mode, dependencies: &DependencySources) -> Result<(), EngineError> {
    for (label, task) in &package.tasks {
        let at = task.declared_at.as_deref().map(|at| format!(", {at}")).unwrap_or_default();
        for (_, sources) in sources_of(label, task)? {
            package.declared_at.insert(sources.to_string(), format!("the sources of {label}{at}"));
        }
        let from_dependencies = dependencies.get(label).map(Vec::as_slice).unwrap_or_default();
        let action = compile_task(label, task, &package.tasks, mode, from_dependencies)?;
        package.workspace_views.insert(label.clone(), workspace_view(&action, task, mode));
        if package.actions.insert(label.clone(), action).is_some() {
            return Err(EngineError::Config(format!("{label} is both an action and a task")));
        }
    }
    Ok(())
}

fn compile_task(
    label: &str,
    task: &Task,
    tasks: &BTreeMap<String, Task>,
    mode: &Mode,
    from_dependencies: &[(String, SourceSpec)],
) -> Result<Spec<SourceSpec>, EngineError> {
    let mut work: BTreeMap<String, SourceSpec> = sources_of(label, task)?.into_iter().collect();
    for (dir, sources) in from_dependencies {
        if work.insert(dir.clone(), sources.clone()).is_some() {
            return Err(EngineError::Config(format!(
                "{label}: its sources and its dependencies both name the directory {dir:?}; name it once"
            )));
        }
    }
    if !work.is_empty() && !work.keys().any(|dir| is_within(&task.dir, dir)) {
        work.insert(task.dir.clone(), SourceSpec::Inline { files: BTreeMap::new() });
    }
    for dep in &task.deps {
        let Some(dep_task) = tasks.get(dep) else {
            return Err(EngineError::Config(format!("{label}: {dep} is not a task")));
        };
        for path in &dep_task.generates {
            let path = workspace_path(&dep_task.dir, path);
            work.insert(path.clone(), SourceSpec::Output { action: dep.clone(), path });
        }
    }
    let outputs = task.generates.iter().map(|path| workspace_path(&task.dir, path)).collect();
    let on_the_host = SourceSpec::Inline { files: BTreeMap::new() };
    let mut env = task.env.clone();
    let local = mode.local.unwrap_or(task.local);
    let (kind, root, caches) = match (mode.isolated, &task.image, local) {
        (true, Some(image), _) => {
            let kind = match task.network {
                true => Kind::Fetch,
                false => Kind::Hermetic,
            };
            (kind, image.clone(), sandbox_caches(task))
        }
        (false, Some(image), LocalRun::Image) => {
            let (container, image_env) = container(label, task, image)?;
            if let ContainerImage::Dockerfile { file, .. } = &container.image {
                work.insert(file.clone(), file_source(file));
            }
            env = image_env.into_iter().chain(task.env.clone()).collect();
            let kind = Kind::Host { workspace: mode.workspace.clone(), container: Some(container) };
            (kind, on_the_host, workspace_caches(task))
        }
        _ => (Kind::Host { workspace: mode.workspace.clone(), container: None }, on_the_host, workspace_caches(task)),
    };
    Ok(Spec {
        kind,
        platform: task.platform.clone(),
        root,
        command: task.command.clone(),
        env,
        inputs: BTreeMap::new(),
        outputs,
        caches,
        work,
        dir: task.dir.clone(),
        secrets: task.secrets.clone(),
    })
}

/// The container in which a local run runs a task in its image, and the environment that its image asks for. Its
/// volumes are the task's caches at absolute paths; on the files of the workspace, a relative cache is the directory
/// itself.
fn container(
    label: &str,
    task: &Task,
    image: &SourceSpec,
) -> Result<(Container, BTreeMap<String, String>), EngineError> {
    let volumes = task.caches.iter().filter(|(path, _)| path.starts_with(ROOT)).map(|(p, n)| (p.clone(), n.clone()));
    let (image, env) = match image {
        SourceSpec::Image { reference, env } => (ContainerImage::Reference(reference.clone()), env.clone()),
        SourceSpec::Dockerfile { file, context, target, args, contexts, env } => {
            let image = ContainerImage::Dockerfile {
                file: file.clone(),
                context: context.clone(),
                target: target.clone(),
                args: args.clone(),
                contexts: contexts.clone(),
            };
            (image, env.clone())
        }
        other => return Err(EngineError::Config(format!("{label}: the image of a task is an image(), not {other}"))),
    };
    Ok((Container { image, volumes: volumes.collect() }, env))
}

/// A file of the workspace as a source: the Dockerfile of a container, so that a change to it runs the task again.
fn file_source(file: &str) -> SourceSpec {
    let (dir, name) = file.rsplit_once('/').unwrap_or(("", file));
    SourceSpec::Glob { dir: dir.to_owned(), include: vec![name.to_owned()], exclude: Vec::new() }
}

/// Where the task's action sees the workspace: in place on the host, or below the work directory in a sandbox.
fn workspace_view(action: &Spec<SourceSpec>, task: &Task, mode: &Mode) -> WorkspaceView {
    let root = match action.kind {
        Kind::Host { .. } => mode.workspace.clone(),
        _ => WORK_DIR.to_owned(),
    };
    WorkspaceView { root, start: task.dir.clone() }
}

/// The caches of a task in its image: an absolute path is a path of the sandbox, and a path relative to the package
/// is that directory of the work directory, which holds the workspace.
fn sandbox_caches(task: &Task) -> BTreeMap<String, String> {
    let at = |path: &String| match path.starts_with(ROOT) {
        true => path.clone(),
        false => format!("{WORK_DIR}/{}", workspace_path(&task.dir, path)),
    };
    task.caches.iter().map(|(path, name)| (at(path), name.clone())).collect()
}

/// The caches of a task on the host: the directories of the workspace that its relative paths name. A tool keeps its
/// state there anyway; naming it lets a new machine, such as a CI runner, start from a copy (decision 0017). Sandbox
/// paths do not exist on the host.
fn workspace_caches(task: &Task) -> BTreeMap<String, String> {
    let relative = task.caches.iter().filter(|(path, _)| !path.starts_with(ROOT));
    relative.map(|(path, name)| (workspace_path(&task.dir, path), name.clone())).collect()
}

/// The files that a task reads, without what it generates itself, by the directory that each set is relative to: the
/// package for its own patterns, and the directory of each `//` pattern (decision 0026). A directory inside another
/// joins the outer one, so that no set holds another.
fn sources_of(label: &str, task: &Task) -> Result<Vec<(String, SourceSpec)>, EngineError> {
    if task.sources.is_empty() {
        return Ok(Vec::new());
    }
    let in_dir = |pattern: &String| in_directory(label, &task.dir, pattern);
    let include = task.sources.iter().map(in_dir).collect::<Result<Vec<_>, _>>()?;
    let exclude = task.exclude.iter().map(in_dir).collect::<Result<Vec<_>, _>>()?;
    let generated = task.generates.iter().flat_map(|path| [path.clone(), format!("{path}{GLOB_ALL_BELOW}")]);
    let mut dirs: Vec<&String> = include.iter().map(|(dir, _)| dir).collect();
    dirs.sort_by_key(|dir| dir.len());
    let mut outer: Vec<String> = Vec::new();
    for dir in dirs {
        if !outer.iter().any(|kept| is_within(dir, kept)) {
            outer.push(dir.clone());
        }
    }
    let holder = |dir: &str| outer.iter().find(|kept| is_within(dir, kept)).cloned();
    let mut sets: BTreeMap<String, (Vec<String>, Vec<String>)> = BTreeMap::new();
    for (dir, pattern) in &include {
        let set = holder(dir).expect("each include's directory has an outer directory");
        sets.entry(set.clone()).or_default().0.push(relative_to(&set, dir, pattern));
    }
    for (dir, pattern) in &exclude {
        let Some(set) = holder(dir) else {
            return Err(EngineError::Config(format!(
                "{label}: exclude {pattern:?} of {dir:?} is outside every source"
            )));
        };
        sets.entry(set.clone()).or_default().1.push(relative_to(&set, dir, pattern));
    }
    if let Some(set) = holder(&task.dir) {
        let excluded = &mut sets.entry(set.clone()).or_default().1;
        excluded.extend(generated.map(|pattern| relative_to(&set, &task.dir, &pattern)));
    }
    Ok(sets
        .into_iter()
        .map(|(dir, (include, exclude))| (dir.clone(), SourceSpec::Glob { dir, include, exclude }))
        .collect())
}

/// Azad's own files, which no tool reads: a dependency does not give them.
const AZAD_FILES: [&str; 3] = ["**/tasks.star", "**/tasks.yaml", "**/config.star"];

/// A selection of the files of `dir` as selections of the directories before their patterns' first wildcard, so that
/// a selection of a subdirectory, such as `tools/shim/**`, is that directory's and not all of `dir`'s; without Azad's
/// own files.
pub(crate) fn dependency_selections(dir: &str, selection: &FileSelection) -> Vec<(String, FileSelection)> {
    let mut sets: BTreeMap<String, FileSelection> = BTreeMap::new();
    for pattern in &selection.include {
        let (below, rest) = fixed_prefix(pattern);
        let set = sets.entry(join_dir(dir, &below)).or_default();
        set.include.push(rest);
        if set.exclude.is_empty() {
            set.exclude = excludes_below(&below, &selection.exclude);
            set.exclude.extend(AZAD_FILES.iter().map(|pattern| (*pattern).to_owned()));
        }
    }
    sets.into_iter().collect()
}

/// The directory of a pattern before its first wildcard, and the pattern below it: `tools/shim/**` is
/// `("tools/shim", "**")`.
fn fixed_prefix(pattern: &str) -> (String, String) {
    let segments: Vec<&str> = pattern.split('/').collect();
    let wildcard = segments.iter().position(|s| s.contains(['*', '?', '[', '{'])).unwrap_or(segments.len() - 1);
    (segments[..wildcard].join("/"), segments[wildcard..].join("/"))
}

/// The excludes that apply below `below`, relative to it: those that start with `**`, and those inside it.
fn excludes_below(below: &str, exclude: &[String]) -> Vec<String> {
    exclude
        .iter()
        .filter_map(|pattern| match (pattern.starts_with("**"), below.is_empty()) {
            (true, _) | (false, true) => Some(pattern.clone()),
            (false, false) => pattern.strip_prefix(&format!("{below}/")).map(str::to_owned),
        })
        .collect()
}

fn join_dir(dir: &str, below: &str) -> String {
    match (dir.is_empty(), below.is_empty()) {
        (_, true) => dir.to_owned(),
        (true, false) => below.to_owned(),
        (false, false) => format!("{dir}/{below}"),
    }
}

/// The directory that a pattern is relative to, and the pattern below it: the package's for a pattern of the package.
fn in_directory(label: &str, package: &str, pattern: &str) -> Result<(String, String), EngineError> {
    match split_workspace_pattern(pattern) {
        None => Ok((package.to_owned(), pattern.to_owned())),
        Some(Ok(split)) => Ok(split),
        Some(Err(problem)) => Err(EngineError::Config(format!("{label}: {problem}"))),
    }
}

/// Whether `dir` is `outer` or a directory below it; every directory is within the root, `""`.
fn is_within(dir: &str, outer: &str) -> bool {
    outer.is_empty() || dir == outer || dir.strip_prefix(outer).is_some_and(|rest| rest.starts_with('/'))
}

/// `pattern` of `dir` as a pattern of `outer`, which holds `dir`.
fn relative_to(outer: &str, dir: &str, pattern: &str) -> String {
    let below = dir.strip_prefix(outer).unwrap_or(dir).trim_start_matches('/');
    match below.is_empty() {
        true => pattern.to_owned(),
        false => format!("{below}/{pattern}"),
    }
}

fn workspace_path(dir: &str, path: &str) -> String {
    match dir.is_empty() {
        true => path.to_owned(),
        false => format!("{dir}/{path}"),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn task(dir: &str, sources: &[&str], exclude: &[&str], generates: &[&str]) -> Task {
        let strings = |items: &[&str]| items.iter().map(|item| (*item).to_owned()).collect();
        Task {
            platform: String::new(),
            dir: dir.to_owned(),
            command: vec!["true".to_owned()],
            sources: strings(sources),
            exclude: strings(exclude),
            generates: strings(generates),
            deps: Vec::new(),
            image: None,
            env: BTreeMap::new(),
            caches: BTreeMap::new(),
            network: false,
            local: LocalRun::Host,
            secrets: BTreeSet::new(),
            doc: String::new(),
            declared_at: None,
            dependencies: Default::default(),
        }
    }

    fn glob(dir: &str, include: &[&str], exclude: &[&str]) -> (String, SourceSpec) {
        let strings = |items: &[&str]| items.iter().map(|item| (*item).to_owned()).collect();
        (dir.to_owned(), SourceSpec::Glob { dir: dir.to_owned(), include: strings(include), exclude: strings(exclude) })
    }

    /// A task with only its own patterns has the one set that it had before decision 0026, so its digest stays.
    #[test]
    fn the_patterns_of_the_package_are_one_set_at_the_package() {
        let sources = sources_of("//app:t", &task("app", &["src/**", "go.mod"], &["src/gen/**"], &["out"])).unwrap();
        assert_eq!(sources, [glob("app", &["src/**", "go.mod"], &["src/gen/**", "out", "out/**"])]);
    }

    #[test]
    fn a_pattern_of_another_directory_is_a_set_at_that_directory() {
        let t = task("apps/api", &["**", "//libs/core/**"], &["//libs/core/**/bin/**"], &[]);
        let sources = sources_of("//apps/api:t", &t).unwrap();
        assert_eq!(sources, [glob("apps/api", &["**"], &[]), glob("libs/core", &["**"], &["**/bin/**"])]);
    }

    /// A directory that holds the package takes the package's patterns, so that no set lies inside another.
    #[test]
    fn a_directory_inside_another_joins_the_outer_one() {
        let holds_the_package = task("apps/api", &["src/**", "//apps/**"], &["obj/**"], &[]);
        let sources = sources_of("//apps/api:t", &holds_the_package).unwrap();
        assert_eq!(sources, [glob("apps", &["api/src/**", "**"], &["api/obj/**"])]);
        let beside_the_package = task("apps/api", &["src/**", "//apps/shared/**"], &["obj/**"], &[]);
        let sources = sources_of("//apps/api:t", &beside_the_package).unwrap();
        assert_eq!(sources, [glob("apps/api", &["src/**"], &["obj/**"]), glob("apps/shared", &["**"], &[])]);
    }

    #[test]
    fn an_exclude_outside_every_source_is_an_error() {
        let t = task("app", &["src/**"], &["//lib/**"], &[]);
        let error = sources_of("//app:t", &t).unwrap_err().to_string();
        assert!(error.contains("outside every source"), "{error}");
    }
}
