//! The contract between frontends and the engine. A frontend reads what people write (Starlark, a GHA workflow, the
//! text form of the IR) and returns a `Package`: actions whose sources are not stored yet, and goals. The engine
//! stores the sources, checks the graph and runs it; a frontend runs nothing.

pub mod gha;
pub mod starlark;
pub mod text;

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use crate::ir::{Digest, Spec};

pub type FrontendError = Box<dyn Error + Send + Sync>;

/// A source before the engine stores it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceSpec {
    /// Files of a directory below the workspace root that match `include` and not `exclude`.
    Glob { dir: String, include: Vec<String>, exclude: Vec<String> },
    /// All of a directory: absolute, or relative to the workspace root.
    Local { path: PathBuf },
    /// An OCI image, named by the digest of its manifest or index. As the root of an action, `env` goes on top of the
    /// image's own environment, and the action's environment on top of both.
    Image { reference: String, env: BTreeMap<String, String> },
    /// An output of another action of the package.
    Output { action: String, path: String },
    /// A tree that is already in the store.
    Tree(Digest),
    /// Files that the frontend makes, such as a generated plan, by relative path.
    Inline { files: BTreeMap<String, InlineFile> },
    /// A file that the engine provides, such as a runner binary, as a tree that holds only that file.
    Tool { name: String },
    /// The files of a directory below the workspace root that git tracks or would track (not ignored), as they are in
    /// the working tree. This is what a checkout gives.
    GitFiles { dir: String },
    /// A git repository at a revision (a commit, tag or branch), without its `.git` directory.
    Git { url: String, rev: String },
    /// The files of a directory below the workspace root that git tracks or would track, and that one of `selections`
    /// keeps (decision 0028): what a dependency gives the tasks that depend on it.
    Tracked { dir: String, selections: Vec<FileSelection> },
    /// An image that a container runtime builds from a Dockerfile of the workspace (decision 0024), by paths relative
    /// to the workspace root. `env` is as for `Image`.
    Dockerfile {
        file: String,
        context: String,
        target: String,
        args: BTreeMap<String, String>,
        /// BuildKit's named build contexts, by name, as paths from the workspace root.
        contexts: BTreeMap<String, String>,
        env: BTreeMap<String, String>,
    },
}

/// Patterns relative to a directory: the files that match `include` and not `exclude`.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct FileSelection {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

impl fmt::Display for FileSelection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "include={:?}, exclude={:?}", self.include, self.exclude)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineFile {
    pub content: Vec<u8>,
    pub executable: bool,
}

impl fmt::Display for SourceSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SourceSpec::Glob { dir, include, exclude } => {
                write!(f, "glob(//{dir}, include={include:?}, exclude={exclude:?})")
            }
            SourceSpec::Local { path } => write!(f, "local({})", path.display()),
            SourceSpec::Image { reference, env } if env.is_empty() => write!(f, "image({reference:?})"),
            SourceSpec::Image { reference, env } => write!(f, "image({reference:?}, env={env:?})"),
            SourceSpec::Output { action, path } => write!(f, "output({action}, {path:?})"),
            SourceSpec::Tree(digest) => write!(f, "{digest}"),
            SourceSpec::Inline { files } => {
                let names: Vec<&str> = files.keys().map(String::as_str).collect();
                write!(f, "inline({})", names.join(", "))
            }
            SourceSpec::Tool { name } => write!(f, "tool({name})"),
            SourceSpec::GitFiles { dir } => write!(f, "git-files(//{dir})"),
            SourceSpec::Tracked { dir, selections } => {
                let selections: Vec<String> = selections.iter().map(FileSelection::to_string).collect();
                write!(f, "tracked(//{dir}, {})", selections.join(" | "))
            }
            SourceSpec::Git { url, rev } => write!(f, "git({url}@{rev})"),
            SourceSpec::Dockerfile { file, target, .. } if target.is_empty() => write!(f, "dockerfile(//{file})"),
            SourceSpec::Dockerfile { file, target, .. } => write!(f, "dockerfile(//{file}, target={target})"),
        }
    }
}

/// The event of a push of a branch, in GitHub's name.
pub const PUSH_EVENT: &str = "push";
/// The event of a pull request that was opened or got a new commit, in GitHub's name.
pub const PULL_REQUEST_EVENT: &str = "pull_request";
/// The events on which a goal can run.
pub const GOAL_EVENTS: [&str; 2] = [PUSH_EVENT, PULL_REQUEST_EVENT];

/// Actions by name, and goals by name, before their sources are stored.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Package {
    pub actions: BTreeMap<String, Spec<SourceSpec>>,
    /// Each goal names the actions that must succeed for it to hold.
    pub goals: BTreeMap<String, BTreeSet<String>>,
    /// Module calls that the engine expands to actions of this package (decision 0014). An action or goal of the
    /// package can name the call's action, `label(call.package, call.name)`, and its declared outputs.
    pub calls: Vec<Call>,
    /// The names to show for actions whose label does not say enough, by label: GHA shows the values of a matrix
    /// job, as in `//ci.yml:test.1 (3.12, slow)`. Other actions show their label.
    pub titles: BTreeMap<String, String>,
    /// Tasks by label (decision 0015). The engine compiles each one to an action of the same label: a host action
    /// for a local run, or a hermetic action in its image for an isolated run.
    pub tasks: BTreeMap<String, Task>,
    /// What each goal is for, by label, for `azad list`.
    pub docs: BTreeMap<String, String>,
    /// The events on which the orchestrator runs each goal, by label, such as `push` and `pull_request`.
    pub events: BTreeMap<String, BTreeSet<String>>,
    /// Where each source was declared, as `<file>:<line>`, by the text of its spec (`SourceSpec::to_string`), for
    /// messages about it.
    pub declared_at: BTreeMap<String, String>,
    /// Where actions see the workspace, by label, for the actions that see it: tasks and GHA jobs. The paths in the
    /// output of other actions are sandbox paths that cannot be mapped.
    pub workspace_views: BTreeMap<String, WorkspaceView>,
    /// The git refs that evaluation resolved, such as `HEAD` or `refs/tags/v1.4.0`, with the commit that each named
    /// (decision 0021). A ref can move, so these are inputs of the evaluation; the facts of a commit are not.
    pub git_refs: BTreeMap<String, String>,
    /// What evaluation decided, and why, for the user (decision 0022).
    pub notes: Vec<Note>,
    /// The tags of tasks and actions, by label, which `--include` and `--exclude` select by (decision 0023).
    pub tags: BTreeMap<String, BTreeSet<String>>,
    /// What the package says of itself with `package()` (decision 0028).
    pub declared: Declarations,
    /// The named sets of files of `files()`, by label (decision 0028).
    pub file_sets: BTreeMap<String, FileSet>,
}

/// What a package says of itself for the packages that depend on it (decision 0028).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Declarations {
    /// The package's immediate dependencies: packages, such as `//libs/core`, or the sets of files and tasks of
    /// packages, such as `//libs/core:contracts`.
    pub deps: Vec<String>,
    /// The patterns of the files that the package gives its dependents; empty for all that git tracks.
    pub exports: Vec<String>,
    /// The labels of the package's tasks whose outputs its dependents get.
    pub provides: Vec<String>,
}

/// A named set of files of a package (decision 0028): files below the package, and the dependencies that come with
/// them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileSet {
    pub dir: String,
    pub selection: FileSelection,
    pub deps: Vec<String>,
}

/// Which dependencies a task gets (decision 0028).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Dependencies {
    /// None: the task reads only its own sources and `deps`.
    #[default]
    None,
    /// The immediate dependencies of its package, and theirs.
    Package,
    /// These dependencies, and theirs.
    Only(Vec<String>),
}

/// How much a note matters, for the verbosity of the output (decision 0022).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum NoteLevel {
    /// A decision that changes what happens, such as facts from another source because a clone is shallow.
    Normal,
    /// A detail of how a decision was made, such as the commit that a ref named.
    Diagnostic,
}

/// What a frontend or the engine decided, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub level: NoteLevel,
    pub text: String,
}

/// Where an action sees the workspace, so that a path in its output can be read as a path in the workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceView {
    /// The absolute path of the workspace root, as the command sees it: the work directory of an isolated task, the
    /// workspace of a GHA job, or the real workspace of a host task.
    pub root: String,
    /// The directory in which the command starts, relative to the root; empty for the root.
    pub start: String,
}

impl WorkspaceView {
    /// The path relative to the workspace root of a path that the action names: an absolute path below `root`, or a
    /// path relative to `start`. `None` for a path outside the workspace, and for the root itself.
    pub fn workspace_path(&self, path: &str) -> Option<String> {
        let path = Path::new(path);
        let relative = match path.is_absolute() {
            true => path.strip_prefix(&self.root).ok()?.to_path_buf(),
            false => Path::new(&self.start).join(path),
        };
        normal_relative(&relative)
    }
}

/// A relative path without `.` and `..`, with `/` between its parts; `None` when it leaves its base or is empty.
fn normal_relative(path: &Path) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str()?),
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    match parts.is_empty() {
        true => None,
        false => Some(parts.join("/")),
    }
}

/// Work described by the files of the workspace, not by sandbox paths, so that it can run on the host or in an image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task {
    pub platform: String,
    /// The package directory below the workspace root. The command starts there, and `sources` and `generates` are
    /// relative to it.
    pub dir: String,
    pub command: Vec<String>,
    /// Patterns of the files that the task reads, and of files to leave out of them: relative to the package, or to
    /// the workspace's root after `//` (decision 0026).
    pub sources: Vec<String>,
    pub exclude: Vec<String>,
    /// The directories that the task writes, for the tasks after it.
    pub generates: Vec<String>,
    /// The labels of tasks that must succeed first, of this package or another (decision 0026); the task reads what
    /// they generate.
    pub deps: Vec<String>,
    /// The dependencies of packages and sets of files that the task reads, with theirs (decision 0028).
    pub dependencies: Dependencies,
    /// The image of an isolated run. A task without one runs only on the host.
    pub image: Option<SourceSpec>,
    pub env: BTreeMap<String, String>,
    /// Cache volumes of an isolated run; on the host, the tools keep their state in the workspace.
    pub caches: BTreeMap<String, String>,
    /// Whether an isolated run has network, as `fetch()` has.
    pub network: bool,
    /// Where a local run runs the task: on the host's tools, or in its image (decision 0024).
    pub local: LocalRun,
    /// The variables of `azad`'s environment that the command gets in its image (decision 0024).
    pub secrets: BTreeSet<String>,
    /// What the task is for, for `azad list`.
    pub doc: String,
    /// Where the task was declared, as `<file>:<line>`, for messages about it.
    pub declared_at: Option<String>,
}

/// Where a local run runs a task (decision 0024). An isolated run always runs a task with an image in it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LocalRun {
    /// On the host, with its tools.
    #[default]
    Host,
    /// In the task's image, through the host's container runtime, on the files of the workspace.
    Image,
}

impl LocalRun {
    pub const HOST: &'static str = "host";
    pub const IMAGE: &'static str = "image";

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            Self::HOST => Some(LocalRun::Host),
            Self::IMAGE => Some(LocalRun::Image),
            _ => None,
        }
    }
}

/// A call of a module, from a frontend that cannot run modules itself, such as GHA.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    /// The package of the actions that the call makes.
    pub package: String,
    /// The directory below the workspace root that the call's file patterns are relative to.
    pub dir: String,
    /// The call's name; the module names its actions with it.
    pub name: String,
    /// The `.star` file of the module, such as `@std//test.star`, and the name of the module in it.
    pub file: String,
    pub symbol: String,
    /// The arguments, by parameter. Text is allowed for every parameter type (see `ParamType`).
    pub args: BTreeMap<String, serde_json::Value>,
}

/// What a frontend needs to read a target.
#[derive(Clone, Copy, Debug)]
pub struct LoadRequest<'a> {
    /// The workspace root; labels and globs resolve against it.
    pub root: &'a Path,
    /// What the user named, such as `//services/orders:check`.
    pub target: &'a str,
    /// The platform of the actions, such as `linux/amd64`.
    pub platform: &'a str,
}

/// What a frontend read for a target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Loaded {
    pub package: Package,
    /// The key of the goal in `package.goals`.
    pub goal: String,
    /// The scope of short action names: an action `x` of the scope `s` is `label(s, x)`. `None` when names are not
    /// labels.
    pub scope: Option<String>,
}

pub trait Frontend: Send + Sync {
    /// A short name for messages, such as `starlark`.
    fn name(&self) -> &'static str;
    /// Whether this frontend reads the target.
    fn accepts(&self, target: &str) -> bool;
    fn load(&self, request: &LoadRequest<'_>) -> Result<Loaded, FrontendError>;
    /// What this frontend cannot run in the target, and what it runs differently, before anything runs.
    fn check(&self, request: &LoadRequest<'_>) -> Result<Vec<Finding>, FrontendError> {
        self.load(request).map(|_| Vec::new())
    }
    /// The targets that this frontend reads among the workspace's files, which are relative to `root`.
    fn targets(&self, _root: &Path, _files: &BTreeSet<PathBuf>, _platform: &str) -> Vec<Listed> {
        Vec::new()
    }
}

/// A target that a frontend found in the workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    /// What `azad run` takes, such as `//services/orders:check`.
    pub target: String,
    /// Why the file of the target cannot be read now, when it cannot.
    pub problem: Option<String>,
    /// What the target is for, when its definition says so.
    pub doc: String,
    /// The events on which the orchestrator runs the target (decision 0020).
    pub events: BTreeSet<String>,
    /// The tags of the target: a task's own, or those of the tasks that a goal requires (decision 0023).
    pub tags: BTreeSet<String>,
}

/// One thing that a frontend reports about a target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub severity: Severity,
    /// Where: a job, a step, or a file.
    pub location: String,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// The target cannot run until this changes.
    Unsupported,
    /// Azad runs it, but differently from the original system.
    Different,
    /// Azad does not use it.
    Ignored,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Severity::Unsupported => "unsupported",
            Severity::Different => "different",
            Severity::Ignored => "ignored",
        };
        f.write_str(text)
    }
}

/// The label of a name in a package: `//dir:name`, or `//:name` at the root.
pub fn label(package: &str, name: &str) -> String {
    format!("//{package}:{name}")
}

/// A dependency as `package()`, `files()` and `task(dependencies = ...)` take it (decision 0028), as its canonical form:
/// a package, `//libs/core`; a set of files or a task of a package, `//libs/core:contracts`; or `:contracts`,
/// of the package `package`.
pub fn dependency_edge(package: &str, text: &str) -> Result<String, String> {
    if let Some(name) = text.strip_prefix(':') {
        return Ok(label(package, name));
    }
    if parse_label(text).is_some() {
        return Ok(text.to_owned());
    }
    let valid = text.strip_prefix("//").is_some_and(|dir| {
        !dir.is_empty() && !dir.contains(':') && dir.split('/').all(|s| !s.is_empty() && s != PARENT_DIR)
    });
    match valid {
        true => Ok(text.to_owned()),
        false => Err(format!("{text:?} is not a dependency such as \"//libs/core\" or \"//libs/core:contracts\"")),
    }
}

/// A dependency as its package directory, and the name of a set of files or a task of it; None for the package's
/// own files.
pub fn split_dependency(edge: &str) -> (&str, Option<&str>) {
    match parse_label(edge) {
        Some((package, name)) => (package, Some(name)),
        None => (edge.trim_start_matches("//"), None),
    }
}

/// The start of a pattern of files of the workspace, not of the package: `//libs/core/**` (decision 0026).
pub const WORKSPACE_PATTERN_PREFIX: &str = "//";
const GLOB_CHARACTERS: [char; 4] = ['*', '?', '[', '{'];
const PARENT_DIR: &str = "..";

/// A pattern of files of the workspace as the directory before its first wildcard and the pattern below it:
/// `//libs/core/**/*.cs` is `("libs/core", "**/*.cs")`, and `//config/shared.json` is `("config", "shared.json")`.
/// None for a pattern of the package, and an error for a workspace pattern that names no file or leaves the workspace.
pub fn split_workspace_pattern(pattern: &str) -> Option<Result<(String, String), String>> {
    let rest = pattern.strip_prefix(WORKSPACE_PATTERN_PREFIX)?;
    let segments: Vec<&str> = rest.split('/').collect();
    if rest.is_empty() || segments.iter().any(|s| s.is_empty() || *s == PARENT_DIR) {
        return Some(Err(format!("{pattern:?} is not a pattern of files below the workspace's root")));
    }
    let first_wildcard = segments.iter().position(|s| s.contains(GLOB_CHARACTERS)).unwrap_or(segments.len() - 1);
    Some(Ok((segments[..first_wildcard].join("/"), segments[first_wildcard..].join("/"))))
}

/// The part of a pattern label that stands for a directory and every directory below it (decision 0023).
const PATTERN_WILDCARD: &str = "...";

/// Splits a pattern `//dir/...:name` into the directory `dir` and the name; `//...:name` gives the root, `""`.
pub fn parse_pattern(label: &str) -> Option<(&str, &str)> {
    let (package, name) = parse_label(label)?;
    if package == PATTERN_WILDCARD {
        return Some(("", name));
    }
    let dir = package.strip_suffix(PATTERN_WILDCARD)?.strip_suffix('/')?;
    (!dir.is_empty() && !dir.split('/').any(|part| part == PATTERN_WILDCARD)).then_some((dir, name))
}

/// The label of the pattern of a directory and the name: `//dir/...:name`, or `//...:name` at the root.
pub fn pattern(dir: &str, name: &str) -> String {
    match dir.is_empty() {
        true => format!("//{PATTERN_WILDCARD}:{name}"),
        false => format!("//{dir}/{PATTERN_WILDCARD}:{name}"),
    }
}

/// Whether the package `package` is the directory `dir` or below it.
pub fn is_at_or_below(package: &str, dir: &str) -> bool {
    dir.is_empty() || package == dir || package.strip_prefix(dir).is_some_and(|rest| rest.starts_with('/'))
}

/// Splits `//dir:name` into the package `dir` and the name.
pub fn parse_label(label: &str) -> Option<(&str, &str)> {
    let rest = label.strip_prefix("//")?;
    let (package, name) = rest.rsplit_once(':')?;
    let valid = !name.is_empty() && !package.starts_with('/') && !package.ends_with('/') && !package.contains("//");
    valid.then_some((package, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_parse() {
        assert_eq!(parse_label("//services/orders:check"), Some(("services/orders", "check")));
        assert_eq!(parse_label("//:check"), Some(("", "check")));
        assert_eq!(parse_label("services:check"), None);
        assert_eq!(parse_label("//a//b:c"), None);
        assert_eq!(parse_label("//a:"), None);
        assert_eq!(label("a/b", "c"), "//a/b:c");
    }

    #[test]
    fn a_workspace_pattern_is_its_directory_and_the_pattern_below_it() {
        let split = |pattern: &str| split_workspace_pattern(pattern).map(|r| r.map_err(|_| ()));
        let ok = |dir: &str, rest: &str| Some(Ok((dir.to_owned(), rest.to_owned())));
        assert_eq!(split("//libs/core/**"), ok("libs/core", "**"));
        assert_eq!(split("//libs/core/**/*.cs"), ok("libs/core", "**/*.cs"));
        assert_eq!(split("//config/shared.json"), ok("config", "shared.json"));
        assert_eq!(split("//*.json"), ok("", "*.json"));
        assert_eq!(split("src/**"), None);
        assert_eq!(split("//"), Some(Err(())));
        assert_eq!(split("//a//b"), Some(Err(())));
        assert_eq!(split("//../outside/**"), Some(Err(())));
    }

    #[test]
    fn patterns_name_a_directory_and_everything_below_it() {
        assert_eq!(parse_pattern("//...:fmt"), Some(("", "fmt")));
        assert_eq!(parse_pattern("//libs/...:fmt"), Some(("libs", "fmt")));
        assert_eq!(parse_pattern("//libs/core:fmt"), None);
        assert_eq!(parse_pattern("//libs...:fmt"), None);
        assert_eq!(parse_pattern("//.../x/...:fmt"), None);
        assert_eq!(pattern("", "fmt"), "//...:fmt");
        assert_eq!(pattern("libs", "fmt"), "//libs/...:fmt");
        assert!(is_at_or_below("libs/core", "libs"));
        assert!(is_at_or_below("libs", "libs"));
        assert!(!is_at_or_below("libs-old", "libs"));
        assert!(is_at_or_below("apps/web", ""));
    }

    #[test]
    fn a_path_in_an_action_is_read_as_a_path_in_the_workspace() {
        let isolated = WorkspaceView { root: "/work".to_owned(), start: "services/orders".to_owned() };
        assert_eq!(isolated.workspace_path("/work/crates/a.rs").as_deref(), Some("crates/a.rs"));
        assert_eq!(isolated.workspace_path("src/main.go").as_deref(), Some("services/orders/src/main.go"));
        assert_eq!(isolated.workspace_path("./src/../go.mod").as_deref(), Some("services/orders/go.mod"));
        assert_eq!(isolated.workspace_path("../../README.md").as_deref(), Some("README.md"));
    }

    #[test]
    fn a_path_outside_the_workspace_has_no_workspace_path() {
        let isolated = WorkspaceView { root: "/work".to_owned(), start: String::new() };
        assert_eq!(isolated.workspace_path("/workspace/a.rs"), None);
        assert_eq!(isolated.workspace_path("/usr/include/stdio.h"), None);
        assert_eq!(isolated.workspace_path("/work/../etc/passwd"), None);
        assert_eq!(isolated.workspace_path("../outside.rs"), None);
        assert_eq!(isolated.workspace_path("/work"), None);
        let host = WorkspaceView { root: "/home/runner/work/app/app".to_owned(), start: String::new() };
        assert_eq!(host.workspace_path("/home/runner/work/app/app/src/a.rs").as_deref(), Some("src/a.rs"));
        assert_eq!(host.workspace_path("/home/runner/work/_temp/a.rs"), None);
    }
}
