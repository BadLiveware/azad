//! Loading and evaluating `.star` files: modules through `load()`, config from `config.star` files, and the
//! `tasks.star` file of one package.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use starlark::environment::{FrozenModule, Globals, GlobalsBuilder, LibraryExtension, Module};
use starlark::eval::{Evaluator, FileLoader};
use starlark::syntax::{AstModule, Dialect};
use starlark::values::list::UnpackList;
use starlark::values::structs::AllocStruct;
use starlark::values::{FrozenValue, UnpackValue};

use crate::frontend::starlark::builtins::{azad_globals, is_valid_tag, namespaces};
use crate::frontend::starlark::values::CALL_NAME_PARAM;
use crate::frontend::starlark::values::Collector;
use crate::frontend::{Call, LocalRun, Package};
use crate::history::History;

use crate::frontend::starlark::yaml::{self, TASKS_YAML_FILE};
use crate::frontend::starlark::{CONFIG_FILE, TASKS_FILE};

/// The name under which `tasks.star` sees the merged config.
const CONFIG_NAME: &str = "config";
const LABEL_PREFIX: &str = "//";
const STAR_EXTENSION: &str = ".star";
/// The standard library's modules are part of Azad, so their version is Azad's version.
const STD_PREFIX: &str = "@std//";
const STD_MODULES: &[(&str, &str)] = &[
    ("project.star", include_str!("../../../std/project.star")),
    ("test.star", include_str!("../../../std/test.star")),
    ("version.star", include_str!("../../../std/version.star")),
];

#[derive(Debug, thiserror::Error)]
pub enum EvalError {
    #[error("{0}")]
    Starlark(String),
    #[error("{path}: {error}")]
    Io { path: PathBuf, error: std::io::Error },
    #[error("package {0:?} has no {TASKS_FILE}")]
    NoTasks(String),
}

fn starlark_error(error: starlark::Error) -> EvalError {
    EvalError::Starlark(error.to_string())
}

struct Loader<'a> {
    root: &'a Path,
    globals: Globals,
    dialect: Dialect,
    loaded: RefCell<HashMap<String, FrozenModule>>,
    loading: RefCell<Vec<String>>,
}

impl<'a> Loader<'a> {
    fn new(root: &'a Path) -> Self {
        Loader {
            root,
            globals: GlobalsBuilder::extended_by(&[LibraryExtension::StructType, LibraryExtension::Json])
                .with(azad_globals)
                .with(namespaces)
                .build(),
            dialect: Dialect::Extended,
            loaded: RefCell::new(HashMap::new()),
            loading: RefCell::new(Vec::new()),
        }
    }

    fn path_of(&self, label: &str) -> anyhow::Result<PathBuf> {
        let Some(relative) = label.strip_prefix(LABEL_PREFIX).filter(|r| r.ends_with(STAR_EXTENSION)) else {
            anyhow::bail!("load({label:?}): a module path starts with // and ends with .star");
        };
        if relative.split('/').any(|part| part.is_empty() || part == "." || part == "..") {
            anyhow::bail!("load({label:?}): the path must be normalized and inside the workspace");
        }
        Ok(self.root.join(relative))
    }

    /// A module of the workspace, or of the standard library (`@std//test.star`).
    fn parse(&self, label: &str) -> anyhow::Result<AstModule> {
        let Some(name) = label.strip_prefix(STD_PREFIX) else {
            return self.read(label, &self.path_of(label)?);
        };
        let Some((_, content)) = STD_MODULES.iter().find(|(file, _)| *file == name) else {
            let known: Vec<&str> = STD_MODULES.iter().map(|(file, _)| *file).collect();
            anyhow::bail!("load({label:?}): the standard library has {known:?}");
        };
        AstModule::parse(label, (*content).to_owned(), &self.dialect).map_err(|e| anyhow::anyhow!("{e}"))
    }

    /// The program that a package's `tasks.yaml` says (decision 0027); its errors name the file, not the program.
    fn read_yaml(&self, label: &str, path: &Path, package: &str) -> anyhow::Result<AstModule> {
        let text = fs::read_to_string(path).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
        let program = yaml::to_starlark(package, &text).map_err(|e| anyhow::anyhow!("{label}: {e}"))?;
        AstModule::parse(label, program, &self.dialect).map_err(|e| anyhow::anyhow!("{label}: {e}"))
    }

    fn read(&self, label: &str, path: &Path) -> anyhow::Result<AstModule> {
        let content = fs::read_to_string(path).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
        AstModule::parse(label, content.clone(), &self.dialect).map_err(|e| parse_error(label, &content, e))
    }
}

// starlark 0.14.2 places an error at the end of a file after its last line, and its message then points to line 1 with
// no source; so such an error names the bracket that is still open.
fn parse_error(label: &str, content: &str, error: starlark::Error) -> anyhow::Error {
    let at_end = error.span().is_some_and(|span| span.span.begin().get() as usize >= content.len());
    match (at_end, last_open_bracket(content)) {
        (true, Some((line, column, bracket))) => {
            let text = content.lines().nth(line).unwrap_or_default();
            let (number, position) = (line + 1, column + 1);
            let gutter = " ".repeat(number.to_string().len());
            anyhow::anyhow!(
                "error: the file ends before the {bracket:?} on line {number} is closed\n{gutter}--> {label}:{number}:{position}\n{gutter} |\n{number} | {text}\n{gutter} | {:column$}^",
                ""
            )
        }
        _ => anyhow::anyhow!("{error}"),
    }
}

/// The line and column (from 0) of the last bracket that is not closed, outside strings and comments.
fn last_open_bracket(content: &str) -> Option<(usize, usize, char)> {
    let mut open: Vec<(usize, usize, char)> = Vec::new();
    for (line, text) in content.lines().enumerate() {
        let mut chars = text.char_indices().peekable();
        while let Some((column, c)) = chars.next() {
            match c {
                '#' => break,
                '"' | '\'' => skip_string(&mut chars, c),
                '(' | '[' | '{' => open.push((line, column, c)),
                ')' | ']' | '}' => {
                    open.pop();
                }
                _ => {}
            }
        }
    }
    open.pop()
}

fn skip_string(chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>, quote: char) {
    while let Some((_, c)) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            c if c == quote => return,
            _ => {}
        }
    }
}

impl FileLoader for Loader<'_> {
    fn load(&self, label: &str) -> starlark::Result<FrozenModule> {
        if let Some(module) = self.loaded.borrow().get(label) {
            return Ok(module.clone());
        }
        if self.loading.borrow().iter().any(|l| l == label) {
            let chain = self.loading.borrow().join(" -> ");
            return Err(anyhow::anyhow!("load cycle: {chain} -> {label}").into());
        }
        let ast = self.parse(label)?;
        self.loading.borrow_mut().push(label.to_owned());
        let frozen = Module::with_temp_heap(|module| -> starlark::Result<FrozenModule> {
            {
                let mut eval = Evaluator::new(&module);
                eval.set_loader(self);
                eval.eval_module(ast, &self.globals)?;
            }
            Ok(module.freeze()?)
        });
        self.loading.borrow_mut().pop();
        let frozen = frozen?;
        self.loaded.borrow_mut().insert(label.to_owned(), frozen.clone());
        Ok(frozen)
    }
}

/// What evaluation can use beyond the files of the workspace.
#[derive(Clone, Debug, Default)]
pub struct Context {
    /// A source of the history that a shallow clone lacks (decision 0021).
    pub history: Option<Arc<dyn History>>,
}

/// Evaluates the `tasks.star` file of `package` (a directory relative to `root`, "" for the root) and returns what it
/// defines. Actions get `platform`.
pub fn evaluate(root: &Path, package: &str, platform: &str) -> Result<Package, EvalError> {
    evaluate_in(&Context::default(), root, package, platform)
}

/// The module of a package: its `tasks.star`, or the program that its `tasks.yaml` says; a package has one of them.
fn package_module(loader: &Loader, package: &str) -> Result<AstModule, EvalError> {
    let star_label = module_label(package, TASKS_FILE);
    let star_path = loader.path_of(&star_label).map_err(|e| EvalError::Starlark(e.to_string()))?;
    let yaml_label = module_label(package, TASKS_YAML_FILE);
    let yaml_path = star_path.with_file_name(TASKS_YAML_FILE);
    let read = match (star_path.is_file(), yaml_path.is_file()) {
        (true, true) => {
            return Err(EvalError::Starlark(format!(
                "package {package:?} has both {TASKS_FILE} and {TASKS_YAML_FILE}; keep one of them"
            )));
        }
        (false, false) => return Err(EvalError::NoTasks(package.to_owned())),
        (true, false) => loader.read(&star_label, &star_path),
        (false, true) => loader.read_yaml(&yaml_label, &yaml_path, package),
    };
    read.map_err(|e| EvalError::Starlark(e.to_string()))
}

/// As `evaluate`, with what `context` gives.
pub fn evaluate_in(context: &Context, root: &Path, package: &str, platform: &str) -> Result<Package, EvalError> {
    let loader = Loader::new(root);
    let ast = package_module(&loader, package)?;
    let configs = config_modules(&loader, package)?;
    let collector = Collector {
        root: root.to_path_buf(),
        package: package.to_owned(),
        dir: package.to_owned(),
        platform: platform.to_owned(),
        defined: RefCell::new(Package::default()),
        default_tags: config_tags(&configs)?,
        default_local: config_local(&configs)?,
        history: context.history.clone(),
        declared_package: Default::default(),
    };
    Module::with_temp_heap(|module| -> Result<(), EvalError> {
        module.set(CONFIG_NAME, merged_config(&module, &configs).to_value());
        let mut eval = Evaluator::new(&module);
        eval.set_loader(&loader);
        eval.extra = Some(&collector);
        eval.eval_module(ast, &loader.globals).map_err(starlark_error)?;
        Ok(())
    })?;
    Ok(collector.defined.into_inner())
}

fn module_label(package: &str, file: &str) -> String {
    match package.is_empty() {
        true => format!("{LABEL_PREFIX}{file}"),
        false => format!("{LABEL_PREFIX}{package}/{file}"),
    }
}

/// The `config.star` modules from the root down to the package, in that order.
fn config_modules(loader: &Loader<'_>, package: &str) -> Result<Vec<FrozenModule>, EvalError> {
    let mut directories = vec![String::new()];
    let mut current = String::new();
    for part in package.split('/').filter(|p| !p.is_empty()) {
        current = match current.is_empty() {
            true => part.to_owned(),
            false => format!("{current}/{part}"),
        };
        directories.push(current.clone());
    }
    let mut modules = Vec::new();
    for directory in directories {
        let label = module_label(&directory, CONFIG_FILE);
        let exists = loader.path_of(&label).map(|p| p.is_file()).unwrap_or(false);
        if exists {
            modules.push(loader.load(&label).map_err(starlark_error)?);
        }
    }
    Ok(modules)
}

/// The name in a `config.star` file whose tags every task and action below it gets.
const CONFIG_TAGS: &str = "tags";

/// The tags of the config modules: unlike other names, a deeper directory adds to the tags above it (decision 0023).
fn config_tags(configs: &[FrozenModule]) -> Result<BTreeSet<String>, EvalError> {
    let mut tags = BTreeSet::new();
    for config in configs {
        let Ok(value) = config.get(CONFIG_TAGS) else { continue };
        let listed = UnpackList::<String>::unpack_value(value.value()).ok().flatten().ok_or_else(|| {
            EvalError::Starlark(format!("`{CONFIG_TAGS}` in a {CONFIG_FILE} must be a list of strings"))
        })?;
        if let Some(bad) = listed.items.iter().find(|tag| !is_valid_tag(tag)) {
            return Err(EvalError::Starlark(format!(
                "`{CONFIG_TAGS}` in a {CONFIG_FILE} has {bad:?}, which is not a tag: letters, digits, '-', '_' and '.'"
            )));
        }
        tags.extend(listed.items);
    }
    Ok(tags)
}

/// The name in a `config.star` file that says where a local run runs the tasks below it (decision 0024).
const CONFIG_LOCAL: &str = "local";

/// Where a local run runs a task that does not say: `local` of the deepest config module that sets it.
fn config_local(configs: &[FrozenModule]) -> Result<LocalRun, EvalError> {
    let Some(value) = configs.iter().rev().find_map(|config| config.get(CONFIG_LOCAL).ok()) else {
        return Ok(LocalRun::default());
    };
    value.value().unpack_str().and_then(LocalRun::parse).ok_or_else(|| {
        EvalError::Starlark(format!(
            "`{CONFIG_LOCAL}` in a {CONFIG_FILE} is \"host\" or \"image\", not {}",
            value.value()
        ))
    })
}

/// The public names of the config modules, as one struct on the frozen heap of `module`; a name in a deeper directory
/// replaces the same name above it. The module keeps a reference to each config heap, so the values stay valid.
fn merged_config(module: &Module<'_>, configs: &[FrozenModule]) -> FrozenValue {
    let mut merged: BTreeMap<String, FrozenValue> = BTreeMap::new();
    for config in configs {
        module.frozen_heap().add_reference(config.frozen_heap());
        for name in config.names() {
            let name = name.as_str();
            if name.starts_with('_') {
                continue;
            }
            if let Ok(owned) = config.get(name) {
                owned.map(|value| {
                    merged.insert(name.to_owned(), value);
                    value
                });
            }
        }
    }
    module.frozen_heap().alloc(AllocStruct(merged))
}

/// The actions that a module call makes, evaluated as a direct call from the call's package (decision 0014): a program
/// that loads the module and calls it, with the arguments decoded from JSON.
pub fn expand(context: &Context, root: &Path, platform: &str, call: &Call) -> Result<Package, EvalError> {
    let loader = Loader::new(root);
    let mut args = call.args.clone();
    args.insert(CALL_NAME_PARAM.to_owned(), serde_json::Value::String(call.name.clone()));
    let args = serde_json::to_string(&args).expect("JSON values serialize");
    let program = format!(
        "load({file}, {CALL_SYMBOL} = {symbol})\n{CALL_SYMBOL}(**json.decode({args}))\n",
        file = quote(&call.file),
        symbol = quote(&call.symbol),
        args = quote(&args),
    );
    let label = format!("{} (a call of {} in {})", call.name, call.symbol, call.file);
    let ast = AstModule::parse(&label, program, &loader.dialect).map_err(|e| EvalError::Starlark(e.to_string()))?;
    let collector = Collector {
        root: root.to_path_buf(),
        package: call.package.clone(),
        dir: call.dir.clone(),
        platform: platform.to_owned(),
        defined: RefCell::new(Package::default()),
        default_tags: config_tags(&config_modules(&loader, &call.package)?)?,
        default_local: config_local(&config_modules(&loader, &call.package)?)?,
        history: context.history.clone(),
        declared_package: Default::default(),
    };
    Module::with_temp_heap(|module| -> Result<(), EvalError> {
        let mut eval = Evaluator::new(&module);
        eval.set_loader(&loader);
        eval.extra = Some(&collector);
        eval.eval_module(ast, &loader.globals).map_err(starlark_error)?;
        Ok(())
    })?;
    Ok(collector.defined.into_inner())
}

const CALL_SYMBOL: &str = "__module";

/// A Starlark string literal: JSON's escapes are valid in Starlark.
fn quote(text: &str) -> String {
    serde_json::to_string(text).expect("a string serializes")
}

/// The package with its calls expanded: the actions and goals of each call are added, and the calls are gone.
pub fn expand_calls(
    context: &Context,
    root: &Path,
    platform: &str,
    mut package: Package,
) -> Result<Package, EvalError> {
    for call in std::mem::take(&mut package.calls) {
        let expanded = expand(context, root, platform, &call)?;
        for (label, spec) in expanded.actions {
            if package.actions.insert(label.clone(), spec).is_some() {
                return Err(EvalError::Starlark(format!(
                    "the call {} defines {label}, which the package has",
                    call.name
                )));
            }
        }
        package.goals.extend(expanded.goals);
        package.declared_at.extend(expanded.declared_at);
        package.git_refs.extend(expanded.git_refs);
        package.read_files.extend(expanded.read_files);
        package.tags.extend(expanded.tags);
        package
            .notes
            .extend(expanded.notes.into_iter().filter(|note| !package.notes.contains(note)).collect::<Vec<_>>());
    }
    Ok(package)
}
