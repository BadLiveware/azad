//! `tasks.yaml` (decision 0027): a package declared as data. Azad checks the file, and turns it into the `tasks.star`
//! program that says the same, so that both kinds of file mean the same thing. YAML gives only values: the package's
//! dependencies (decision 0028), tasks, goals, the roles of `@std//project.star`, and calls of modules that return
//! parts; `{config: NAME}`, a value of `config.star`, is its only reference.

use yaml_rust2::yaml::Hash;
use yaml_rust2::{Yaml, YamlLoader};

use crate::frontend::{WORKSPACE_PATTERN_PREFIX, label};

/// The name of the file that declares a package as data, in place of `tasks.star`.
pub const TASKS_YAML_FILE: &str = "tasks.yaml";

/// The keys of each mapping of the file; the JSON Schema of the file names the same keys.
pub const TOP_KEYS: [&str; 5] = ["package", "file_sets", "uses", "tasks", "goals"];
pub const PACKAGE_KEYS: [&str; 3] = ["deps", "exports", "provides"];
pub const FILE_SET_KEYS: [&str; 3] = ["sources", "exclude", "deps"];
pub const USE_KEYS: [&str; 2] = ["module", "with"];
pub const TASK_KEYS: [&str; 16] = [
    "cmd",
    "sources",
    "exclude",
    "generates",
    "deps",
    "image",
    "env",
    "caches",
    "network",
    "tags",
    "local",
    "secrets",
    "doc",
    "role",
    "ci",
    "dependencies",
];
pub const GOAL_KEYS: [&str; 3] = ["require", "doc", "on"];
pub const CONFIG_KEY: &str = "config";

const MODULE_FILE_SUFFIX: &str = ".star";
const STD_PREFIX: &str = "@std//";
const STRING_LIST_KEYS: [&str; 5] = ["sources", "exclude", "generates", "tags", "secrets"];
const STRING_KEYS: [&str; 2] = ["local", "doc"];
const MAP_KEYS: [&str; 2] = ["env", "caches"];

/// The `tasks.star` program that the `tasks.yaml` text of `package` says.
pub fn to_starlark(package: &str, text: &str) -> Result<String, String> {
    let documents = YamlLoader::load_from_str(text).map_err(|e| e.to_string())?;
    let [document] = documents.as_slice() else {
        return Err(format!("{TASKS_YAML_FILE} is one YAML document, not {}", documents.len()));
    };
    let top = mapping(document, "the file")?;
    allow_keys(top, &TOP_KEYS, "the file")?;
    let mut program = Program::default();
    if let Some(package) = top.get(&key("package")) {
        program.package(package)?;
    }
    if let Some(sets) = top.get(&key("file_sets")) {
        program.file_sets(sets)?;
    }
    if let Some(uses) = top.get(&key("uses")) {
        program.uses(uses)?;
    }
    if let Some(tasks) = top.get(&key("tasks")) {
        program.tasks(package, tasks)?;
    }
    if let Some(goals) = top.get(&key("goals")) {
        program.goals(goals)?;
    }
    Ok(program.text())
}

/// The statements of the program, in the order that it runs them: tasks first, then the project of the calls' and
/// tasks' parts, then the goals, which can name the tasks of both.
#[derive(Default)]
struct Program {
    declarations: Vec<String>,
    loads: Vec<String>,
    tasks: Vec<String>,
    parts: Vec<String>,
    goals: Vec<String>,
}

impl Program {
    fn text(self) -> String {
        let mut lines = vec![format!("# Made by Azad from {TASKS_YAML_FILE} (decision 0027).")];
        if !self.parts.is_empty() {
            lines.push("load(\"@std//project.star\", \"part\", \"project\")".to_owned());
        }
        lines.extend(self.loads);
        lines.extend(self.declarations);
        lines.extend(self.tasks);
        if !self.parts.is_empty() {
            lines.push(format!("project({})", self.parts.join(", ")));
        }
        lines.extend(self.goals);
        lines.join("\n") + "\n"
    }

    fn package(&mut self, package: &Yaml) -> Result<(), String> {
        let fields = mapping(package, "package")?;
        allow_keys(fields, &PACKAGE_KEYS, "package")?;
        let arguments = string_lists(fields, &PACKAGE_KEYS, "package")?;
        self.declarations.push(format!("package({})", arguments.join(", ")));
        Ok(())
    }

    fn file_sets(&mut self, sets: &Yaml) -> Result<(), String> {
        for (name, fields) in mapping(sets, "file_sets")? {
            let name = string(name, "a name of file_sets")?;
            let at = format!("file_sets.{name}");
            let fields = mapping(fields, &at)?;
            allow_keys(fields, &FILE_SET_KEYS, &at)?;
            if !fields.contains_key(&key("sources")) {
                return Err(format!("{at}: sources: is needed"));
            }
            let mut arguments = vec![format!("name = {}", literal(name, &at)?)];
            arguments.extend(string_lists(fields, &FILE_SET_KEYS, &at)?);
            self.declarations.push(format!("file_set({})", arguments.join(", ")));
        }
        Ok(())
    }

    fn uses(&mut self, uses: &Yaml) -> Result<(), String> {
        let Yaml::Array(calls) = uses else { return Err("uses: is a list of calls of modules".to_owned()) };
        for (index, call) in calls.iter().enumerate() {
            let at = format!("uses[{index}]");
            let call = mapping(call, &at)?;
            allow_keys(call, &USE_KEYS, &at)?;
            let Some(module) = call.get(&key("module")) else { return Err(format!("{at}: module: is needed")) };
            let (file, name) = module_reference(string(module, &format!("{at}.module"))?, &at)?;
            let alias = format!("_use_{index}");
            self.loads.push(format!("load({}, {alias} = {})", literal(file, &at)?, literal(name, &at)?));
            let arguments = match call.get(&key("with")) {
                Some(with) => arguments(with, &format!("{at}.with"))?,
                None => Vec::new(),
            };
            self.parts.push(format!("{alias}({})", arguments.join(", ")));
        }
        Ok(())
    }

    fn tasks(&mut self, package: &str, tasks: &Yaml) -> Result<(), String> {
        let tasks = mapping(tasks, "tasks")?;
        for (index, (name, fields)) in tasks.iter().enumerate() {
            let name = string(name, "a name of tasks")?;
            let at = format!("tasks.{name}");
            let fields = mapping(fields, &at)?;
            allow_keys(fields, &TASK_KEYS, &at)?;
            let variable = format!("_task_{index}");
            let mut arguments = vec![format!("name = {}", literal(name, &at)?)];
            for (field, value) in fields {
                let field = string(field, &at)?;
                let at = format!("{at}.{field}");
                if let Some(argument) = task_argument(package, field, value, &at)? {
                    arguments.push(format!("{field} = {argument}"));
                }
            }
            self.tasks.push(format!("{variable} = task({})", arguments.join(", ")));
            if let Some(part) = role_part(fields, &variable, &at)? {
                self.parts.push(part);
            }
        }
        Ok(())
    }

    fn goals(&mut self, goals: &Yaml) -> Result<(), String> {
        let goals = mapping(goals, "goals")?;
        for (name, fields) in goals {
            let name = string(name, "a name of goals")?;
            let at = format!("goals.{name}");
            let fields = mapping(fields, &at)?;
            allow_keys(fields, &GOAL_KEYS, &at)?;
            let Some(require) = fields.get(&key("require")) else { return Err(format!("{at}: require: is needed")) };
            let mut arguments = vec![literal(name, &at)?, format!("require = {}", string_list(require, &at)?)];
            if let Some(doc) = fields.get(&key("doc")) {
                arguments.push(format!("doc = {}", literal(string(doc, &format!("{at}.doc"))?, &at)?));
            }
            if let Some(on) = fields.get(&key("on")) {
                arguments.push(format!("on = {}", string_list(on, &format!("{at}.on"))?));
            }
            self.goals.push(format!("goal({})", arguments.join(", ")));
        }
        Ok(())
    }
}

/// The Starlark value of one field of a task, or None for `role` and `ci`, which make the task's part.
fn task_argument(package: &str, field: &str, value: &Yaml, at: &str) -> Result<Option<String>, String> {
    let argument = match field {
        "cmd" => match value {
            Yaml::Array(_) => string_list(value, at)?,
            _ => literal(string(value, at)?, at)?,
        },
        "deps" => {
            let deps = strings(value, at)?;
            let labels: Vec<String> = deps
                .iter()
                .map(|dep| match dep.starts_with(WORKSPACE_PATTERN_PREFIX) {
                    true => (*dep).to_owned(),
                    false => label(package, dep),
                })
                .collect();
            list(&labels.iter().map(String::as_str).collect::<Vec<_>>(), at)?
        }
        "image" => match config_reference(value, at)? {
            Some(reference) => reference,
            None => format!("image({})", literal(string(value, at)?, at)?),
        },
        "network" => starlark_bool(boolean(value, at)?).to_owned(),
        "dependencies" => match value {
            Yaml::Boolean(all) => starlark_bool(*all).to_owned(),
            _ => string_list(value, at)?,
        },
        "role" | "ci" => return Ok(None),
        field if STRING_LIST_KEYS.contains(&field) => string_list(value, at)?,
        field if STRING_KEYS.contains(&field) => literal(string(value, at)?, at)?,
        field if MAP_KEYS.contains(&field) => string_map(value, at)?,
        other => return Err(format!("{at}: {other} is not a field of a task")),
    };
    Ok(Some(argument))
}

/// The part that puts a task in its `role`, which CI checks when `ci` is true.
fn role_part(fields: &Hash, variable: &str, at: &str) -> Result<Option<String>, String> {
    let ci = match fields.get(&key("ci")) {
        Some(value) => boolean(value, &format!("{at}.ci"))?,
        None => false,
    };
    let Some(role) = fields.get(&key("role")) else {
        return match ci {
            true => Err(format!("{at}: ci: true needs a role:, the goal that the task is in")),
            false => Ok(None),
        };
    };
    let role = string(role, &format!("{at}.role"))?;
    if role.is_empty() || !role.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Err(format!("{at}.role: {role:?} is not a role such as fmt or fmt-check"));
    }
    let checked = match ci {
        true => format!("[{}]", literal(role, at)?),
        false => "[]".to_owned(),
    };
    Ok(Some(format!("part({} = {variable}, ci = {checked})", role.replace('-', "_"))))
}

/// The arguments of a call: each parameter as a value, a list of values, or a value of `config.star`.
fn arguments(with: &Yaml, at: &str) -> Result<Vec<String>, String> {
    let with = mapping(with, at)?;
    let mut arguments = Vec::new();
    for (name, value) in with {
        let name = string(name, at)?;
        if !is_identifier(name) {
            return Err(format!("{at}: {name:?} is not a parameter name"));
        }
        let at = format!("{at}.{name}");
        let value = match config_reference(value, &at)? {
            Some(reference) => reference,
            None => argument(value, &at)?,
        };
        arguments.push(format!("{name} = {value}"));
    }
    Ok(arguments)
}

fn argument(value: &Yaml, at: &str) -> Result<String, String> {
    match value {
        Yaml::String(text) => literal(text, at),
        Yaml::Integer(n) => Ok(n.to_string()),
        Yaml::Boolean(b) => Ok(starlark_bool(*b).to_owned()),
        Yaml::Array(items) => {
            let values = items
                .iter()
                .enumerate()
                .map(|(index, item)| match item {
                    Yaml::Array(_) | Yaml::Hash(_) => {
                        Err(format!("{at}[{index}]: a list holds text, numbers and booleans"))
                    }
                    _ => argument(item, &format!("{at}[{index}]")),
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(format!("[{}]", values.join(", ")))
        }
        _ => Err(format!("{at}: a parameter is text, a number, a boolean, a list of them, or {{{CONFIG_KEY}: NAME}}")),
    }
}

/// `config.NAME` for `{config: NAME}`, the one reference that the file has; None for any other value.
fn config_reference(value: &Yaml, at: &str) -> Result<Option<String>, String> {
    let Yaml::Hash(hash) = value else { return Ok(None) };
    let [(name_key, name)] = hash.iter().collect::<Vec<_>>()[..] else {
        return Err(format!("{at}: a mapping here is {{{CONFIG_KEY}: NAME}}"));
    };
    if string(name_key, at)? != CONFIG_KEY {
        return Err(format!("{at}: a mapping here is {{{CONFIG_KEY}: NAME}}"));
    }
    let name = string(name, at)?;
    if !is_identifier(name) {
        return Err(format!("{at}: {name:?} is not the name of a value of config.star"));
    }
    Ok(Some(format!("{CONFIG_KEY}.{name}")))
}

/// The file and the name of a module, such as `//tools/azad/dotnet.star:standard`.
fn module_reference<'t>(reference: &'t str, at: &str) -> Result<(&'t str, &'t str), String> {
    let problem = || format!("{at}.module: {reference:?} is not a module such as //tools/azad/dotnet.star:standard");
    let (file, name) = reference.rsplit_once(':').ok_or_else(problem)?;
    let in_workspace = file.starts_with(WORKSPACE_PATTERN_PREFIX) || file.starts_with(STD_PREFIX);
    if !in_workspace || !file.ends_with(MODULE_FILE_SUFFIX) || !is_identifier(name) {
        return Err(problem());
    }
    Ok((file, name))
}

fn mapping<'y>(value: &'y Yaml, at: &str) -> Result<&'y Hash, String> {
    match value {
        Yaml::Hash(hash) => Ok(hash),
        _ => Err(format!("{at}: a mapping is needed")),
    }
}

fn allow_keys(hash: &Hash, allowed: &[&str], at: &str) -> Result<(), String> {
    for name in hash.keys() {
        let name = string(name, at)?;
        if !allowed.contains(&name) {
            return Err(format!("{at}: {name:?} is not a key here; the keys are {allowed:?}"));
        }
    }
    Ok(())
}

fn key(name: &str) -> Yaml {
    Yaml::String(name.to_owned())
}

fn string<'y>(value: &'y Yaml, at: &str) -> Result<&'y str, String> {
    match value {
        Yaml::String(text) => Ok(text),
        Yaml::Integer(_) | Yaml::Real(_) | Yaml::Boolean(_) => {
            Err(format!("{at}: text is needed; put a number or a boolean in quotes, such as \"8080\""))
        }
        _ => Err(format!("{at}: text is needed")),
    }
}

fn strings<'y>(value: &'y Yaml, at: &str) -> Result<Vec<&'y str>, String> {
    let Yaml::Array(items) = value else { return Err(format!("{at}: a list of text is needed")) };
    items.iter().enumerate().map(|(index, item)| string(item, &format!("{at}[{index}]"))).collect()
}

fn boolean(value: &Yaml, at: &str) -> Result<bool, String> {
    match value {
        Yaml::Boolean(b) => Ok(*b),
        _ => Err(format!("{at}: true or false is needed")),
    }
}

/// `name = [...]` for each of `keys` that `fields` has, as lists of text.
fn string_lists(fields: &Hash, keys: &[&str], at: &str) -> Result<Vec<String>, String> {
    keys.iter()
        .filter_map(|name| fields.get(&key(name)).map(|value| (name, value)))
        .map(|(name, value)| Ok(format!("{name} = {}", string_list(value, &format!("{at}.{name}"))?)))
        .collect()
}

fn string_list(value: &Yaml, at: &str) -> Result<String, String> {
    list(&strings(value, at)?, at)
}

fn list(items: &[&str], at: &str) -> Result<String, String> {
    let items = items.iter().map(|item| literal(item, at)).collect::<Result<Vec<_>, _>>()?;
    Ok(format!("[{}]", items.join(", ")))
}

fn string_map(value: &Yaml, at: &str) -> Result<String, String> {
    let entries = mapping(value, at)?
        .iter()
        .map(|(name, value)| {
            let name = string(name, at)?;
            let value = string(value, &format!("{at}.{name}"))?;
            Ok(format!("{}: {}", literal(name, at)?, literal(value, at)?))
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(format!("{{{}}}", entries.join(", ")))
}

fn starlark_bool(value: bool) -> &'static str {
    match value {
        true => "True",
        false => "False",
    }
}

fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A Starlark string literal of `text`.
fn literal(text: &str, at: &str) -> Result<String, String> {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for c in text.chars() {
        match c {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            c if c.is_control() => return Err(format!("{at}: text holds the control character {c:?}")),
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    Ok(quoted)
}
