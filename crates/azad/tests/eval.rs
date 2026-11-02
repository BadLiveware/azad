#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::Path;

use azad::frontend::starlark::{EvalError, evaluate};
use azad::frontend::{SourceSpec, parse_label};
use azad::ir::Kind;
use tempfile::TempDir;

const PLATFORM: &str = "linux/amd64";
const BASE: &str = "registry.example/base@sha256:1111111111111111111111111111111111111111111111111111111111111111";
const GO: &str = "registry.example/go@sha256:2222222222222222222222222222222222222222222222222222222222222222";

fn write(root: &Path, path: &str, content: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn workspace() -> TempDir {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write(root, "config.star", &format!("image_ref = {BASE:?}\ngo_version = \"1.24\"\n_private = 1\n"));
    write(root, "services/config.star", &format!("image_ref = {GO:?}\n"));
    write(
        root,
        "modules/go.star",
        r#"
def go_service(name, src, root, version):
    build = action(
        name = name + "-build",
        cmd = ["go", "build", "-o", "out/" + name, "./..."],
        root = root,
        inputs = {"/src": src},
        outputs = ["out"],
        env = {"GOTOOLCHAIN": "go" + version},
        caches = {"/tmp/gocache": "go-build"},
    )
    test = action(
        name = name + "-test",
        cmd = ["go", "test", "./..."],
        root = root,
        inputs = {"/src": src, "/bin": build.out("out")},
    )
    return struct(build = build, test = test)
"#,
    );
    write(
        root,
        "services/orders/tasks.star",
        r#"
load("//modules/go.star", "go_service")

orders = go_service(
    name = "orders",
    src = glob(["**/*.go", "go.mod"], exclude = ["**/*_test.go"]),
    root = image(config.image_ref),
    version = config.go_version,
)
goal("check", require = [orders.build, orders.test])
"#,
    );
    dir
}

#[test]
fn a_package_evaluates_to_labelled_actions_and_goals() {
    let dir = workspace();
    let package = evaluate(dir.path(), "services/orders", PLATFORM).unwrap();
    let labels: Vec<&str> = package.actions.keys().map(String::as_str).collect();
    assert_eq!(labels, ["//services/orders:orders-build", "//services/orders:orders-test"]);
    assert_eq!(
        package.goals["//services/orders:check"].iter().collect::<Vec<_>>(),
        ["//services/orders:orders-build", "//services/orders:orders-test"]
    );

    let build = &package.actions["//services/orders:orders-build"];
    assert_eq!(build.kind, Kind::Hermetic);
    assert_eq!(build.platform, PLATFORM);
    assert_eq!(build.env["GOTOOLCHAIN"], "go1.24");
    assert_eq!(build.caches["/tmp/gocache"], "go-build");
    assert_eq!(
        build.inputs["/src"],
        SourceSpec::Glob {
            dir: "services/orders".to_owned(),
            include: vec!["**/*.go".to_owned(), "go.mod".to_owned()],
            exclude: vec!["**/*_test.go".to_owned()],
        }
    );
    let test = &package.actions["//services/orders:orders-test"];
    assert_eq!(
        test.inputs["/bin"],
        SourceSpec::Output { action: "//services/orders:orders-build".to_owned(), path: "out".to_owned() }
    );
}

#[test]
fn the_nearest_config_wins() {
    let dir = workspace();
    let package = evaluate(dir.path(), "services/orders", PLATFORM).unwrap();
    let build = &package.actions["//services/orders:orders-build"];
    assert_eq!(build.root, SourceSpec::Image { reference: GO.to_owned(), env: Default::default() });
}

#[test]
fn evaluation_is_deterministic() {
    let dir = workspace();
    let first = evaluate(dir.path(), "services/orders", PLATFORM).unwrap();
    let second = evaluate(dir.path(), "services/orders", PLATFORM).unwrap();
    assert_eq!(first, second);
}

fn error_of(files: &[(&str, &str)]) -> String {
    let dir = workspace();
    for (path, content) in files {
        write(dir.path(), path, content);
    }
    match evaluate(dir.path(), "app", PLATFORM) {
        Err(EvalError::Starlark(message)) => message,
        other => panic!("expected an evaluation error, found {other:?}"),
    }
}

#[test]
fn a_module_cannot_see_config_unless_it_is_passed() {
    let message = error_of(&[
        ("modules/leaky.star", "def f():\n    return config.image_ref\n"),
        ("app/tasks.star", "load(\"//modules/leaky.star\", \"f\")\nf()\n"),
    ]);
    assert!(message.contains("config"), "{message}");
}

#[test]
fn a_module_cannot_declare_actions_when_it_is_loaded() {
    let message = error_of(&[
        ("modules/eager.star", "action(name = \"x\", cmd = [\"true\"], root = image(\"a@sha256:00\"))\ny = 1\n"),
        ("app/tasks.star", "load(\"//modules/eager.star\", \"y\")\n"),
    ]);
    assert!(message.contains("only while a tasks.star file is evaluated"), "{message}");
}

#[test]
fn errors_are_clear() {
    let cases = [
        ("app/tasks.star", "image(\"golang:1.24\")\n", "must name a digest"),
        ("app/tasks.star", "image(\"golang:1.24\")\n", "`azad pull golang:1.24` prints the reference with its digest"),
        (
            "app/tasks.star",
            "a = action(name = \"a\", cmd = [\"true\"], root = image(\"a@sha256:00\"), outputs = [\"out\"])\na.out(\"bin\")\n",
            "does not declare the output",
        ),
        (
            "app/tasks.star",
            "action(name = \"a\", cmd = [\"true\"], root = image(\"a@sha256:00\"))\naction(name = \"a\", cmd = [\"true\"], root = image(\"a@sha256:00\"))\n",
            "is defined twice",
        ),
        ("app/tasks.star", "action(name = \"a\", cmd = [\"true\"], root = \"/\")\n", "root must be a source"),
        (
            "app/tasks.star",
            "fetch(name = \"f\", cmd = [\"true\"], root = image(\"a@sha256:00\"), inputs = {})\n",
            "lock files",
        ),
        ("app/tasks.star", "load(\"//app/tasks.star\", \"x\")\n", "load cycle"),
    ];
    for (path, content, expected) in cases {
        let message = error_of(&[(path, content)]);
        assert!(message.contains(expected), "expected {expected:?} in:\n{message}");
    }
}

#[test]
fn a_package_without_tasks_is_reported() {
    let dir = workspace();
    assert!(matches!(evaluate(dir.path(), "nothing/here", PLATFORM), Err(EvalError::NoTasks(_))));
}

#[test]
fn labels_parse() {
    assert_eq!(parse_label("//services/orders:check"), Some(("services/orders", "check")));
    assert_eq!(parse_label("//:check"), Some(("", "check")));
    assert_eq!(parse_label("services:check"), None);
    assert_eq!(parse_label("//a//b:c"), None);
    assert_eq!(parse_label("//a:"), None);
}

fn split_workspace(call: &str) -> TempDir {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "tests/a.sh", "true");
    write(dir.path(), "tests/deep/b.sh", "true");
    write(dir.path(), "lib/common.sh", "");
    write(
        dir.path(),
        "tasks.star",
        &format!("load(\"@std//test.star\", \"split\")\nunit = {call}\ngoal(\"check\", require = [unit])\n"),
    );
    dir
}

#[test]
fn a_module_call_checks_its_arguments_and_makes_its_actions() {
    let call = format!(
        "split(name = \"unit\", files = \"tests/**/*.sh\", command = \"sh {{file}}\", image = {BASE:?}, inputs = [\"lib/**\"])"
    );
    let dir = split_workspace(&call);
    let package = evaluate(dir.path(), "", PLATFORM).unwrap();
    let labels: Vec<&str> = package.actions.keys().map(String::as_str).collect();
    assert_eq!(labels, ["//:unit", "//:unit.tests_a.sh", "//:unit.tests_deep_b.sh"]);
    let a = &package.actions["//:unit.tests_a.sh"];
    assert!(a.command[2].contains("sh tests/a.sh"));
    assert!(matches!(&a.inputs["/src"], SourceSpec::Glob { include, .. } if include == &["tests/a.sh", "lib/**"]));
    let join = &package.actions["//:unit"];
    assert_eq!(join.inputs.len(), 2);
    assert!(join.outputs.contains("reports"));

    let errors = [
        (
            "split(name = \"unit\", files = 1, command = \"x\", image = \"i\")",
            "files: a string or a list of strings is needed, not int",
        ),
        ("split(name = \"unit\", files = \"tests/*.sh\", image = \"i\")", "command is required"),
        (
            "split(name = \"unit\", files = \"tests/*.sh\", command = \"x\", image = \"i\", speed = 2)",
            "has no parameter \"speed\"",
        ),
        ("split(files = \"tests/*.sh\", command = \"x\", image = \"i\")", "a call needs a name"),
        ("split(\"unit\")", "positional"),
    ];
    for (call, expected) in errors {
        let dir = split_workspace(call);
        let message = evaluate(dir.path(), "", PLATFORM).unwrap_err().to_string();
        assert!(message.contains(expected), "{call}: {message}");
    }
}

#[test]
fn a_file_that_ends_inside_brackets_names_the_open_bracket() {
    let message =
        error_of(&[("app/tasks.star", "x = [\"(\"]  # (\na = action(\n    name = \"a\",\n    cmd = [\"true\"],\n\n")]);
    assert!(message.contains("the file ends before the '(' on line 2 is closed"), "{message}");
    assert!(message.contains("//app/tasks.star:2:11"), "{message}");
}
