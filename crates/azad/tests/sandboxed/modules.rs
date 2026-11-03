//! Decision 0014: a module call gives the same actions from Starlark and from a GHA workflow.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use azad::frontend::gha::GhaFrontend;
use azad::frontend::starlark::{evaluate, expand_calls};
use azad::frontend::{Frontend, LoadRequest, Package, Severity, SourceSpec};
use azad::ir::Spec;
use azad_gha_runner::plan::{JobPlan, StepKind, paths};
use tempfile::TempDir;

const PLATFORM: &str = "linux/amd64";

fn write(root: &Path, path: &str, content: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

const WORKFLOW: &str = r#"
name: CI
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - id: unit
        uses: BadLiveware/azad/std/test-split@main
        with:
          files: tests/**/*.sh
          command: sh {file}
          inputs: |
            lib/**
      - run: echo after the tests
"#;

/// The package's actions whose labels start with `from`, relabelled to start with `to`, in their outputs too.
fn relabelled(package: &Package, from: &str, to: &str) -> BTreeMap<String, Spec<SourceSpec>> {
    let relabel = |label: &str| label.replacen(from, to, 1);
    package
        .actions
        .iter()
        .filter(|(label, _)| label.starts_with(from))
        .map(|(label, spec)| {
            let mut spec = spec.clone();
            for source in spec.inputs.values_mut() {
                if let SourceSpec::Output { action, .. } = source {
                    *action = relabel(action);
                }
            }
            (relabel(label), spec)
        })
        .collect()
}

#[test]
fn a_module_call_gives_the_same_actions_from_starlark_and_from_a_workflow() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write(root, "tests/a.sh", "true\n");
    write(root, "tests/deep/b.sh", "true\n");
    write(root, "lib/common.sh", "\n");
    write(root, "ci.yml", WORKFLOW);
    let request = LoadRequest { root, target: "ci.yml", platform: PLATFORM };
    let loaded = GhaFrontend::default().load(&request).unwrap();
    assert_eq!(loaded.package.calls.len(), 1);
    let job = &loaded.package.actions["//ci.yml:test"];
    let SourceSpec::Image { reference: image, .. } = &job.root else { panic!("a job's root is an image") };
    let wait = format!("{}/2", paths::MODULES_DIR);
    assert!(
        matches!(&job.inputs[&wait], SourceSpec::Output { action, path } if action == "//ci.yml:test.unit" && path == "reports")
    );
    assert!(!job.inputs.keys().any(|k| k.contains("test-split")), "the module's action is not fetched");
    let SourceSpec::Inline { files } = &job.inputs[paths::PLAN_DIR] else { unreachable!() };
    let plan: JobPlan = serde_json::from_slice(&files[paths::PLAN_FILE].content).unwrap();
    assert!(matches!(&plan.steps[1].kind, StepKind::Uses { .. }));
    assert_eq!(plan.steps[1].module.as_deref(), Some("//ci.yml:test.unit"));

    write(
        root,
        "tasks.star",
        &format!(
            "load(\"@std//test.star\", \"split\")\nunit = split(name = \"unit\", files = [\"tests/**/*.sh\"], command = \"sh {{file}}\", inputs = [\"lib/**\"], image = {image:?})\ngoal(\"check\", require = [unit])\n"
        ),
    );
    let starlark = evaluate(root, "", PLATFORM).unwrap();
    let workflow = expand_calls(&Default::default(), root, PLATFORM, loaded.package).unwrap();
    assert!(workflow.calls.is_empty());
    let from_starlark = relabelled(&starlark, "//:unit", "<call>");
    let from_workflow = relabelled(&workflow, "//ci.yml:test.unit", "<call>");
    assert_eq!(from_starlark.len(), 3, "two tests and the call's action");
    assert_eq!(from_starlark, from_workflow);
}

#[test]
fn module_steps_that_cannot_be_called_are_reported() {
    let dir = TempDir::new().unwrap();
    let workflow = WORKFLOW
        .replace("      - id: unit\n", "      - id: unit\n        if: github.ref == 'refs/heads/main'\n")
        .replace("command: sh {file}", "command: sh {file} ${{ matrix.flag }}");
    write(dir.path(), "ci.yml", &workflow);
    let request = LoadRequest { root: dir.path(), target: "ci.yml", platform: PLATFORM };
    let findings = GhaFrontend::default().check(&request).unwrap();
    let unsupported: Vec<&str> =
        findings.iter().filter(|f| f.severity == Severity::Unsupported).map(|f| f.message.as_str()).collect();
    assert_eq!(unsupported.len(), 2, "{unsupported:?}");
}
