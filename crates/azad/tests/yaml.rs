//! `tasks.yaml` (decision 0027): a package declared as data means what the same `tasks.star` means.

#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::Path;

use azad::frontend::Package;
use azad::frontend::starlark::evaluate;
use tempfile::TempDir;

const PLATFORM: &str = "linux/amd64";
const CONFIG: &str = r#"TOOLS = image("tools@sha256:00000000000000000000000000000000000000000000000000000000000000aa", env = {"PATH": "/bin"})"#;

/// A module that returns parts, with each kind of parameter that a call from YAML gives.
const MODULE: &str = r#"
load("@std//project.star", "part")

def _lint(tool, files, ci = False):
    checks = []
    if ci:
        checks = ["lint"]
    lint = task(name = "tool-lint", cmd = "lint " + " ".join(files), image = tool, sources = files)
    return [part(lint = lint, ci = checks)]

lint = module(
    name = "tools.lint",
    params = {"tool": "image", "files": "strings", "ci": "bool?"},
    returns = "parts",
    implementation = _lint,
)
"#;

const STARLARK: &str = r#"
load("@std//project.star", "part", "project")
load("//lib/tools.star", "lint")

package(deps = ["//lib", "//shared:notes"], exports = ["src/**"], provides = ["generate"])
file_set(name = "schemas", sources = ["schema/**"], exclude = ["schema/old/**"], deps = ["//lib"])

generate = task(
    name = "generate",
    cmd = "./gen.sh",
    sources = ["schema/**", "//shared/**"],
    exclude = ["schema/old/**"],
    generates = ["gen"],
    image = config.TOOLS,
    env = {"MODE": "fast"},
    caches = {"/cache/gen": "gen"},
    network = True,
    tags = ["codegen"],
    local = "image",
    secrets = ["TOKEN"],
    doc = "Generate the code",
)
check = task(name = "check-generated", cmd = ["git", "diff", "--exit-code", "gen"], deps = [generate, "//lib:build"], dependencies = True)
other = task(name = "other", cmd = "true", dependencies = [":schemas", "//lib"])
project(lint(tool = config.TOOLS, files = ["src/**"], ci = True), part(lint = check, ci = ["lint"]))
goal("release", require = ["generate", "tool-lint"], doc = "What a release needs", on = ["push"])
"#;

const YAML: &str = r#"
package:
  deps: ["//lib", "//shared:notes"]
  exports: ["src/**"]
  provides: [generate]

file_sets:
  schemas:
    sources: ["schema/**"]
    exclude: ["schema/old/**"]
    deps: ["//lib"]

uses:
  - module: //lib/tools.star:lint
    with:
      tool: {config: TOOLS}
      files: ["src/**"]
      ci: true

tasks:
  generate:
    cmd: ./gen.sh
    sources: ["schema/**", "//shared/**"]
    exclude: ["schema/old/**"]
    generates: [gen]
    image: {config: TOOLS}
    env: {MODE: fast}
    caches: {/cache/gen: gen}
    network: true
    tags: [codegen]
    local: image
    secrets: [TOKEN]
    doc: Generate the code
  check-generated:
    cmd: [git, diff, --exit-code, gen]
    deps: [generate, "//lib:build"]
    role: lint
    ci: true
    dependencies: true
  other:
    cmd: "true"
    dependencies: [":schemas", "//lib"]

goals:
  release:
    require: [generate, tool-lint]
    doc: What a release needs
    on: [push]
"#;

fn write(root: &Path, path: &str, content: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

/// The package `app` of a workspace whose `app/<file>` holds `content`.
fn evaluated(file: &str, content: &str) -> Result<Package, String> {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "config.star", CONFIG);
    write(dir.path(), "lib/tools.star", MODULE);
    write(dir.path(), &format!("app/{file}"), content);
    evaluate(dir.path(), "app", PLATFORM).map_err(|error| error.to_string())
}

/// What a package defines, without where each task was written, which differs between the two files.
fn defined(mut package: Package) -> Package {
    package.tasks.values_mut().for_each(|task| task.declared_at = None);
    package.declared_at.clear();
    package
}

#[test]
fn a_tasks_yaml_defines_what_the_same_tasks_star_defines() {
    let from_starlark = defined(evaluated("tasks.star", STARLARK).unwrap());
    let from_yaml = defined(evaluated("tasks.yaml", YAML).unwrap());
    assert_eq!(from_yaml.tasks, from_starlark.tasks);
    assert_eq!(from_yaml.goals, from_starlark.goals);
    assert_eq!(from_yaml.docs, from_starlark.docs);
    assert_eq!(from_yaml.events, from_starlark.events);
    assert_eq!(from_yaml.tags, from_starlark.tags);
    assert_eq!(from_yaml.declared, from_starlark.declared);
    assert_eq!(from_yaml.file_sets, from_starlark.file_sets);
    let goals: Vec<&str> = from_yaml.goals.keys().map(String::as_str).collect();
    assert_eq!(goals, ["//app:ci", "//app:lint", "//app:release"]);
}

/// The mistakes that YAML makes easy fail with the key that has them, before any Starlark runs.
#[test]
fn a_tasks_yaml_with_a_mistake_says_where() {
    let cases = [
        ("tasks:\n  t:\n    cmd: x\n    sourcse: [a]\n", "tasks.t: \"sourcse\" is not a key here"),
        ("tasks:\n  t:\n    cmd: x\n    ci: true\n", "tasks.t: ci: true needs a role:"),
        ("tasks:\n  t:\n    cmd: x\n    env: {PORT: 8080}\n", "tasks.t.env.PORT: text is needed; put a number"),
        (
            "tasks:\n  t:\n    cmd: x\n    image: {config: TOOLS, extra: 1}\n",
            "tasks.t.image: a mapping here is {config: NAME}",
        ),
        ("uses:\n  - module: tools.star:lint\n", "uses[0].module: \"tools.star:lint\" is not a module"),
        ("goals:\n  g:\n    doc: x\n", "goals.g: require: is needed"),
        ("goals:\n  g:\n    require: [missing]\n", "//app:missing is not a task or an action of this package"),
        ("tasks: [a, b]\n", "tasks: a mapping is needed"),
    ];
    for (yaml, expected) in cases {
        let error = evaluated("tasks.yaml", yaml).unwrap_err();
        assert!(error.contains(expected), "{yaml}\n{error}");
    }
}

#[test]
fn a_module_call_from_yaml_checks_its_arguments() {
    let wrong_type =
        "uses:\n  - module: //lib/tools.star:lint\n    with: {tool: {config: TOOLS}, files: [a], ci: maybe}\n";
    assert!(evaluated("tasks.yaml", wrong_type).unwrap_err().contains("ci"), "a bool parameter takes no text");
    let not_an_image = "uses:\n  - module: //lib/tools.star:lint\n    with: {tool: 3, files: [a]}\n";
    assert!(evaluated("tasks.yaml", not_an_image).unwrap_err().contains("an image is needed"));
    let named = "load(\"//lib/tools.star\", \"lint\")\nlint(name = \"x\", tool = config.TOOLS, files = [\"a\"])\n";
    assert!(evaluated("tasks.star", named).unwrap_err().contains("takes no name"));
}

#[test]
fn a_package_has_one_kind_of_tasks_file() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "config.star", CONFIG);
    write(dir.path(), "app/tasks.star", "task(name = \"a\", cmd = \"true\")\n");
    write(dir.path(), "app/tasks.yaml", "tasks:\n  b:\n    cmd: \"true\"\n");
    let error = evaluate(dir.path(), "app", PLATFORM).unwrap_err().to_string();
    assert!(error.contains("has both tasks.star and tasks.yaml"), "{error}");
}

/// The JSON Schema that editors read names the keys that the parser takes, and no others.
#[test]
fn the_json_schema_names_the_keys_of_the_parser() {
    use azad::frontend::starlark::yaml::{
        CONFIG_KEY, FILE_SET_KEYS, GOAL_KEYS, PACKAGE_KEYS, TASK_KEYS, TOP_KEYS, USE_KEYS,
    };
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/tasks.schema.json");
    let schema: serde_json::Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    let keys = |pointer: &str| {
        let mut keys: Vec<String> = schema.pointer(pointer).unwrap().as_object().unwrap().keys().cloned().collect();
        keys.sort();
        keys
    };
    let sorted = |names: &[&str]| {
        let mut names: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
        names.sort();
        names
    };
    assert_eq!(keys("/properties"), sorted(&TOP_KEYS));
    assert_eq!(keys("/$defs/use/properties"), sorted(&USE_KEYS));
    assert_eq!(keys("/$defs/task/properties"), sorted(&TASK_KEYS));
    assert_eq!(keys("/$defs/goal/properties"), sorted(&GOAL_KEYS));
    assert_eq!(keys("/$defs/config/properties"), sorted(&[CONFIG_KEY]));
    assert_eq!(keys("/properties/package/properties"), sorted(&PACKAGE_KEYS));
    assert_eq!(keys("/$defs/file_set/properties"), sorted(&FILE_SET_KEYS));
}
