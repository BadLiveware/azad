//! The functions that `tasks.star` files and modules can call.
// Each keyword argument of a Starlark function is a parameter of the Rust function that `starlark_module` makes.
#![allow(clippy::too_many_arguments)]

use std::collections::{BTreeMap, BTreeSet};

use crate::ir::graph::is_valid_name;
use crate::ir::{Kind, Spec};
use starlark::codemap::FileSpan;
use starlark::environment::GlobalsBuilder;
use starlark::eval::Evaluator;
use starlark::starlark_module;
use starlark::values::dict::UnpackDictEntries;
use starlark::values::list::{ListRef, UnpackList};
use starlark::values::none::NoneOr;
use starlark::values::{UnpackValue, Value, ValueLike};

use crate::frontend::starlark::git::git_functions;
use crate::frontend::starlark::semver::semver_functions;
use crate::frontend::starlark::values::{ActionValue, Collector, ModuleDef, ModuleSchema, Returns, SourceValue};
use crate::frontend::{
    Declarations, Dependencies, FileSelection, FileSet, GOAL_EVENTS, LocalRun, SourceSpec, Task, dependency_edge,
    label, parse_label, split_dependency, split_workspace_pattern,
};
use crate::store::Selection;

pub(crate) fn collector<'a>(eval: &'a Evaluator<'_, '_, '_>, function: &str) -> anyhow::Result<&'a Collector> {
    eval.extra
        .and_then(|extra| extra.downcast_ref::<Collector>())
        .ok_or_else(|| anyhow::anyhow!("{function}() can be called only while a tasks.star file is evaluated"))
}

fn source(value: Value<'_>, what: &str) -> anyhow::Result<SourceSpec> {
    match value.downcast_ref::<SourceValue>() {
        Some(source) => Ok(source.spec.clone()),
        None => anyhow::bail!("{what} must be a source (glob(), image() or <action>.out()), not {}", value.get_type()),
    }
}

const SHELL: &str = "sh";
const SHELL_COMMAND_FLAG: &str = "-c";
/// Dependencies in their canonical form; a package does not depend on itself.
fn dependency_edges(package: &str, edges: Vec<String>, function: &str) -> anyhow::Result<Vec<String>> {
    edges
        .into_iter()
        .map(|edge| {
            let edge = dependency_edge(package, &edge).map_err(|e| anyhow::anyhow!("{function}(): {e}"))?;
            if split_dependency(&edge) == (package, None) {
                anyhow::bail!("{function}(): a package does not depend on itself, as {edge:?} would");
            }
            Ok(edge)
        })
        .collect()
}

/// The `dependencies` of a task: `True`, `False`, or a list of dependencies.
fn task_dependencies(package: &str, task: &str, value: Option<Value>) -> anyhow::Result<Dependencies> {
    let Some(value) = value.filter(|value| !value.is_none()) else { return Ok(Dependencies::None) };
    if let Some(all) = value.unpack_bool() {
        return Ok(match all {
            true => Dependencies::Package,
            false => Dependencies::None,
        });
    }
    let Some(list) = ListRef::from_value(value) else {
        anyhow::bail!(
            "task({task:?}): dependencies is True, False or a list of dependencies, not {}",
            value.get_type()
        );
    };
    let edges = list
        .iter()
        .map(|item| {
            item.unpack_str()
                .map(str::to_owned)
                .ok_or_else(|| anyhow::anyhow!("task({task:?}): dependencies hold text"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(Dependencies::Only(dependency_edges(package, edges, "task")?))
}

/// The label of what a goal requires: an action or task value, or the name or label of one that this package defined.
fn required_label(collector: &Collector, goal: &str, value: Value) -> anyhow::Result<String> {
    if let Some(action) = value.downcast_ref::<ActionValue>() {
        return Ok(action.label.clone());
    }
    let Some(text) = value.unpack_str() else {
        anyhow::bail!("goal({goal:?}): require must hold actions, tasks or their names, not {}", value.get_type());
    };
    let required = match parse_label(text) {
        Some(_) => text.to_owned(),
        None => label(&collector.package, text),
    };
    let defined = collector.defined.borrow();
    if !defined.tasks.contains_key(&required) && !defined.actions.contains_key(&required) {
        anyhow::bail!("goal({goal:?}): {required} is not a task or an action of this package defined before the goal");
    }
    Ok(required)
}

/// The label of a task in `deps`: the value of a `task()`, or the label of a task, such as one of another package.
fn dep_label(task: &str, value: Value) -> anyhow::Result<String> {
    if let Some(dep) = value.downcast_ref::<ActionValue>() {
        return Ok(dep.label.clone());
    }
    let Some(text) = value.unpack_str() else {
        anyhow::bail!("task({task:?}): deps must hold tasks or their labels, not {}", value.get_type());
    };
    if parse_label(text).is_none() {
        anyhow::bail!("task({task:?}): {text:?} in deps is not a label such as \"//libs/logging:build\"");
    }
    Ok(text.to_owned())
}

/// Why a goal cannot share a name with a task or an action: a target names the goal first.
const GOAL_HIDES: &str = "and a goal would hide a task or an action of its name; give them different names";

/// A command: a list of words, or one line of text that the shell runs.
fn command(value: Value<'_>, function: &str) -> anyhow::Result<Vec<String>> {
    if let Some(line) = value.unpack_str() {
        return Ok(vec![SHELL.to_owned(), SHELL_COMMAND_FLAG.to_owned(), line.to_owned()]);
    }
    let words = UnpackList::<String>::unpack_value(value).ok().flatten().ok_or_else(|| {
        anyhow::anyhow!("{function}(): cmd must be a string or a list of strings, not {}", value.get_type())
    })?;
    Ok(words.items)
}

/// Where a built-in is called, as `<file>:<line>`; when a function of another file calls it, such as a module's
/// function that a `tasks.star` calls, the line of that outermost call too.
fn call_site(eval: &Evaluator<'_, '_, '_>) -> Option<String> {
    let places: Vec<FileSpan> = eval.call_stack().frames.into_iter().filter_map(|frame| frame.location).collect();
    let (outermost, innermost) = (places.first()?, places.last()?);
    let place = |span: &FileSpan| format!("{}:{}", span.filename(), span.resolve_span().begin.line + 1);
    match outermost.filename() == innermost.filename() {
        true => Some(place(innermost)),
        false => Some(format!("{}, called from {}", place(innermost), place(outermost))),
    }
}

struct ActionArgs<'v> {
    name: String,
    cmd: Vec<String>,
    root: Value<'v>,
    inputs: Vec<(String, Value<'v>)>,
    outputs: Vec<String>,
    env: Vec<(String, String)>,
    caches: Vec<(String, String)>,
    tags: Vec<String>,
}

const TAG_CHARACTERS: &str = "letters, digits, '-', '_' and '.'";

/// The tags of a task or action: its own, and those that the `config.star` files above its package give (decision
/// 0023).
fn tag_set(collector: &Collector, own: Vec<String>, function: &str) -> anyhow::Result<BTreeSet<String>> {
    if let Some(bad) = own.iter().find(|tag| !is_valid_tag(tag)) {
        anyhow::bail!("{function}(): the tag {bad:?} may hold only {TAG_CHARACTERS}");
    }
    Ok(collector.default_tags.iter().cloned().chain(own).collect())
}

/// An image by its reference, which must name a digest; exactly one of a reference and a Dockerfile.
fn image_by_reference(
    reference: Option<&str>,
    dockerfile: Option<&str>,
    env: BTreeMap<String, String>,
) -> anyhow::Result<SourceValue> {
    let reference = match (reference, dockerfile) {
        (Some(reference), None) => reference,
        _ => anyhow::bail!("image() takes a reference or a dockerfile, and not both"),
    };
    if !reference.contains("@sha256:") {
        anyhow::bail!(
            "image({reference:?}) must name a digest (<name>@sha256:<hex>), so that a moved tag cannot change it; \
             `azad pull {reference}` prints the reference with its digest"
        );
    }
    Ok(SourceValue { spec: SourceSpec::Image { reference: reference.to_owned(), env } })
}

const WORKSPACE_PREFIX: &str = "//";

/// A path of the workspace, as `//path` names it, without the prefix.
fn workspace_file(path: &str, what: &str) -> anyhow::Result<String> {
    let Some(relative) = path.strip_prefix(WORKSPACE_PREFIX) else {
        anyhow::bail!("image(): {what} {path:?} is a path of the workspace, from its root: //{path}");
    };
    if relative.split('/').any(|part| part == ".." || part == ".") {
        anyhow::bail!("image(): {what} {path:?} must be normalized and inside the workspace");
    }
    Ok(relative.trim_end_matches('/').to_owned())
}

/// The names of secrets: names of environment variables.
fn secret_names(names: Vec<String>, task: &str) -> anyhow::Result<BTreeSet<String>> {
    let valid = |name: &String| {
        name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    if let Some(bad) = names.iter().find(|name| !valid(name)) {
        anyhow::bail!("task({task:?}): the secret {bad:?} is not the name of an environment variable");
    }
    Ok(names.into_iter().collect())
}

pub(crate) fn is_valid_tag(tag: &str) -> bool {
    !tag.is_empty() && tag.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn declare(
    eval: &mut Evaluator<'_, '_, '_>,
    kind: Kind,
    args: ActionArgs<'_>,
    function: &str,
) -> anyhow::Result<ActionValue> {
    let collector = collector(eval, function)?;
    if !is_valid_name(&args.name) || args.name.contains(['/', ':']) {
        anyhow::bail!("{function}(): the name {:?} may hold only letters, digits, '_', '-' and '.'", args.name);
    }
    if args.cmd.is_empty() {
        anyhow::bail!("{function}(): cmd must not be empty");
    }
    let inputs = args
        .inputs
        .into_iter()
        .map(|(path, value)| source(value, &format!("input {path:?}")).map(|s| (path, s)))
        .collect::<anyhow::Result<BTreeMap<_, _>>>()?;
    let outputs: BTreeSet<String> = args.outputs.iter().cloned().collect();
    let spec = Spec {
        kind,
        platform: collector.platform.clone(),
        root: source(args.root, "root")?,
        command: args.cmd,
        env: args.env.into_iter().collect(),
        inputs,
        outputs,
        caches: args.caches.into_iter().collect(),
        work: BTreeMap::new(),
        dir: String::new(),
        secrets: BTreeSet::new(),
    };
    let action_label = label(&collector.package, &args.name);
    let tags = tag_set(collector, args.tags, function)?;
    let mut defined = collector.defined.borrow_mut();
    if defined.goals.contains_key(&action_label) {
        anyhow::bail!("{function}(): {action_label} is also a goal, {GOAL_HIDES}");
    }
    if defined.actions.insert(action_label.clone(), spec).is_some() {
        anyhow::bail!("{function}(): {action_label} is defined twice");
    }
    if !tags.is_empty() {
        defined.tags.insert(action_label.clone(), tags);
    }
    Ok(ActionValue { label: action_label, outputs: args.outputs })
}

/// The namespaces of values that are not actions: `semver` and `git` (decision 0021).
pub(crate) fn namespaces(builder: &mut GlobalsBuilder) {
    builder.namespace("semver", semver_functions);
    builder.namespace("git", git_functions);
}

#[starlark_module]
pub(crate) fn azad_globals(builder: &mut GlobalsBuilder) {
    /// A hermetic action: a command in a sandbox, with no network, that runs again only when its inputs change.
    ///
    /// The sandbox holds the root file system, the inputs (read-only) and the cache volumes.
    /// The command starts in `/work`, an empty directory that it can write.
    /// The directories of `outputs` are kept; the other files that the command writes are discarded.
    /// The result is cached by the digest of the action: its command, environment, root and inputs.
    ///
    /// # Arguments
    /// * `name`: The name of the action in its package, which gives the label `//<package>:<name>`.
    ///
    ///   It can hold letters, digits, `_`, `-` and `.`.
    /// * `cmd`: The program and its arguments.
    ///
    ///   No shell starts it; use `["sh", "-c", "..."]` for a shell command.
    /// * `root`: The root file system, usually an `image()`.
    /// * `inputs`: The sources to mount, read-only, by absolute path in the sandbox, such as `{"/src": glob(["**"])}`.
    ///
    ///   A source is a `glob()`, an `image()`, or an output of another action (`<action>.out("bin")`).
    ///   An action that reads the output of another action runs after it.
    /// * `outputs`: The directories that the command writes and that are kept, relative to `/work`, such as `["bin"]`.
    ///
    ///   Other actions read them with `<action>.out("bin")`.
    /// * `env`: The environment variables.
    ///
    ///   The environment of the image is the base; a variable here replaces the variable of the image of that name.
    /// * `caches`: The cache volumes, by absolute path in the sandbox, such as `{"/cache/go": "go-build"}`.
    ///
    ///   A cache volume is a writable directory that a tool keeps between runs, such as the build cache of a compiler.
    ///   Its content is not part of the digest, so it must not change the result (decision 0011).
    /// * `tags`: Names that `azad run --include` and `--exclude` select by, such as `dotnet` (decision 0023). The
    ///   `tags` of the `config.star` files above the package are added. Tags are not part of the digest.
    ///
    /// Examples:
    /// ```python
    /// go = image("golang:1.25@sha256:<hex>")
    /// build = action(
    ///     name = "build",
    ///     cmd = ["sh", "-c", "cd /src && go build -o /work/bin/ ./..."],
    ///     root = go,
    ///     inputs = {"/src": glob(["**/*.go", "go.mod", "go.sum"])},
    ///     outputs = ["bin"],
    ///     caches = {"/root/.cache/go-build": "go-build"},
    /// )
    /// ```
    fn action<'v>(
        #[starlark(require = named)] name: &str,
        #[starlark(require = named)] cmd: UnpackList<String>,
        #[starlark(require = named)] root: Value<'v>,
        #[starlark(require = named, default = UnpackDictEntries::default())] inputs: UnpackDictEntries<
            String,
            Value<'v>,
        >,
        #[starlark(require = named, default = UnpackList::default())] outputs: UnpackList<String>,
        #[starlark(require = named, default = UnpackDictEntries::default())] env: UnpackDictEntries<String, String>,
        #[starlark(require = named, default = UnpackDictEntries::default())] caches: UnpackDictEntries<String, String>,
        #[starlark(require = named, default = UnpackList::default())] tags: UnpackList<String>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<ActionValue> {
        let args = ActionArgs {
            name: name.to_owned(),
            cmd: cmd.items,
            root,
            inputs: inputs.entries,
            outputs: outputs.items,
            env: env.entries,
            caches: caches.entries,
            tags: tags.items,
        };
        declare(eval, Kind::Hermetic, args, "action")
    }

    /// A fetch: an action that can use the network to download dependencies, cached by the digest of its inputs.
    ///
    /// The inputs must hold the lock files that pin what it downloads (decision 0007).
    /// So the same inputs give the same files.
    ///
    /// # Arguments
    /// * `name`: As for `action()`.
    /// * `cmd`: As for `action()`.
    /// * `root`: As for `action()`.
    /// * `inputs`: As for `action()`, and it must hold the lock files, such as `go.sum`.
    /// * `outputs`: As for `action()`.
    /// * `env`: As for `action()`.
    /// * `caches`: As for `action()`.
    /// * `tags`: As for `action()`.
    fn fetch<'v>(
        #[starlark(require = named)] name: &str,
        #[starlark(require = named)] cmd: UnpackList<String>,
        #[starlark(require = named)] root: Value<'v>,
        #[starlark(require = named)] inputs: UnpackDictEntries<String, Value<'v>>,
        #[starlark(require = named, default = UnpackList::default())] outputs: UnpackList<String>,
        #[starlark(require = named, default = UnpackDictEntries::default())] env: UnpackDictEntries<String, String>,
        #[starlark(require = named, default = UnpackDictEntries::default())] caches: UnpackDictEntries<String, String>,
        #[starlark(require = named, default = UnpackList::default())] tags: UnpackList<String>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<ActionValue> {
        if inputs.entries.is_empty() {
            anyhow::bail!("fetch(): inputs must hold the lock files that pin what it downloads");
        }
        let args = ActionArgs {
            name: name.to_owned(),
            cmd: cmd.items,
            root,
            inputs: inputs.entries,
            outputs: outputs.items,
            env: env.entries,
            caches: caches.entries,
            tags: tags.items,
        };
        declare(eval, Kind::Fetch, args, "fetch")
    }

    /// An impure action: an action that can use the network, and that runs each time a goal needs it.
    ///
    /// Use it for work whose result does not depend only on its inputs, such as a deploy.
    /// Its outputs are stored by digest, so an action that reads them is a cache hit when they did not change.
    ///
    /// # Arguments
    /// * `name`: As for `action()`.
    /// * `cmd`: As for `action()`.
    /// * `root`: As for `action()`.
    /// * `inputs`: As for `action()`.
    /// * `outputs`: As for `action()`.
    /// * `env`: As for `action()`.
    /// * `caches`: As for `action()`.
    /// * `tags`: As for `action()`.
    fn shell<'v>(
        #[starlark(require = named)] name: &str,
        #[starlark(require = named)] cmd: UnpackList<String>,
        #[starlark(require = named)] root: Value<'v>,
        #[starlark(require = named, default = UnpackDictEntries::default())] inputs: UnpackDictEntries<
            String,
            Value<'v>,
        >,
        #[starlark(require = named, default = UnpackList::default())] outputs: UnpackList<String>,
        #[starlark(require = named, default = UnpackDictEntries::default())] env: UnpackDictEntries<String, String>,
        #[starlark(require = named, default = UnpackDictEntries::default())] caches: UnpackDictEntries<String, String>,
        #[starlark(require = named, default = UnpackList::default())] tags: UnpackList<String>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<ActionValue> {
        let args = ActionArgs {
            name: name.to_owned(),
            cmd: cmd.items,
            root,
            inputs: inputs.entries,
            outputs: outputs.items,
            env: env.entries,
            caches: caches.entries,
            tags: tags.items,
        };
        declare(eval, Kind::Impure, args, "shell")
    }

    /// A task (decision 0015): work described by the files of the workspace, which runs on the host locally and
    /// in its image in CI.
    ///
    /// A local run runs the command on this host, with its tools and environment, in the package directory, and the
    /// task writes its files there.
    /// `--isolated`, which CI uses, runs it in `image`, in a sandbox whose work directory holds only the task's
    /// `sources` and what its `deps` generate, at the same paths as in the workspace.
    /// Both forms skip the task when its command, `env`, sources and the output of its deps did not change since it
    /// last succeeded; a local run also runs it again when a directory that it generates was changed or removed.
    /// A task without an image runs on the host in both, as CI runs it on its runner; use it for work that needs the
    /// host, such as tests that start sandboxes.
    ///
    /// # Arguments
    /// * `name`: The name of the task in its package.
    /// * `cmd`: One line that the shell runs, or a list of words: the program and its arguments.
    /// * `sources`: Patterns of the files that the task reads, relative to the package, as for `glob()`; or, after
    ///   `//`, relative to the workspace's root, such as `//libs/core/**` for a project that this one builds from
    ///   source (decision 0026). An isolated run has them at the same paths, and they are part of the digest.
    /// * `exclude`: Patterns of files to leave out of `sources`, in the same two forms.
    /// * `generates`: Directories that the task writes, relative to the package, for the tasks after it.
    /// * `deps`: Tasks that must succeed first; the task reads what they generate, at their paths. A task of this file
    ///   is the value of its `task()`; a task of another package is its label, such as `"//libs/logging:build"`,
    ///   whose package Azad reads too (decision 0026).
    /// * `image`: The image of an isolated run, from `image()`.
    /// * `env`: Variables of the command; on the host, they replace the host's variables of the same name.
    /// * `caches`: State that the task's tools keep between runs, by path, and the name of each.
    ///   A path relative to the package, such as `target`, is that directory in both forms: on the host, the real one,
    ///   and in the image, a cache volume at the same place.
    ///   An absolute path is a cache volume at that path of the sandbox, such as a tool's download directory; on the
    ///   host, the tool uses its own.
    ///   A new machine, such as a CI runner, starts each from its newest copy in the remote cache (decision 0017).
    /// * `network`: Whether an isolated run has network access, as `fetch()` has.
    /// * `tags`: Names that `azad run --include` and `--exclude` select by, such as `dotnet` or `emulator` (decision
    ///   0023). The `tags` of the `config.star` files above the package are added. Tags are not part of the digest.
    /// * `local`: Where a local run runs the task: `"host"`, on this host's tools, or `"image"`, in its image through
    ///   Docker or Podman, on the files of the workspace (decision 0024). The default is `local` of the nearest
    ///   `config.star`, or `"host"`. `azad run --local` and the personal defaults can choose for every task.
    /// * `secrets`: Variables of `azad`'s environment that the command gets in its image, by name, such as a registry's
    ///   token. Their values are not part of the digest. On the host, the command has the whole environment.
    /// * `doc`: What the task is for.
    /// * `dependencies`: The dependencies that the task reads (decision 0028): `True` for those of its `package()`, a
    ///   list of them, such as `["//libs/core:contracts"]`, or `False`, the default. It gets each one's files and
    ///   provided outputs at their workspace paths, and those of their dependencies, which run first; all of them are
    ///   part of its digest.
    ///
    /// Examples:
    /// ```python
    /// build = task(name = "build", cmd = "go build -o out/app .", sources = ["**/*.go", "go.*"], generates = ["out"], image = go)
    /// test = task(name = "test", cmd = "out/app --self-test", deps = [build], image = go)
    /// goal("check", require = [test])
    /// ```
    fn task<'v>(
        #[starlark(require = named)] name: &str,
        #[starlark(require = named)] cmd: Value<'v>,
        #[starlark(require = named, default = UnpackList::default())] sources: UnpackList<String>,
        #[starlark(require = named, default = UnpackList::default())] exclude: UnpackList<String>,
        #[starlark(require = named, default = UnpackList::default())] generates: UnpackList<String>,
        #[starlark(require = named, default = UnpackList::default())] deps: UnpackList<Value<'v>>,
        #[starlark(require = named)] image: Option<Value<'v>>,
        #[starlark(require = named, default = UnpackDictEntries::default())] env: UnpackDictEntries<String, String>,
        #[starlark(require = named, default = UnpackDictEntries::default())] caches: UnpackDictEntries<String, String>,
        #[starlark(require = named, default = false)] network: bool,
        #[starlark(require = named, default = UnpackList::default())] tags: UnpackList<String>,
        #[starlark(require = named, default = NoneOr::None)] local: NoneOr<&str>,
        #[starlark(require = named, default = UnpackList::default())] secrets: UnpackList<String>,
        #[starlark(require = named, default = "")] doc: &str,
        #[starlark(require = named)] dependencies: Option<Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<ActionValue> {
        let collector = collector(eval, "task")?;
        let dependencies = task_dependencies(&collector.package, name, dependencies)?;
        let declared_at = call_site(eval);
        if !is_valid_name(name) || name.contains(['/', ':']) {
            anyhow::bail!("task(): the name {name:?} may hold only letters, digits, '_', '-' and '.'");
        }
        let command = command(cmd, "task")?;
        if command.is_empty() {
            anyhow::bail!("task(): cmd must not be empty");
        }
        let deps = deps.items.into_iter().map(|value| dep_label(name, value)).collect::<anyhow::Result<Vec<_>>>()?;
        for pattern in sources.items.iter().chain(&exclude.items) {
            if let Some(Err(problem)) = split_workspace_pattern(pattern) {
                anyhow::bail!("task({name:?}): {problem}");
            }
        }
        let image = image.filter(|value| !value.is_none()).map(|value| source(value, "image")).transpose()?;
        let dir = collector.dir.clone();
        let outputs = generates
            .items
            .iter()
            .map(|path| match dir.is_empty() {
                true => path.clone(),
                false => format!("{dir}/{path}"),
            })
            .collect();
        let task = Task {
            platform: collector.platform.clone(),
            dir,
            command,
            sources: sources.items,
            exclude: exclude.items,
            generates: generates.items,
            deps,
            image,
            env: env.entries.into_iter().collect(),
            caches: caches.entries.into_iter().collect(),
            network,
            local: match local.into_option() {
                Some(text) => LocalRun::parse(text)
                    .ok_or_else(|| anyhow::anyhow!("task({name:?}): local is \"host\" or \"image\", not {text:?}"))?,
                None => collector.default_local,
            },
            secrets: secret_names(secrets.items, name)?,
            doc: doc.to_owned(),
            declared_at,
            dependencies,
        };
        let task_label = label(&collector.package, name);
        let tags = tag_set(collector, tags.items, "task")?;
        let mut defined = collector.defined.borrow_mut();
        if defined.goals.contains_key(&task_label) {
            anyhow::bail!("task(): {task_label} is also a goal, {GOAL_HIDES}");
        }
        if defined.actions.contains_key(&task_label) || defined.tasks.insert(task_label.clone(), task).is_some() {
            anyhow::bail!("task(): {task_label} is defined twice");
        }
        if !tags.is_empty() {
            defined.tags.insert(task_label.clone(), tags);
        }
        Ok(ActionValue { label: task_label, outputs })
    }

    /// The files of the package that match a pattern of `include` and no pattern of `exclude`, as a source.
    ///
    /// The patterns are relative to the package directory, the directory of the `tasks.star`.
    /// `*` does not match `/`; `**` does.
    /// The files keep their paths relative to the package: at `/src`, `glob(["cmd/*.go"])` gives `/src/cmd/main.go`.
    /// Only files and links are selected; a directory is there when it holds a selected file.
    /// A glob that matches no file gives a warning when the goal loads.
    ///
    /// # Arguments
    /// * `include`: The patterns of the files to select, such as `["**/*.go", "go.mod"]`.
    /// * `exclude`: The patterns of the files to leave out, such as `["**/*_test.go"]`.
    ///
    /// Examples:
    /// ```python
    /// src = glob(["**/*.go", "go.mod", "go.sum"], exclude = ["**/*_test.go"])
    /// ```
    fn glob<'v>(
        include: UnpackList<String>,
        #[starlark(require = named, default = UnpackList::default())] exclude: UnpackList<String>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<SourceValue> {
        let collector = collector(eval, "glob")?;
        let spec = SourceSpec::Glob { dir: collector.dir.clone(), include: include.items, exclude: exclude.items };
        if let Some(at) = call_site(eval) {
            collector.defined.borrow_mut().declared_at.entry(spec.to_string()).or_insert(at);
        }
        Ok(SourceValue { spec })
    }

    /// The paths of the files that `glob()` with the same patterns selects, relative to the package, sorted.
    ///
    /// A module can make one action per file with it.
    ///
    /// # Arguments
    /// * `include`: As for `glob()`.
    /// * `exclude`: As for `glob()`.
    fn files<'v>(
        include: UnpackList<String>,
        #[starlark(require = named, default = UnpackList::default())] exclude: UnpackList<String>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Vec<String>> {
        let collector = collector(eval, "files")?;
        let selection = Selection::globs(&include.items, &exclude.items)?;
        let listed = selection.list(&collector.root.join(&collector.dir))?;
        Ok(listed.into_iter().map(|path| path.to_string_lossy().into_owned()).collect())
    }

    /// An OCI (Open Container Initiative) image, as a source: by a reference with its digest, or built from a
    /// Dockerfile of the workspace.
    ///
    /// A tag can move to another image, so a reference must hold the digest; a tag in it is only for people.
    /// `azad pull <name>:<tag>` prints the reference of what the tag points to now.
    /// Azad pulls the image for the platform of the host.
    /// The `Env` of the image is the base environment of an action that has the image as its root (decision 0016);
    /// `env` goes on top of it, and the action's `env` on top of both.
    ///
    /// An image from a Dockerfile is built by the host's container runtime, Docker or Podman, with every feature that
    /// it has (decision 0024). A task that runs in its image locally runs in the runtime; an isolated run stores the
    /// image's file system, once for each content, and runs the task in Azad's sandbox. A run of a pull request does not
    /// build one, because a build runs the Dockerfile's steps with the host's runtime.
    ///
    /// Choose a `target` that holds the toolchain alone, such as a `base` stage with the language and its package
    /// manager. A stage that also installs the project's dependencies or builds it does that work in every new build,
    /// stores its result in the image, and gives tasks that do the same work again in their own sandbox.
    ///
    /// # Arguments
    /// * `reference`: The reference, such as `python:3.13-slim@sha256:<hex>` or `ghcr.io/owner/tool@sha256:<hex>`.
    /// * `env`: Variables that using the image needs, such as a writable `CARGO_HOME`. A task gets them only when it
    ///   runs in the image, not on the host.
    /// * `dockerfile`: Instead of `reference`, a Dockerfile of the workspace, by its path from the root, such as
    ///   `//tools/dotnet.Dockerfile`.
    /// * `context`: The build context of the Dockerfile, from the root; its directory when it is not given.
    /// * `target`: The stage of the Dockerfile to build; the last one when it is not given.
    /// * `contexts`: BuildKit's named build contexts, which `COPY --from=<name>` reads, by name, as paths from the
    ///   root, such as `{"src-logging": "//libs/logging"}`; what `contexts` of `docker buildx bake` gives.
    /// * `args`: The build arguments.
    ///
    /// Examples:
    /// ```python
    /// python = image("python:3.13-slim@sha256:2d44738ebf4ce5f63b9d9ea871e9c7cfdc0426f7b0c97cac915fd88ded1d86bb")
    /// tools = image(dockerfile = "//tools/azad/dotnet.Dockerfile", target = "format")
    /// ```
    fn image(
        #[starlark(default = NoneOr::None)] reference: NoneOr<&str>,
        #[starlark(require = named, default = UnpackDictEntries::default())] env: UnpackDictEntries<String, String>,
        #[starlark(require = named, default = NoneOr::None)] dockerfile: NoneOr<&str>,
        #[starlark(require = named, default = NoneOr::None)] context: NoneOr<&str>,
        #[starlark(require = named, default = "")] target: &str,
        #[starlark(require = named, default = UnpackDictEntries::default())] args: UnpackDictEntries<String, String>,
        #[starlark(require = named, default = UnpackDictEntries::default())] contexts: UnpackDictEntries<
            String,
            String,
        >,
    ) -> anyhow::Result<SourceValue> {
        let env = env.entries.into_iter().collect();
        let (reference, dockerfile) = (reference.into_option(), dockerfile.into_option());
        let (Some(file), None) = (dockerfile, reference) else {
            return image_by_reference(reference, dockerfile, env);
        };
        let file = workspace_file(file, "dockerfile")?;
        let context = match context.into_option() {
            Some(context) => workspace_file(context, "context")?,
            None => file.rsplit_once('/').map(|(dir, _)| dir.to_owned()).unwrap_or_default(),
        };
        let args = args.entries.into_iter().collect();
        let contexts = contexts
            .entries
            .into_iter()
            .map(|(name, path)| {
                if name.is_empty() || name.contains(['=', ',']) {
                    anyhow::bail!("image(): {name:?} is not the name of a build context");
                }
                Ok((name, workspace_file(&path, "contexts")?))
            })
            .collect::<anyhow::Result<_>>()?;
        let target = target.to_owned();
        Ok(SourceValue { spec: SourceSpec::Dockerfile { file, context, target, args, contexts, env } })
    }

    /// What the package says of itself for the packages that depend on it (decision 0028); a file calls it at most
    /// once.
    ///
    /// # Arguments
    /// * `deps`: The package's immediate dependencies: packages, such as `"//libs/core"`, or the sets of files and
    ///   tasks of packages, such as `"//libs/core:contracts"`. A task with `dependencies = True` reads them, and
    ///   what they depend on, so a package names only its own dependencies.
    /// * `exports`: Patterns of the files that the package gives the tasks that depend on it, relative to it, such as
    ///   `["**/*.cs", "**/*.csproj"]`; by default, all that git tracks or would track. Fewer files run fewer
    ///   dependents again.
    /// * `provides`: Tasks of the package, by name, whose outputs the tasks that depend on it get, such as the build of
    ///   a library; they run first.
    ///
    /// Examples:
    /// ```python
    /// package(deps = ["//libs/logging"], provides = ["build"])
    /// ```
    fn package<'v>(
        #[starlark(require = named, default = UnpackList::default())] deps: UnpackList<String>,
        #[starlark(require = named, default = UnpackList::default())] exports: UnpackList<String>,
        #[starlark(require = named, default = UnpackList::default())] provides: UnpackList<String>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<starlark::values::none::NoneType> {
        let collector = collector(eval, "package")?;
        if collector.declared_package.replace(true) {
            anyhow::bail!("package(): a file calls it once");
        }
        let deps = dependency_edges(&collector.package, deps.items, "package")?;
        if let Some(pattern) = exports.items.iter().find(|p| split_workspace_pattern(p).is_some()) {
            anyhow::bail!("package(): exports are patterns of the package's own files, not {pattern:?}");
        }
        let provides =
            provides.items.iter().map(|name| label(&collector.package, name.trim_start_matches(':'))).collect();
        collector.defined.borrow_mut().declared = Declarations { deps, exports: exports.items, provides };
        Ok(starlark::values::none::NoneType)
    }

    /// A named set of files of the package (decision 0028), which other packages can depend on in place of all of the
    /// package's files, such as `"//libs/core:contracts"`.
    ///
    /// # Arguments
    /// * `name`: The name of the set in its package.
    /// * `sources`: Patterns of its files, relative to the package, of the files that git tracks or would track.
    /// * `exclude`: Patterns of files to leave out.
    /// * `deps`: The dependencies that come with the set, as `package(deps = ...)` takes them.
    ///
    /// Examples:
    /// ```python
    /// file_set(name = "contracts", sources = ["Contracts/**"], deps = [":analyzers"])
    /// ```
    fn file_set<'v>(
        #[starlark(require = named)] name: &str,
        #[starlark(require = named)] sources: UnpackList<String>,
        #[starlark(require = named, default = UnpackList::default())] exclude: UnpackList<String>,
        #[starlark(require = named, default = UnpackList::default())] deps: UnpackList<String>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<starlark::values::none::NoneType> {
        let collector = collector(eval, "file_set")?;
        if !is_valid_name(name) || name.contains(['/', ':']) {
            anyhow::bail!("file_set(): the name {name:?} may hold only letters, digits, '_', '-' and '.'");
        }
        if let Some(pattern) = sources.items.iter().chain(&exclude.items).find(|p| split_workspace_pattern(p).is_some())
        {
            anyhow::bail!("file_set({name:?}): its patterns are of the package's own files, not {pattern:?}");
        }
        let deps = dependency_edges(&collector.package, deps.items, "file_set")?;
        let set_label = label(&collector.package, name);
        let selection = FileSelection { include: sources.items, exclude: exclude.items };
        let mut defined = collector.defined.borrow_mut();
        let taken = defined.tasks.contains_key(&set_label)
            || defined.actions.contains_key(&set_label)
            || defined.goals.contains_key(&set_label);
        if taken || defined.file_sets.contains_key(&set_label) {
            anyhow::bail!("file_set(): {set_label} is defined twice, or is also a task, an action or a goal");
        }
        defined.file_sets.insert(set_label, FileSet { dir: collector.dir.clone(), selection, deps });
        Ok(starlark::values::none::NoneType)
    }

    /// A module: a function with typed parameters, which frontends other than Starlark can call too (decision 0014).
    ///
    /// A call of the module calls `implementation` with the arguments of the call.
    /// A module that returns an action, the default, also gets the call's `name`, and the call must define an action
    /// with that name, which declares the outputs of the module.
    /// A module that returns parts gives `part()`s of `@std//project.star` for `project()`, and its call has no name;
    /// a `tasks.yaml` file calls it with `uses:` (decision 0027).
    /// A GHA workflow calls a module with `uses:` and `with:`, so each parameter type can also be given as text.
    ///
    /// # Arguments
    /// * `name`: The name of the module, such as `std/test.split`.
    /// * `params`: The type of each parameter, by name.
    ///
    ///   A type is `string`, `strings`, `int`, `bool`, `patterns` (patterns of files of the package) or `image` (from
    ///   `image()`, or a reference as text).
    ///   A type with `?` at the end, such as `strings?`, is optional.
    /// * `outputs`: The outputs that the action `name` of each call declares.
    /// * `returns`: What a call gives back: `"action"`, the action `name`, or `"parts"`.
    /// * `implementation`: The function that defines the actions of a call.
    ///
    /// Examples:
    /// ```python
    /// def _lint(name, files, ruff):
    ///     src = {"/src": glob(files)}
    ///     return action(name = name, cmd = ["ruff", "check", "/src"], root = image(ruff), inputs = src)
    ///
    /// lint = module(name = "lint", params = {"files": "patterns", "ruff": "string"}, implementation = _lint)
    /// ```
    fn module<'v>(
        #[starlark(require = named)] name: &str,
        #[starlark(require = named)] params: UnpackDictEntries<String, String>,
        #[starlark(require = named, default = UnpackList::default())] outputs: UnpackList<String>,
        #[starlark(require = named, default = "action")] returns: &str,
        #[starlark(require = named)] implementation: Value<'v>,
    ) -> anyhow::Result<ModuleDef<'v>> {
        let returns = Returns::parse(returns).map_err(|e| anyhow::anyhow!("module {name}: {e}"))?;
        let schema = ModuleSchema::new(name, params.entries, outputs.items, returns)?;
        Ok(ModuleDef { schema, implementation })
    }

    /// A goal of the package: it holds when each action in `require` succeeds.
    ///
    /// `azad run //<package>:<name>` runs what the goal needs.
    /// In the package directory, `azad run <name>` does the same.
    /// A task or action can also run by its own name, as if a goal required only it.
    ///
    /// # Arguments
    /// * `name`: The name of the goal, such as `check`.
    /// * `require`: The actions and tasks that must succeed: their values, or the names or labels of tasks and actions
    ///   of this package that are defined before the goal, such as those that a module made.
    ///
    ///   An action runs after the actions whose outputs it reads.
    /// * `doc`: What the goal is for, for `azad list`.
    /// * `on`: The events on which the orchestrator (`azad-orchestrator serve`) runs the goal: `push`, `pull_request`, or both.
    ///
    ///   A push to the default branch runs at the trust level `main`; a pull request, and a push to another branch,
    ///   at `pull-request`.
    ///
    /// Examples:
    /// ```python
    /// goal("check", require = [build, test], on = ["push", "pull_request"])
    /// ```
    fn goal<'v>(
        name: &str,
        #[starlark(require = named)] require: UnpackList<Value<'v>>,
        #[starlark(require = named, default = "")] doc: &str,
        #[starlark(require = named, default = UnpackList::default())] on: UnpackList<String>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<starlark::values::none::NoneType> {
        let collector = collector(eval, "goal")?;
        let actions = require
            .items
            .into_iter()
            .map(|value| required_label(collector, name, value))
            .collect::<anyhow::Result<BTreeSet<_>>>()?;
        if actions.is_empty() {
            anyhow::bail!("goal({name:?}): require must name at least one action");
        }
        let goal_label = label(&collector.package, name);
        let mut defined = collector.defined.borrow_mut();
        if defined.tasks.contains_key(&goal_label) || defined.actions.contains_key(&goal_label) {
            anyhow::bail!("goal(): {goal_label} is also a task or an action, {GOAL_HIDES}");
        }
        if defined.goals.insert(goal_label.clone(), actions).is_some() {
            anyhow::bail!("goal(): {goal_label} is defined twice");
        }
        if let Some(unknown) = on.items.iter().find(|event| !GOAL_EVENTS.contains(&event.as_str())) {
            anyhow::bail!("goal({name:?}): on has {unknown:?}, which is not one of {GOAL_EVENTS:?}");
        }
        if !on.items.is_empty() {
            defined.events.insert(goal_label.clone(), on.items.into_iter().collect());
        }
        if !doc.is_empty() {
            defined.docs.insert(goal_label, doc.to_owned());
        }
        Ok(starlark::values::none::NoneType)
    }
}
