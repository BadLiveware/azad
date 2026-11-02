#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use azad_gha_runner::plan::{JobPlan, JobResult, PLAN_VERSION, RunDefaults, Step, StepKind, paths};
use azad_gha_runner::{JobOutcome, Layout, run};
use serde_json::json;
use tempfile::TempDir;

/// A sandbox layout in a temporary directory.
struct Sandbox {
    root: TempDir,
    layout: Layout,
}

impl Sandbox {
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
        Self { root, layout }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.path().join(relative)
    }

    fn run(&self, plan: &JobPlan) -> (JobOutcome, JobResult) {
        fs::write(&self.layout.plan_file, serde_json::to_string(plan).unwrap()).unwrap();
        let outcome = run(&self.layout);
        let result = serde_json::from_str(&fs::read_to_string(self.layout.result_file()).unwrap()).unwrap();
        (outcome, result)
    }

    /// Gives this sandbox the result and artifacts of a job that ran in another one.
    fn add_need(&self, job: &str, done: &Sandbox) {
        let target = self.layout.needs_dir.join(job);
        copy_dir(&done.layout.work_dir.join(paths::META_OUTPUT), &target.join(paths::META_OUTPUT));
        copy_dir(&done.layout.artifacts_dir(), &target.join(paths::ARTIFACTS_OUTPUT));
    }

    fn add_need_result(&self, job: &str, result: &str, outputs: &[(&str, &str)]) {
        let file = self.layout.needs_dir.join(job).join("meta/result.json");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        let result = JobResult { result: result.to_string(), outputs: text_map(outputs) };
        fs::write(file, serde_json::to_string(&result).unwrap()).unwrap();
    }
}

fn copy_dir(source: &Path, target: &Path) {
    fs::create_dir_all(target).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        match path.is_dir() {
            true => copy_dir(&path, &target.join(entry.file_name())),
            false => {
                fs::copy(&path, target.join(entry.file_name())).unwrap();
            }
        }
    }
}

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn text_map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(key, value)| (key.to_string(), value.to_string())).collect()
}

fn plan(steps: Vec<Step>) -> JobPlan {
    JobPlan {
        version: PLAN_VERSION,
        workflow: "ci".to_string(),
        job: "build".to_string(),
        condition: None,
        needs: Vec::new(),
        contexts: BTreeMap::from([(
            "github".to_string(),
            json!({ "repository": "owner/repo", "sha": "abc123", "ref": "refs/heads/main", "event_name": "push" }),
        )]),
        workflow_env: BTreeMap::new(),
        job_env: BTreeMap::new(),
        defaults: RunDefaults::default(),
        steps,
        outputs: BTreeMap::new(),
        actions: BTreeMap::new(),
        part: None,
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

fn uses(id: &str, action: &str, with: &[(&str, &str)]) -> Step {
    Step { shell: None, kind: StepKind::Uses { action: action.to_string(), with: text_map(with) }, ..sh(id, "") }
}

fn with_condition(step: Step, condition: &str) -> Step {
    Step { condition: Some(condition.to_string()), ..step }
}

fn outputs(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    text_map(pairs)
}

#[test]
fn a_later_step_reads_the_outputs_of_an_earlier_one() {
    let sandbox = Sandbox::new();
    let mut job = plan(vec![
        sh(
            "first",
            "echo \"value=one\" >> \"$GITHUB_OUTPUT\"\nprintf 'text<<END\\nline 1\\nline 2\\nEND\\n' >> \"$GITHUB_OUTPUT\"\necho '::set-output name=old::legacy'",
        ),
        sh(
            "second",
            "echo \"joined=${{ steps.first.outputs.value }}-${{ steps.first.outputs.old }}\" >> \"$GITHUB_OUTPUT\"",
        ),
    ]);
    job.outputs = outputs(&[
        ("joined", "${{ steps.second.outputs.joined }}"),
        ("text", "${{ steps.first.outputs.text }}"),
        ("outcome", "${{ steps.first.outcome }}"),
    ]);
    let (outcome, result) = sandbox.run(&job);
    assert_eq!(outcome, JobOutcome::Success);
    assert_eq!(result.result, "success");
    assert_eq!(
        result.outputs,
        outputs(&[("joined", "one-legacy"), ("text", "line 1\nline 2"), ("outcome", "success")])
    );
}

#[test]
fn conditions_follow_the_status_of_the_job() {
    let sandbox = Sandbox::new();
    let mut job = plan(vec![
        sh("fails", "echo \"before=yes\" >> \"$GITHUB_OUTPUT\"\nexit 3"),
        sh("normal", "echo should not run"),
        with_condition(sh("on_failure", "echo \"ran=yes\" >> \"$GITHUB_OUTPUT\""), "failure()"),
        with_condition(sh("always", "echo \"ran=yes\" >> \"$GITHUB_OUTPUT\""), "${{ always() }}"),
        with_condition(sh("on_success", "echo should not run"), "success()"),
    ]);
    job.outputs = outputs(&[
        ("fails", "${{ steps.fails.outcome }}/${{ steps.fails.conclusion }}/${{ steps.fails.outputs.before }}"),
        ("normal", "${{ steps.normal.outcome }}/${{ steps.normal.conclusion }}"),
        ("on_failure", "${{ steps.on_failure.outputs.ran }}"),
        ("always", "${{ steps.always.outputs.ran }}"),
        ("on_success", "${{ steps.on_success.outcome }}"),
        ("status", "${{ job.status }}"),
    ]);
    let (outcome, result) = sandbox.run(&job);
    assert_eq!(outcome.exit_code(), 3, "the code of the step that failed the job");
    assert_eq!(result.result, "failure");
    assert_eq!(
        result.outputs,
        outputs(&[
            ("fails", "failure/failure/yes"),
            ("normal", "skipped/skipped"),
            ("on_failure", "yes"),
            ("always", "yes"),
            ("on_success", "skipped"),
            ("status", "failure"),
        ])
    );
}

#[test]
fn continue_on_error_keeps_the_job_going() {
    let sandbox = Sandbox::new();
    let mut job = plan(vec![
        Step { continue_on_error: Some("true".to_string()), ..sh("allowed", "exit 1") },
        Step {
            continue_on_error: Some("${{ github.event_name == 'push' }}".to_string()),
            ..sh("allowed_by_expression", "exit 1")
        },
        sh("after", "echo \"ran=yes\" >> \"$GITHUB_OUTPUT\""),
    ]);
    job.outputs = outputs(&[
        ("allowed", "${{ steps.allowed.outcome }}/${{ steps.allowed.conclusion }}"),
        ("expression", "${{ steps.allowed_by_expression.conclusion }}"),
        ("after", "${{ steps.after.outputs.ran }}"),
    ]);
    let (outcome, result) = sandbox.run(&job);
    assert_eq!(outcome, JobOutcome::Success);
    assert_eq!(result.outputs, outputs(&[("allowed", "failure/success"), ("expression", "success"), ("after", "yes")]));
}

#[test]
fn the_job_exits_with_the_code_of_the_first_step_that_failed_it() {
    let sandbox = Sandbox::new();
    let job = plan(vec![
        Step { continue_on_error: Some("true".to_string()), ..sh("allowed", "exit 5") },
        sh("fails", "exit 2"),
        with_condition(sh("cleanup", "exit 7"), "always()"),
    ]);
    let (outcome, result) = sandbox.run(&job);
    assert_eq!(outcome.exit_code(), 2);
    assert_eq!(result.result, "failure");
}

#[test]
fn a_failed_step_in_a_composite_action_gives_its_code_to_the_job() {
    let sandbox = Sandbox::new();
    let repository = sandbox.path("actions/failing");
    write(&repository.join("action.yml"), "runs:\n  using: composite\n  steps:\n    - shell: sh\n      run: exit 4\n");
    let mut job = plan(vec![uses("failing", "owner/failing@v1", &[])]);
    job.actions = text_map(&[("owner/failing@v1", &repository.to_string_lossy())]);
    let (outcome, _) = sandbox.run(&job);
    assert_eq!(outcome.exit_code(), 4);
}

#[test]
fn env_layers_and_github_env_and_github_path() {
    let sandbox = Sandbox::new();
    let tools = sandbox.path("tools");
    write(&tools.join("hello-tool"), "#!/bin/sh\necho hello from the tool\n");
    fs::set_permissions(tools.join("hello-tool"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let mut job = plan(vec![
        sh(
            "write",
            &format!(
                "echo \"FROM_STEP=written\" >> \"$GITHUB_ENV\"\necho \"JOB_LEVEL=overridden\" >> \"$GITHUB_ENV\"\nprintf 'MULTI<<EOF\\na\\nb\\nEOF\\n' >> \"$GITHUB_ENV\"\necho \"{}\" >> \"$GITHUB_PATH\"\necho \"FROM_STEP=$FROM_STEP\" >> \"$GITHUB_OUTPUT\"",
                tools.display()
            ),
        ),
        Step {
            env: text_map(&[("STEP_LEVEL", "${{ env.JOB_LEVEL }}+step")]),
            ..sh(
                "read",
                "{\n echo \"workflow=$WORKFLOW_LEVEL\"\n echo \"job=$JOB_LEVEL\"\n echo \"step=$STEP_LEVEL\"\n echo \"from=$FROM_STEP\"\n echo \"context=${{ env.FROM_STEP }}\"\n echo \"tool=$(hello-tool)\"\n echo \"ci=$CI/$GITHUB_ACTIONS/$RUNNER_OS/$GITHUB_REPOSITORY/$GITHUB_SHA/$GITHUB_JOB\"\n echo \"workspace=$PWD\"\n} >> \"$GITHUB_OUTPUT\"\nprintf 'multi<<X\\n%s\\nX\\n' \"$MULTI\" >> \"$GITHUB_OUTPUT\"",
            )
        },
    ]);
    job.workflow_env = text_map(&[("WORKFLOW_LEVEL", "w")]);
    job.job_env = text_map(&[("JOB_LEVEL", "${{ env.WORKFLOW_LEVEL }}+job")]);
    job.outputs = outputs(&[
        ("before", "${{ steps.write.outputs.FROM_STEP }}"),
        ("workflow", "${{ steps.read.outputs.workflow }}"),
        ("job", "${{ steps.read.outputs.job }}"),
        ("step", "${{ steps.read.outputs.step }}"),
        ("from", "${{ steps.read.outputs.from }}"),
        ("context", "${{ steps.read.outputs.context }}"),
        ("tool", "${{ steps.read.outputs.tool }}"),
        ("ci", "${{ steps.read.outputs.ci }}"),
        ("multi", "${{ steps.read.outputs.multi }}"),
        ("workspace", "${{ steps.read.outputs.workspace == github.workspace }}"),
    ]);
    let (outcome, result) = sandbox.run(&job);
    assert_eq!(outcome, JobOutcome::Success);
    assert_eq!(
        result.outputs,
        outputs(&[
            ("before", ""),
            ("workflow", "w"),
            ("job", "overridden"),
            ("step", "overridden+step"),
            ("from", "written"),
            ("context", "written"),
            ("tool", "hello from the tool"),
            ("ci", "true/true/Linux/owner/repo/abc123/build"),
            ("multi", "a\nb"),
            ("workspace", "true"),
        ])
    );
}

#[test]
fn the_step_env_wins_over_github_env() {
    let sandbox = Sandbox::new();
    let mut job = plan(vec![
        sh("write", "echo \"VALUE=from-file\" >> \"$GITHUB_ENV\""),
        Step { env: text_map(&[("VALUE", "from-step")]), ..sh("read", "echo \"value=$VALUE\" >> \"$GITHUB_OUTPUT\"") },
    ]);
    job.outputs = outputs(&[("value", "${{ steps.read.outputs.value }}")]);
    let (_, result) = sandbox.run(&job);
    assert_eq!(result.outputs, outputs(&[("value", "from-step")]));
}

#[test]
fn working_directory_and_defaults() {
    let sandbox = Sandbox::new();
    let mut job = plan(vec![
        sh("make", "mkdir -p sub/deeper"),
        Step {
            working_directory: Some("sub/deeper".to_string()),
            ..sh("step_dir", "echo \"dir=$(basename \"$PWD\")\" >> \"$GITHUB_OUTPUT\"")
        },
        sh("default_dir", "echo \"dir=$(basename \"$PWD\")\" >> \"$GITHUB_OUTPUT\""),
    ]);
    job.defaults.working_directory = Some("sub".to_string());
    job.steps[0].working_directory = Some(".".to_string());
    job.outputs =
        outputs(&[("step", "${{ steps.step_dir.outputs.dir }}"), ("default", "${{ steps.default_dir.outputs.dir }}")]);
    let (outcome, result) = sandbox.run(&job);
    assert_eq!(outcome, JobOutcome::Success);
    assert_eq!(result.outputs, outputs(&[("step", "deeper"), ("default", "sub")]));
}

#[test]
fn a_composite_action_gets_inputs_and_gives_outputs() {
    let sandbox = Sandbox::new();
    let repository = sandbox.path("actions/greeter");
    write(
        &repository.join("greet/action.yml"),
        r#"
name: greet
inputs:
  who:
    required: true
  greeting:
    default: hello from ${{ github.repository }}
outputs:
  message:
    value: ${{ steps.make.outputs.message }}
  shouted:
    value: ${{ steps.shout.outputs.text }}
runs:
  using: composite
  steps:
    - id: make
      shell: sh
      run: echo "message=${{ inputs.greeting }}, $WHO" >> "$GITHUB_OUTPUT"
      env:
        WHO: ${{ inputs.who }}
    - id: shout
      shell: sh
      run: |
        . "$GITHUB_ACTION_PATH/lib.sh"
        echo "text=$(shout '${{ steps.make.outputs.message }}')" >> "$GITHUB_OUTPUT"
    - id: skipped
      if: failure()
      shell: sh
      run: exit 1
"#,
    );
    write(&repository.join("greet/lib.sh"), "shout() { printf '%s' \"$1\" | tr '[:lower:]' '[:upper:]'; }\n");
    let mut job = plan(vec![uses("greet", "owner/greeter/greet@v1", &[("who", "${{ github.sha }}")])]);
    job.actions = text_map(&[("owner/greeter/greet@v1", &repository.to_string_lossy())]);
    job.outputs = outputs(&[
        ("message", "${{ steps.greet.outputs.message }}"),
        ("shouted", "${{ steps.greet.outputs.shouted }}"),
    ]);
    let (outcome, result) = sandbox.run(&job);
    assert_eq!(outcome, JobOutcome::Success);
    assert_eq!(
        result.outputs,
        outputs(&[("message", "hello from owner/repo, abc123"), ("shouted", "HELLO FROM OWNER/REPO, ABC123"),])
    );
}

#[test]
fn a_local_composite_action_must_name_its_shells() {
    let sandbox = Sandbox::new();
    write(
        &sandbox.path("src/.github/actions/bad/action.yml"),
        "runs:\n  using: composite\n  steps:\n    - run: echo no shell\n",
    );
    let job = plan(vec![uses("checkout", "actions/checkout@v4", &[]), uses("bad", "./.github/actions/bad", &[])]);
    let (outcome, _) = sandbox.run(&job);
    assert!(matches!(outcome, JobOutcome::Failure { .. }), "{outcome:?}");
}

#[test]
fn a_job_is_skipped_when_a_need_failed() {
    let sandbox = Sandbox::new();
    sandbox.add_need_result("test", "failure", &[]);
    let mut job = plan(vec![sh("never", "exit 1")]);
    job.needs = vec!["test".to_string()];
    let (outcome, result) = sandbox.run(&job);
    assert_eq!(outcome, JobOutcome::Skipped);
    assert_eq!(outcome.exit_code(), 0);
    assert_eq!(result.result, "skipped");
}

#[test]
fn a_job_with_always_runs_after_a_failed_need_and_reads_its_outputs() {
    let sandbox = Sandbox::new();
    sandbox.add_need_result("test", "failure", &[("report", "3 failed")]);
    let mut job = plan(vec![sh(
        "notify",
        "echo \"text=${{ needs.test.result }}: ${{ needs.test.outputs.report }}\" >> \"$GITHUB_OUTPUT\"",
    )]);
    job.needs = vec!["test".to_string()];
    job.condition = Some("always()".to_string());
    job.outputs = outputs(&[("text", "${{ steps.notify.outputs.text }}")]);
    let (outcome, result) = sandbox.run(&job);
    assert_eq!(outcome, JobOutcome::Success);
    assert_eq!(result.outputs, outputs(&[("text", "failure: 3 failed")]));
}

#[test]
fn a_later_job_downloads_the_artifact_of_an_earlier_one() {
    let producer = Sandbox::new();
    let upload = plan(vec![
        sh("build", "mkdir -p dist/sub\necho one > dist/a.txt\necho two > dist/sub/b.txt"),
        uses("upload", "actions/upload-artifact@v4", &[("name", "dist"), ("path", "dist")]),
        uses(
            "nothing",
            "actions/upload-artifact@v4",
            &[("name", "empty"), ("path", "missing/*"), ("if-no-files-found", "ignore")],
        ),
    ]);
    let (outcome, _) = producer.run(&upload);
    assert_eq!(outcome, JobOutcome::Success);
    assert!(producer.layout.artifacts_dir().join("dist/sub/b.txt").is_file());

    let consumer = Sandbox::new();
    consumer.add_need("producer", &producer);
    let mut download = plan(vec![
        uses("named", "actions/download-artifact@v4", &[("name", "dist"), ("path", "out")]),
        uses("all", "actions/download-artifact@v4", &[("path", "everything")]),
        sh(
            "read",
            "echo \"named=$(cat out/a.txt)$(cat out/sub/b.txt)\" >> \"$GITHUB_OUTPUT\"\necho \"all=$(cat everything/dist/sub/b.txt)\" >> \"$GITHUB_OUTPUT\"",
        ),
    ]);
    download.needs = vec!["producer".to_string()];
    download.outputs =
        outputs(&[("named", "${{ steps.read.outputs.named }}"), ("all", "${{ steps.read.outputs.all }}")]);
    let (outcome, result) = consumer.run(&download);
    assert_eq!(outcome, JobOutcome::Success);
    assert_eq!(result.outputs, outputs(&[("named", "onetwo"), ("all", "two")]));
}

#[test]
fn an_upload_without_files_fails_when_asked_to() {
    let sandbox = Sandbox::new();
    let job = plan(vec![uses(
        "upload",
        "actions/upload-artifact@v4",
        &[("path", "missing.txt"), ("if-no-files-found", "error")],
    )]);
    let (outcome, _) = sandbox.run(&job);
    assert!(matches!(outcome, JobOutcome::Failure { .. }), "{outcome:?}");
}

#[test]
fn checkout_copies_the_source_and_fails_clearly_without_it() {
    let sandbox = Sandbox::new();
    write(&sandbox.path("src/README.md"), "readme");
    let mut job = plan(vec![
        uses("checkout", "actions/checkout@v4", &[("path", "repo"), ("fetch-depth", "0")]),
        sh("read", "echo \"text=$(cat repo/README.md)\" >> \"$GITHUB_OUTPUT\""),
    ]);
    job.outputs = outputs(&[("text", "${{ steps.read.outputs.text }}")]);
    let (outcome, result) = sandbox.run(&job);
    assert_eq!(outcome, JobOutcome::Success);
    assert_eq!(result.outputs, outputs(&[("text", "readme")]));

    let without_source = Sandbox::new();
    let (outcome, _) = without_source.run(&plan(vec![uses("checkout", "actions/checkout@v4", &[])]));
    assert!(matches!(outcome, JobOutcome::Failure { .. }), "{outcome:?}");
}

#[test]
fn cache_actions_do_nothing_and_miss() {
    let sandbox = Sandbox::new();
    let mut job = plan(vec![uses(
        "cache",
        "actions/cache@v4",
        &[("path", "~/.npm"), ("key", "npm-${{ hashFiles('**/package-lock.json') }}")],
    )]);
    job.outputs = outputs(&[("hit", "${{ steps.cache.outputs.cache-hit }}")]);
    let (outcome, result) = sandbox.run(&job);
    assert_eq!(outcome, JobOutcome::Success);
    assert_eq!(result.outputs, outputs(&[("hit", "false")]));
}

#[test]
fn hash_files_reads_the_workspace() {
    let sandbox = Sandbox::new();
    let mut job = plan(vec![sh("write", "echo a > a.lock")]);
    job.outputs = outputs(&[("some", "${{ hashFiles('*.lock') != '' }}"), ("none", "${{ hashFiles('*.missing') }}")]);
    let (_, result) = sandbox.run(&job);
    assert_eq!(result.outputs, outputs(&[("some", "true"), ("none", "")]));
}

#[test]
fn the_token_is_empty() {
    let sandbox = Sandbox::new();
    let mut job = plan(vec![]);
    job.contexts.insert("secrets".to_string(), json!({ "GITHUB_TOKEN": "real", "OTHER": "kept" }));
    job.outputs =
        outputs(&[("token", "${{ github.token }}${{ secrets.GITHUB_TOKEN }}"), ("other", "${{ secrets.OTHER }}")]);
    let (_, result) = sandbox.run(&job);
    assert_eq!(result.outputs, outputs(&[("token", ""), ("other", "kept")]));
}

#[test]
fn unsupported_shells_and_docker_actions_fail_the_step() {
    let sandbox = Sandbox::new();
    let repository = sandbox.path("actions/docker");
    write(&repository.join("action.yml"), "runs:\n  using: docker\n  image: Dockerfile\n");
    let mut job = plan(vec![
        Step {
            shell: Some("pwsh".to_string()),
            continue_on_error: Some("true".to_string()),
            ..sh("pwsh", "Write-Host hi")
        },
        uses("docker", "owner/docker@v1", &[]),
    ]);
    job.actions = text_map(&[("owner/docker@v1", &repository.to_string_lossy())]);
    job.outputs = outputs(&[("pwsh", "${{ steps.pwsh.outcome }}"), ("docker", "${{ steps.docker.outcome }}")]);
    let (outcome, result) = sandbox.run(&job);
    assert!(matches!(outcome, JobOutcome::Failure { .. }), "{outcome:?}");
    assert_eq!(result.outputs, outputs(&[("pwsh", "failure"), ("docker", "failure")]));
}

#[test]
fn refuses_another_plan_version() {
    let sandbox = Sandbox::new();
    let mut job = plan(vec![sh("never", "exit 0")]);
    job.version = PLAN_VERSION + 1;
    let (outcome, result) = sandbox.run(&job);
    assert!(matches!(outcome, JobOutcome::Failure { .. }), "{outcome:?}");
    assert_eq!(result.result, "failure");
}

#[test]
fn a_node_action_reads_an_input_and_writes_an_output() {
    let has_node = std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|directory| directory.join("node").is_file()));
    if !has_node {
        eprintln!("skipped: no `node` on PATH");
        return;
    }
    let sandbox = Sandbox::new();
    let repository = sandbox.path("actions/node");
    write(
        &repository.join("action.yml"),
        "inputs:\n  name:\n    required: true\n  greeting:\n    default: hi\nruns:\n  using: node20\n  pre: pre.js\n  main: main.js\n  post: post.js\n",
    );
    let append =
        "const fs = require('fs'); const append = (file, line) => fs.appendFileSync(process.env[file], line + '\\n');";
    write(&repository.join("pre.js"), &format!("{append}\nappend('GITHUB_STATE', 'started=yes');"));
    write(
        &repository.join("main.js"),
        &format!(
            "{append}\nappend('GITHUB_OUTPUT', `text=${{process.env.INPUT_GREETING}} ${{process.env.INPUT_NAME}} ${{process.env.STATE_started}}`);\nconsole.log('::set-output name=legacy::ok');"
        ),
    );
    write(
        &repository.join("post.js"),
        &format!(
            "{append}\nrequire('fs').writeFileSync(process.env.GITHUB_WORKSPACE + '/post.txt', process.env.STATE_started);"
        ),
    );
    let mut job = plan(vec![uses("node", "owner/node@v2", &[("name", "${{ github.sha }}")])]);
    job.actions = text_map(&[("owner/node@v2", &repository.to_string_lossy())]);
    job.outputs =
        outputs(&[("text", "${{ steps.node.outputs.text }}"), ("legacy", "${{ steps.node.outputs.legacy }}")]);
    let (outcome, result) = sandbox.run(&job);
    assert_eq!(outcome, JobOutcome::Success);
    assert_eq!(result.outputs, outputs(&[("text", "hi abc123 yes"), ("legacy", "ok")]));
    let post = fs::read_to_string(sandbox.layout.workspace().join("post.txt")).unwrap();
    assert_eq!(post, "yes");
}
