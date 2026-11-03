#![allow(clippy::unwrap_used)]

use std::fs;

use azad::frontend::gha::GhaFrontend;
use azad::frontend::{Frontend, LoadRequest, Severity, SourceSpec};
use azad::ir::Kind;
use azad_gha_runner::plan::{JobPlan, StepKind, paths};
use serde_json::json;
use tempfile::TempDir;

const PLATFORM: &str = "linux/amd64";

fn workflow(content: &str) -> TempDir {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("ci.yml"), content).unwrap();
    dir
}

fn plan_of(spec: &azad::ir::Spec<SourceSpec>) -> JobPlan {
    let SourceSpec::Inline { files } = &spec.inputs[paths::PLAN_DIR] else { panic!("the plan is inline") };
    serde_json::from_slice(&files[paths::PLAN_FILE].content).unwrap()
}

const SAMPLE: &str = r#"
name: CI
on: [push]
env:
  GLOBAL: one
jobs:
  build:
    runs-on: ubuntu-latest
    outputs:
      version: ${{ steps.v.outputs.version }}
    steps:
      - uses: actions/checkout@v4
      - id: v
        run: echo "version=1.2" >> "$GITHUB_OUTPUT"
      - uses: actions/setup-go@v5
        with:
          go-version: "1.24"
      - uses: actions/upload-artifact@v4
        with:
          name: bin
          path: out/
  test:
    needs: build
    runs-on: ${{ matrix.os }}
    strategy:
      matrix:
        os: [ubuntu-latest, ubuntu-24.04]
        go: ["1.23", "1.24"]
        exclude:
          - os: ubuntu-24.04
            go: "1.23"
    steps:
      - uses: actions/download-artifact@v4
        with:
          name: bin
      - run: echo ${{ matrix.go }}
"#;

#[test]
fn a_workflow_compiles_to_one_impure_action_per_job_and_combination() {
    let dir = workflow(SAMPLE);
    let request = LoadRequest { root: dir.path(), target: "ci.yml", platform: PLATFORM };
    let loaded = GhaFrontend::default().load(&request).unwrap();
    assert_eq!(loaded.goal, "//ci.yml:all");
    let labels: Vec<&str> = loaded.package.actions.keys().map(String::as_str).collect();
    assert_eq!(labels, ["//ci.yml:build", "//ci.yml:test.0", "//ci.yml:test.1", "//ci.yml:test.2"]);
    assert_eq!(loaded.package.goals["//ci.yml:test"].len(), 3);

    let build = &loaded.package.actions["//ci.yml:build"];
    assert_eq!(build.kind, Kind::Impure);
    assert!(matches!(&build.inputs[paths::SOURCE_DIR], SourceSpec::GitFiles { dir } if dir.is_empty()));
    let setup_go = format!("{}/actions/setup-go@v5", paths::ACTIONS_DIR);
    assert!(
        matches!(&build.inputs[&setup_go], SourceSpec::Git { url, rev } if url == "https://github.com/actions/setup-go" && rev == "v5")
    );
    assert!(!build.inputs.keys().any(|k| k.contains("checkout")), "checkout is built in");
    let plan = plan_of(build);
    assert_eq!(plan.workflow, "CI");
    assert_eq!(plan.workflow_env["GLOBAL"], "one");
    assert_eq!(plan.outputs["version"], "${{ steps.v.outputs.version }}");
    assert_eq!(plan.actions["actions/setup-go@v5"], setup_go);
    assert!(matches!(&plan.steps[2].kind, StepKind::Uses { with, .. } if with["go-version"] == "1.24"));

    let test = &loaded.package.actions["//ci.yml:test.0"];
    assert!(!test.inputs.contains_key(paths::SOURCE_DIR), "a job without checkout gets no source");
    let meta = format!("{}/build/{}", paths::NEEDS_DIR, paths::META_OUTPUT);
    assert!(
        matches!(&test.inputs[&meta], SourceSpec::Output { action, path } if action == "//ci.yml:build" && path == paths::META_OUTPUT)
    );
    let matrices: Vec<serde_json::Value> = ["//ci.yml:test.0", "//ci.yml:test.1", "//ci.yml:test.2"]
        .iter()
        .map(|l| plan_of(&loaded.package.actions[*l]).contexts["matrix"].clone())
        .collect();
    assert!(!matrices.contains(&serde_json::json!({"os": "ubuntu-24.04", "go": "1.23"})));
}

#[test]
fn a_matrix_expands_in_the_order_of_its_keys_as_github_does() {
    let dir = workflow(
        r#"
jobs:
  test:
    runs-on: ubuntu-latest
    strategy:
      matrix:
        python: ["3.12", "3.13"]
        mode: [fast, slow]
    steps: [{run: "true"}]
"#,
    );
    let request = LoadRequest { root: dir.path(), target: "ci.yml", platform: PLATFORM };
    let loaded = GhaFrontend::default().load(&request).unwrap();
    let contexts: Vec<(serde_json::Value, serde_json::Value)> = (0..4)
        .map(|index| plan_of(&loaded.package.actions[&format!("//ci.yml:test.{index}")]))
        .map(|plan| (plan.contexts["matrix"].clone(), plan.contexts["strategy"].clone()))
        .collect();
    let expected: Vec<(serde_json::Value, serde_json::Value)> =
        [("3.12", "fast"), ("3.12", "slow"), ("3.13", "fast"), ("3.13", "slow")]
            .into_iter()
            .enumerate()
            .map(|(index, (python, mode))| {
                (json!({"python": python, "mode": mode}), json!({"job-index": index, "job-total": 4}))
            })
            .collect();
    assert_eq!(contexts, expected);
    assert_eq!(loaded.package.titles["//ci.yml:test.1"], "//ci.yml:test.1 (3.12, slow)", "GitHub's test (3.12, slow)");
}

#[test]
fn one_job_is_a_goal() {
    let dir = workflow(SAMPLE);
    let request = LoadRequest { root: dir.path(), target: "ci.yml:build", platform: PLATFORM };
    let loaded = GhaFrontend::default().load(&request).unwrap();
    assert_eq!(loaded.goal, "//ci.yml:build");
    assert_eq!(loaded.package.goals[&loaded.goal].iter().collect::<Vec<_>>(), ["//ci.yml:build"]);
}

#[test]
fn what_cannot_run_is_reported_before_anything_runs() {
    let dir = workflow(
        r#"
jobs:
  reuse:
    uses: org/repo/.github/workflows/x.yml@v1
  mac:
    runs-on: macos-latest
    steps: [{run: "true"}]
  db:
    runs-on: ubuntu-latest
    services:
      postgres: {image: postgres}
    steps: [{run: "true"}]
  dynamic:
    runs-on: ubuntu-latest
    strategy:
      matrix: ${{ fromJSON(needs.x.outputs.m) }}
    steps: [{run: "true"}]
  cleanup:
    runs-on: ubuntu-latest
    if: always()
    steps:
      - uses: docker://alpine:3
      - run: echo ${{ secrets.TOKEN }}
"#,
    );
    let request = LoadRequest { root: dir.path(), target: "ci.yml", platform: PLATFORM };
    let findings = GhaFrontend::default().check(&request).unwrap();
    let unsupported: Vec<String> = findings
        .iter()
        .filter(|f| f.severity == Severity::Unsupported)
        .map(|f| format!("{}: {}", f.location, f.message))
        .collect();
    for expected in [
        "reusable workflows",
        "macos-latest has no image",
        "services: needs Docker",
        "a matrix that an expression makes",
        "always()",
        "docker:// actions",
    ] {
        assert!(unsupported.iter().any(|u| u.contains(expected)), "expected {expected:?} in {unsupported:#?}");
    }
    assert!(findings.iter().any(|f| f.severity == Severity::Different && f.message.contains("secrets are empty")));
    let error = GhaFrontend::default().load(&request).unwrap_err().to_string();
    assert!(error.contains("the workflow cannot run"), "{error}");
}

const HINTED: &str = r#"
name: CI
on: [push]
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - id: test
        run: go test ./...
        env:
          GOFLAGS: -mod=mod
          AZAD_KIND: fetch
          AZAD_INPUTS: go.mod go.sum **/*.go
      - run: echo between
      - id: build
        run: go build -o out/app .
        env:
          AZAD_KIND: hermetic
          AZAD_INPUTS: |
            go.mod
            **/*.go
          AZAD_OUTPUTS: out/**
      - if: always()
        run: echo done
  after:
    needs: build
    runs-on: ubuntu-latest
    steps:
      - run: echo after
"#;

#[test]
fn hinted_steps_split_their_job_into_a_chain() {
    let dir = workflow(HINTED);
    let request = LoadRequest { root: dir.path(), target: "ci.yml", platform: PLATFORM };
    let loaded = GhaFrontend::default().load(&request).unwrap();
    let labels: Vec<&str> = loaded.package.actions.keys().map(String::as_str).collect();
    assert_eq!(
        labels,
        [
            "//ci.yml:after",
            "//ci.yml:build",
            "//ci.yml:build.build",
            "//ci.yml:build.part0",
            "//ci.yml:build.part1",
            "//ci.yml:build.test"
        ]
    );
    assert_eq!(loaded.package.goals["//ci.yml:build"].iter().collect::<Vec<_>>(), ["//ci.yml:build"]);
    let after = &loaded.package.actions["//ci.yml:after"];
    let meta = format!("{}/build/{}", paths::NEEDS_DIR, paths::META_OUTPUT);
    assert!(matches!(&after.inputs[&meta], SourceSpec::Output { action, .. } if action == "//ci.yml:build"));

    let test = &loaded.package.actions["//ci.yml:build.test"];
    assert_eq!(test.kind, Kind::Fetch);
    assert_eq!(test.inputs.len(), 3, "the runner, the hint and the tool cache: {:?}", test.inputs.keys());
    assert!(
        matches!(&test.inputs[paths::HINT_DIR], SourceSpec::Output { action, path } if action == "//ci.yml:build.part0" && path == paths::HINT_OUTPUT)
    );
    assert_eq!(loaded.package.actions["//ci.yml:build.build"].kind, Kind::Hermetic);

    let parts: Vec<_> = ["//ci.yml:build.part0", "//ci.yml:build.part1", "//ci.yml:build"]
        .iter()
        .map(|l| plan_of(&loaded.package.actions[*l]).part.unwrap())
        .map(|p| (p.first, p.end, p.resumes, p.prepares))
        .collect();
    assert_eq!(parts, [(0, 1, false, Some(1)), (2, 3, true, Some(3)), (4, 5, true, None)]);
    let last = &loaded.package.actions["//ci.yml:build"];
    assert!(
        matches!(&last.inputs[paths::STATE_DIR], SourceSpec::Output { action, .. } if action == "//ci.yml:build.part1")
    );
    assert!(
        matches!(&last.inputs[paths::HINTED_DIR], SourceSpec::Output { action, .. } if action == "//ci.yml:build.build")
    );

    let plan = plan_of(last);
    let hint = plan.steps[3].hint.as_ref().unwrap();
    assert_eq!(hint.inputs, ["go.mod", "**/*.go"]);
    assert_eq!(hint.outputs, ["out/**"]);
    assert!(!plan.steps[1].env.contains_key("AZAD_KIND"), "hints are not in the step's env");
    assert_eq!(plan.steps[1].env["GOFLAGS"], "-mod=mod");

    let findings = GhaFrontend::default().check(&request).unwrap();
    assert!(
        findings.iter().any(|f| f.location.contains("step 5") && f.message.contains("hinted step before it fails"))
    );
}

#[test]
fn without_hints_a_job_is_one_action_and_bad_hints_are_reported() {
    let plain = HINTED
        .lines()
        .filter(|l| {
            !l.contains("AZAD_") && !l.trim_start().starts_with("go.mod") && !l.trim_start().starts_with("**/*.go")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let dir = workflow(&plain);
    let request = LoadRequest { root: dir.path(), target: "ci.yml:build", platform: PLATFORM };
    let loaded = GhaFrontend::default().load(&request).unwrap();
    assert_eq!(loaded.package.actions.keys().collect::<Vec<_>>(), ["//ci.yml:after", "//ci.yml:build"]);

    let bad = HINTED
        .replace("AZAD_KIND: fetch", "AZAD_KIND: sometimes")
        .replace("AZAD_KIND: hermetic", "AZAD_KIND: ${{ matrix.kind }}");
    let dir = workflow(&bad);
    let request = LoadRequest { root: dir.path(), target: "ci.yml:build", platform: PLATFORM };
    let findings = GhaFrontend::default().check(&request).unwrap();
    let unhinted: Vec<&str> = findings
        .iter()
        .filter(|f| f.severity == Severity::Different && f.message.ends_with("the step runs without its hint"))
        .map(|f| f.message.as_str())
        .collect();
    assert_eq!(unhinted.len(), 2, "{unhinted:?}");
    assert!(GhaFrontend::default().load(&request).is_ok(), "a bad hint does not stop the workflow");
}

#[test]
fn a_hinted_script_that_reads_a_hidden_variable_runs_without_its_hint() {
    for (script, hinted) in [
        ("go build -ldflags \"-X main.commit=$GITHUB_SHA\" -o out/app .", false),
        ("echo ${GITHUB_REF}", false),
        ("echo $GITHUB_SHA_SHORT", true),
        ("go build -ldflags \"-X main.commit=${{ github.sha }}\" -o out/app .", true),
    ] {
        let text = HINTED.replace("run: go build -o out/app .", &format!("run: {script:?}"));
        let dir = workflow(&text);
        let request = LoadRequest { root: dir.path(), target: "ci.yml:build", platform: PLATFORM };
        let loaded = GhaFrontend::default().load(&request).unwrap();
        assert_eq!(loaded.package.actions.contains_key("//ci.yml:build.build"), hinted, "{script}");
        let findings = GhaFrontend::default().check(&request).unwrap();
        let reported =
            findings.iter().any(|f| f.severity == Severity::Different && f.message.contains("the script reads"));
        assert_eq!(reported, !hinted, "{script}");
    }
}
