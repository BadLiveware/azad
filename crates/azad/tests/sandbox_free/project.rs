//! `@std//project.star`: a project's goals from parts (decision 0025).

#![allow(clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::fs;

use azad::frontend::Package;
use azad::frontend::starlark::evaluate;
use tempfile::TempDir;

const PLATFORM: &str = "linux/amd64";

/// Two modules' parts, as a repository's own modules would give them: a .NET-like preset, and a Node-like part.
const PARTS: &str = r#"
load("@std//project.star", "part", "project")

def dotnet_standard():
    fmt = task(name = "dotnet-fmt", cmd = "true")
    check = task(name = "dotnet-fmt-check", cmd = "true")
    test = task(name = "dotnet-test", cmd = "true")
    return [part(fmt = fmt, fmt_check = check), part(test = test, ci = ["test"])]

def node(lint = True):
    lint_task = None
    if lint:
        lint_task = task(name = "node-lint", cmd = "true")
    return part(fmt = task(name = "node-fmt", cmd = "true"), lint = lint_task, ci = ["fmt"])
"#;

fn evaluated(tasks: &str) -> Result<Package, String> {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("tasks.star"), format!("{PARTS}\n{tasks}\n")).unwrap();
    evaluate(dir.path(), "", PLATFORM).map_err(|error| error.to_string())
}

fn labels(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| format!("//:{name}")).collect()
}

/// Two kinds of project in one directory: each role's goal requires the tasks of both, and `ci` requires only the
/// roles that each part names, so the .NET formatting check stays out while the Node formatting is in.
#[test]
fn the_goals_of_a_project_are_the_roles_of_its_parts() {
    let package = evaluated("project(dotnet_standard(), node())").unwrap();
    let goals: Vec<&str> = package.goals.keys().map(String::as_str).collect();
    assert_eq!(goals, ["//:ci", "//:fmt", "//:fmt-check", "//:lint", "//:test"]);
    assert_eq!(package.goals["//:fmt"], labels(&["dotnet-fmt", "node-fmt"]));
    assert_eq!(package.goals["//:fmt-check"], labels(&["dotnet-fmt-check"]));
    assert_eq!(package.goals["//:ci"], labels(&["dotnet-test", "node-fmt"]));
    assert_eq!(package.events["//:ci"], ["pull_request".to_owned(), "push".to_owned()].into());
    assert_eq!(package.docs["//:fmt"], "Format the files");
}

/// A part leaves out a role that is None or an empty list, and a project that takes no CI role has no goal `ci`.
#[test]
fn a_role_that_is_none_or_empty_is_left_out() {
    let package = evaluated("project(node(lint = False))").unwrap();
    assert!(!package.goals.contains_key("//:lint"));
    let package =
        evaluated("project(part(fmt = task(name = \"own-fmt\", cmd = \"true\"), test = [], ci = [\"fmt\"]))").unwrap();
    assert!(!package.goals.contains_key("//:test"));
    let package = evaluated("project(part(fmt = task(name = \"own-fmt\", cmd = \"true\")))").unwrap();
    assert!(!package.goals.contains_key("//:ci"));
}

#[test]
fn a_part_or_a_project_that_is_wrong_fails_and_says_why() {
    let cases = [
        ("project(part(fmt = task(name = \"f\", cmd = \"true\"), ci = [\"test\"]))", "which the part has no task for"),
        ("project(\"fmt\")", "an argument is a part() or a list of parts"),
        (
            "project(part(fmt = task(name = \"f\", cmd = \"true\"), test = [], ci = [\"test\"]))",
            "which the part has no task for",
        ),
        ("project(part(test = task(name = \"test\", cmd = \"true\")))", "is also a task or an action"),
        (
            "goal(\"own\", require = [task(name = \"t\", cmd = \"true\")])\ntask(name = \"own\", cmd = \"true\")",
            "is also a goal",
        ),
    ];
    for (tasks, expected) in cases {
        let error = evaluated(tasks).unwrap_err();
        assert!(error.contains(expected), "{tasks}: {error}");
    }
}
