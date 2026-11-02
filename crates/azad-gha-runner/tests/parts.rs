//! GHA+ (decision 0013): a job split at its hinted steps gives the same result as the whole job.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use azad_gha_runner::plan::{
    Hint, HintKind, JobPlan, JobResult, PLAN_VERSION, Part, RunDefaults, Step, StepKind, paths,
};
use azad_gha_runner::{JobOutcome, Layout, run, run_hinted};
use serde_json::json;
use tempfile::TempDir;

/// The directories of one action, as the sandbox shows them.
struct Action {
    root: TempDir,
    layout: Layout,
}

impl Action {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        let base = root.path();
        let layout = Layout {
            plan_file: base.join("job/job.json"),
            work_dir: base.join("work"),
            source_dir: base.join("src"),
            needs_dir: base.join("needs"),
            github_dir: base.join("github"),
            state_dir: base.join("state"),
            hinted_dir: base.join("hinted"),
            hint_dir: base.join("hint"),
        };
        fs::create_dir_all(base.join("job")).unwrap();
        fs::create_dir_all(&layout.work_dir).unwrap();
        Action { root, layout }
    }

    fn run(&self, plan: &JobPlan) -> JobOutcome {
        fs::write(&self.layout.plan_file, serde_json::to_string(plan).unwrap()).unwrap();
        run(&self.layout)
    }

    fn output(&self, name: &str) -> std::path::PathBuf {
        self.layout.work_dir.join(name)
    }

    fn result(&self) -> JobResult {
        serde_json::from_str(&fs::read_to_string(self.layout.result_file()).unwrap()).unwrap()
    }
}

/// Copies an output of one action to an input of another, read-only as the store gives it.
fn give(source: &Path, target: &Path) {
    copy(source, target);
}

fn copy(source: &Path, target: &Path) {
    if source.is_file() {
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::copy(source, target).unwrap();
        let mut permissions = fs::metadata(target).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(target, permissions).unwrap();
        return;
    }
    fs::create_dir_all(target).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        copy(&entry.path(), &target.join(entry.file_name()));
    }
}

fn sh(id: &str, script: &str) -> Step {
    Step {
        id: Some(id.to_string()),
        name: None,
        condition: None,
        env: BTreeMap::new(),
        continue_on_error: None,
        working_directory: None,
        shell: Some("sh".to_string()),
        kind: StepKind::Run { script: script.to_string() },
        hint: None,
        module: None,
    }
}

fn hinted(step: Step, inputs: &[&str], outputs: &[&str]) -> Step {
    let hint = Hint {
        kind: HintKind::Hermetic,
        inputs: inputs.iter().map(|s| (*s).to_owned()).collect(),
        outputs: outputs.iter().map(|s| (*s).to_owned()).collect(),
    };
    Step { hint: Some(hint), ..step }
}

fn plan(steps: Vec<Step>) -> JobPlan {
    JobPlan {
        version: PLAN_VERSION,
        workflow: "ci".to_string(),
        job: "build".to_string(),
        condition: None,
        needs: Vec::new(),
        contexts: BTreeMap::from([("github".to_string(), json!({ "repository": "owner/repo", "sha": "abc123" }))]),
        workflow_env: BTreeMap::new(),
        job_env: BTreeMap::from([("LEVEL".to_string(), "job".to_string())]),
        defaults: RunDefaults::default(),
        steps,
        outputs: BTreeMap::from([
            ("made".to_string(), "${{ steps.make.outputs.made }}".to_string()),
            ("used".to_string(), "${{ steps.use.outputs.used }}".to_string()),
        ]),
        actions: BTreeMap::new(),
        part: None,
    }
}

/// `prepare` writes files and env; `make` (hinted) reads one file and writes another; `use` reads what `make` gave.
fn steps(make_condition: Option<&str>) -> Vec<Step> {
    let prepare = sh(
        "prepare",
        "echo 'source' > in.txt && echo 'not declared' > other.txt && echo FROM_PREPARE=yes >> \"$GITHUB_ENV\" && \
         mkdir -p \"$RUNNER_TOOL_CACHE/tool/bin\" && printf '#!/bin/sh\\necho tool' > \"$RUNNER_TOOL_CACHE/tool/bin/tool\" && \
         chmod +x \"$RUNNER_TOOL_CACHE/tool/bin/tool\" && echo \"$RUNNER_TOOL_CACHE/tool/bin\" >> \"$GITHUB_PATH\"",
    );
    let make = Step {
        condition: make_condition.map(str::to_owned),
        ..hinted(
            sh(
                "make",
                "mkdir -p out && { cat in.txt; tool; echo \"$FROM_PREPARE $LEVEL\"; } > out/made.txt && \
                 echo made=${{ env.LEVEL }} >> \"$GITHUB_OUTPUT\" && echo FROM_MAKE=yes >> \"$GITHUB_ENV\"",
            ),
            &["in.txt"],
            &["out/**"],
        )
    };
    let using = sh("use", "echo \"used=$(tr '\\n' ' ' < out/made.txt)$FROM_MAKE\" >> \"$GITHUB_OUTPUT\"");
    vec![prepare, make, using]
}

/// Runs the plan as part, hinted step, part, each in its own directories.
fn run_split(plan: &JobPlan) -> (JobOutcome, Option<JobResult>) {
    let first = Action::new();
    let part = Part { first: 0, end: 1, resumes: false, prepares: Some(1) };
    assert_eq!(first.run(&JobPlan { part: Some(part), ..plan.clone() }), JobOutcome::Success);

    let step = Action::new();
    give(&first.output(paths::HINT_OUTPUT), &step.layout.hint_dir);
    give(&first.output(paths::TOOLS_OUTPUT), &step.layout.tool_cache());
    let hinted = run_hinted(&step.layout);
    if hinted != JobOutcome::Success {
        return (hinted, None);
    }

    let last = Action::new();
    give(&first.output(paths::STATE_OUTPUT), &last.layout.state_dir);
    give(&step.output(paths::STEP_RESULT_OUTPUT), &last.layout.hinted_dir);
    let part = Part { first: 2, end: 3, resumes: true, prepares: None };
    let outcome = last.run(&JobPlan { part: Some(part), ..plan.clone() });
    (outcome, Some(last.result()))
}

fn without_hints(steps: Vec<Step>) -> Vec<Step> {
    steps.into_iter().map(|step| Step { hint: None, ..step }).collect()
}

#[test]
fn a_job_split_at_a_hinted_step_gives_the_result_of_the_whole_job() {
    let whole = Action::new();
    assert_eq!(whole.run(&plan(without_hints(steps(None)))), JobOutcome::Success);
    let (outcome, split) = run_split(&plan(steps(None)));
    assert_eq!(outcome, JobOutcome::Success);
    let outputs = split.unwrap().outputs;
    assert_eq!(outputs, whole.result().outputs);
    assert_eq!(outputs["used"], "source tool yes job yes", "{outputs:?}");
    assert_eq!(outputs["made"], "job");
}

#[test]
fn a_hinted_step_does_not_see_files_that_its_inputs_do_not_name() {
    let mut steps = steps(None);
    let StepKind::Run { script } = &mut steps[1].kind else { unreachable!() };
    *script = format!("cat other.txt && {script}");
    let (outcome, _) = run_split(&plan(steps.clone()));
    assert!(matches!(outcome, JobOutcome::Failure { .. }), "{outcome:?}");
    let whole = Action::new();
    assert_eq!(whole.run(&plan(without_hints(steps))), JobOutcome::Success, "the whole job sees it");
}

#[test]
fn a_hinted_step_whose_condition_is_false_is_skipped_in_the_part_after() {
    let mut steps = steps(Some("false"));
    steps[2] = sh("use", "echo \"used=${{ steps.make.outcome }}\" >> \"$GITHUB_OUTPUT\"");
    let (outcome, split) = run_split(&plan(steps));
    assert_eq!(outcome, JobOutcome::Success);
    assert_eq!(split.unwrap().outputs["used"], "skipped");
}

#[test]
fn the_hint_does_not_change_when_other_files_or_the_commit_change() {
    let hint_of = |extra: &str, sha: &str| {
        let mut steps = steps(None);
        let StepKind::Run { script } = &mut steps[0].kind else { unreachable!() };
        *script = format!("echo '{extra}' > unrelated.txt && {script}");
        let mut plan = plan(steps);
        plan.contexts.insert("github".to_string(), json!({ "repository": "owner/repo", "sha": sha }));
        let first = Action::new();
        let part = Part { first: 0, end: 1, resumes: false, prepares: Some(1) };
        first.run(&JobPlan { part: Some(part), ..plan });
        let text = fs::read_to_string(first.output(paths::HINT_OUTPUT).join(paths::HINTED_STEP_FILE)).unwrap();
        // The sandbox has the same paths in every run; these test directories do not.
        text.replace(&first.root.path().display().to_string(), "<root>")
    };
    assert_eq!(hint_of("one", "abc"), hint_of("two", "def"));
}

#[test]
fn a_hinted_step_exits_with_the_code_of_its_script() {
    let mut steps = steps(None);
    let StepKind::Run { script } = &mut steps[1].kind else { unreachable!() };
    *script = "exit 4".to_string();
    let first = Action::new();
    let part = Part { first: 0, end: 1, resumes: false, prepares: Some(1) };
    assert_eq!(first.run(&JobPlan { part: Some(part), ..plan(steps) }), JobOutcome::Success);
    let step = Action::new();
    give(&first.output(paths::HINT_OUTPUT), &step.layout.hint_dir);
    give(&first.output(paths::TOOLS_OUTPUT), &step.layout.tool_cache());
    assert_eq!(run_hinted(&step.layout).exit_code(), 4);
}
