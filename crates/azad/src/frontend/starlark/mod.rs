//! The Starlark frontend: evaluates `tasks.star` files to actions and goals.
//!
//! Evaluation is pure. A function such as `glob()` returns a reference; the engine stores the files after evaluation.

mod builtins;
mod decode;
mod git;
mod loader;
mod reference;
mod semver;
mod values;
mod workspace;
pub mod yaml;

pub use loader::{Context, EvalError, evaluate, evaluate_in, expand, expand_calls};
pub use reference::reference;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::frontend::{Frontend, FrontendError, Listed, LoadRequest, Loaded, parse_label};

/// The name of the file that defines the actions and goals of a package.
pub const TASKS_FILE: &str = "tasks.star";

/// Whether a file, by its name, makes its directory a package: a `tasks.star`, or a `tasks.yaml` (decision 0027).
pub fn is_tasks_file(name: &std::ffi::OsStr) -> bool {
    name == TASKS_FILE || name == yaml::TASKS_YAML_FILE
}

/// Whether a directory is a package: it holds a `tasks.star` or a `tasks.yaml`.
pub fn has_tasks_file(dir: &Path) -> bool {
    dir.join(TASKS_FILE).is_file() || dir.join(yaml::TASKS_YAML_FILE).is_file()
}
/// The name of the file that sets config for a directory and the directories below it.
pub const CONFIG_FILE: &str = "config.star";
const LABEL_PREFIX: &str = "//";

/// Reads goal labels such as `//services/orders:check` from `tasks.star` files.
#[derive(Debug, Default, Clone)]
pub struct StarlarkFrontend {
    pub context: Context,
}

impl Frontend for StarlarkFrontend {
    fn name(&self) -> &'static str {
        "starlark"
    }

    fn accepts(&self, target: &str) -> bool {
        target.starts_with(LABEL_PREFIX)
    }

    fn load(&self, request: &LoadRequest<'_>) -> Result<Loaded, FrontendError> {
        let (package, _) = parse_label(request.target)
            .ok_or_else(|| format!("{:?} is not a goal label such as //services/orders:check", request.target))?;
        let defined = evaluate_in(&self.context, request.root, package, request.platform)?;
        Ok(Loaded { package: defined, goal: request.target.to_owned(), scope: Some(package.to_owned()) })
    }

    fn targets(&self, root: &Path, files: &BTreeSet<PathBuf>, platform: &str) -> Vec<Listed> {
        let packages: std::collections::BTreeMap<String, String> = files
            .iter()
            .filter(|file| file.file_name().is_some_and(is_tasks_file))
            .map(|file| {
                let package = file.parent().map(|dir| dir.display().to_string()).unwrap_or_default();
                (package, file.display().to_string())
            })
            .collect();
        packages
            .iter()
            .flat_map(|(package, file)| package_targets(&self.context, root, package, file, platform))
            .collect()
    }
}

/// The package of a directory: the nearest directory at or above `dir`, and not above `root`, that holds a
/// `tasks.star`, relative to `root` ("" for the root). `None` when there is none, or when `dir` is not inside `root`.
pub fn package_of(root: &Path, dir: &Path) -> Option<String> {
    let root = std::fs::canonicalize(root).ok()?;
    let dir = std::fs::canonicalize(dir).ok()?;
    package_at(&root, dir.strip_prefix(&root).ok()?)
}

/// The package of a path relative to `root`, which need not exist: the nearest directory at or above it that holds a
/// `tasks.star` or a `tasks.yaml`, relative to `root` ("" for the root). `None` when there is none.
pub fn package_at(root: &Path, relative: &Path) -> Option<String> {
    let found = relative.ancestors().find(|ancestor| has_tasks_file(&root.join(ancestor)))?;
    Some(found.display().to_string())
}

const STARLARK_ERROR_PREFIX: &str = "error: ";

fn package_targets(context: &Context, root: &Path, package: &str, file: &str, platform: &str) -> Vec<Listed> {
    match evaluate_in(context, root, package, platform) {
        Ok(defined) => {
            let tags_of = |label: &String| defined.tags.get(label).cloned().unwrap_or_default();
            let goals = defined.goals.iter().map(|(label, required)| Listed {
                target: label.clone(),
                problem: None,
                doc: defined.docs.get(label).cloned().unwrap_or_default(),
                events: defined.events.get(label).cloned().unwrap_or_default(),
                tags: required.iter().flat_map(tags_of).collect(),
            });
            let tasks =
                defined.tasks.iter().filter(|(label, _)| !defined.goals.contains_key(*label)).map(|(label, task)| {
                    Listed {
                        target: label.clone(),
                        problem: None,
                        doc: task.doc.clone(),
                        events: BTreeSet::new(),
                        tags: tags_of(label),
                    }
                });
            goals.chain(tasks).collect()
        }
        Err(error) => {
            let problem = Some(summary(&error.to_string()));
            vec![Listed {
                target: file.to_owned(),
                problem,
                doc: String::new(),
                events: BTreeSet::new(),
                tags: BTreeSet::new(),
            }]
        }
    }
}

/// The line of a Starlark error that says what is wrong, without its traceback and source snippet.
fn summary(error: &str) -> String {
    let mut lines = error.lines();
    let stated = lines.clone().find_map(|line| line.strip_prefix(STARLARK_ERROR_PREFIX));
    stated.or_else(|| lines.next()).unwrap_or_default().to_owned()
}
