//! Compiling a workflow to a package: one impure action per job (and per matrix combination) that runs the GHA runner
//! with a job plan. The runner evaluates the expressions that depend on what earlier steps did; this module evaluates
//! only what must be known before the job starts, such as `runs-on` and the matrix.

use std::collections::{BTreeMap, BTreeSet};

use crate::frontend::{Call, Finding, InlineFile, Package, Severity, SourceSpec, WorkspaceView, label};
use crate::ir::{Kind, Spec};
use azad_gha_runner::Layout;
use azad_gha_runner::expr::{Context, JobStatus, Value, evaluate_template, references, template_expressions, to_text};
use azad_gha_runner::plan::{Hint, HintKind, JobPlan, PLAN_VERSION, Part, RunDefaults, Step, StepKind, paths};
use serde_json::Map;

use crate::frontend::gha::matrix;
use crate::frontend::gha::yaml::KeyOrder;

const CHECKOUT: &str = "actions/checkout";
const UPLOAD_ARTIFACT: &str = "actions/upload-artifact";
const DOWNLOAD_ARTIFACT: &str = "actions/download-artifact";
const CACHE_ACTIONS: [&str; 3] = ["actions/cache", "actions/cache/restore", "actions/cache/save"];
const DOCKER_PREFIX: &str = "docker://";
const LOCAL_PREFIX: &str = "./";
const GITHUB_URL: &str = "https://github.com";
const ALL_GOAL: &str = "all";
const UNSUPPORTED_SHELLS: [&str; 2] = ["pwsh", "powershell"];
const STATUS_FUNCTIONS: [&str; 3] = ["always()", "failure()", "cancelled()"];
/// GHA+ hints (decision 0013): `env:` variables that GitHub passes on and Azad reads.
const HINT_KIND_ENV: &str = "AZAD_KIND";
const HINT_INPUTS_ENV: &str = "AZAD_INPUTS";
const HINT_OUTPUTS_ENV: &str = "AZAD_OUTPUTS";
const HINT_KIND_HERMETIC: &str = "hermetic";
const HINT_KIND_FETCH: &str = "fetch";
const STEP_FLAG: &str = "--step";
/// The variables of the run that a hinted step does not see, because they change per run or per commit (decision
/// 0013). A script that reads one would quietly get an empty value, so such a step runs without its hint.
const HIDDEN_FROM_HINTED: [&str; 17] = [
    "GITHUB_REPOSITORY",
    "GITHUB_REPOSITORY_OWNER",
    "GITHUB_SHA",
    "GITHUB_REF",
    "GITHUB_REF_NAME",
    "GITHUB_REF_TYPE",
    "GITHUB_HEAD_REF",
    "GITHUB_BASE_REF",
    "GITHUB_EVENT_NAME",
    "GITHUB_ACTOR",
    "GITHUB_RUN_ID",
    "GITHUB_RUN_NUMBER",
    "GITHUB_RUN_ATTEMPT",
    "GITHUB_SERVER_URL",
    "GITHUB_API_URL",
    "GITHUB_GRAPHQL_URL",
    "GITHUB_EVENT_PATH",
];
const IMAGE_PARAM: &str = "image";
const JOBS_KEY: &str = "jobs";
const STRATEGY_KEY: &str = "strategy";
const MATRIX_KEY: &str = "matrix";

/// A GHA action that is an Azad module (decision 0014). GitHub runs the action itself; Azad calls the module.
struct RecognizedModule {
    /// The action's path without its ref, in lower case.
    action: &'static str,
    file: &'static str,
    symbol: &'static str,
    /// The output of the call's action that the job waits for.
    output: &'static str,
    /// The module has an `image` parameter, which is the job's image when `with:` does not give it.
    takes_image: bool,
}

const MODULES: [RecognizedModule; 1] = [RecognizedModule {
    action: "badliveware/azad/std/test-split",
    file: "@std//test.star",
    symbol: "split",
    output: "reports",
    takes_image: true,
}];

fn recognized_module(action: &str) -> Option<&'static RecognizedModule> {
    let path = action.split('@').next().unwrap_or(action).to_lowercase();
    MODULES.iter().find(|module| module.action == path)
}

/// The actions of one job: those that `needs` and the job's goal name, and the parts and hinted steps of a job that
/// GHA+ hints split.
#[derive(Default)]
struct CompiledJob {
    jobs: Vec<(String, Spec<SourceSpec>)>,
    parts: Vec<(String, Spec<SourceSpec>)>,
    calls: Vec<Call>,
    titles: BTreeMap<String, String>,
}

impl CompiledJob {
    /// Titles the actions after the first `jobs` and `parts` with the values of their matrix combination, as GitHub
    /// names a matrix job `test (3.12, fast)`.
    fn title(&mut self, (jobs, parts): (usize, usize), values: &str) {
        let labels = self.jobs[jobs..].iter().chain(&self.parts[parts..]).map(|(label, _)| label);
        let titles: Vec<(String, String)> =
            labels.map(|label| (label.clone(), format!("{label} ({values})"))).collect();
        self.titles.extend(titles);
    }
}

/// Where the steps of a job, and a hinted step, see the workspace: the runner's `GITHUB_WORKSPACE`, in which a run
/// step starts and `actions/checkout` puts the repository.
fn job_workspace_view() -> WorkspaceView {
    WorkspaceView { root: Layout::sandbox().workspace().display().to_string(), start: String::new() }
}

/// The images for `runs-on` labels.
pub type Images = BTreeMap<String, String>;

pub(crate) struct Compiler<'a> {
    pub(crate) package_path: &'a str,
    pub(crate) platform: &'a str,
    pub(crate) images: &'a Images,
    pub(crate) key_order: &'a KeyOrder,
    pub(crate) github: Map<String, Value>,
    pub(crate) findings: Vec<Finding>,
}

/// What compile-time evaluation can read: the matrix, `github`, and empty `vars`, `inputs` and `secrets`.
struct StaticContext<'a> {
    contexts: &'a BTreeMap<String, Value>,
}

impl Context for StaticContext<'_> {
    fn context(&self, name: &str) -> Option<Value> {
        self.contexts.get(name).cloned()
    }

    fn job_status(&self) -> JobStatus {
        JobStatus::Success
    }

    fn hash_files(&self, _patterns: &[String]) -> Result<String, String> {
        Err("hashFiles() cannot be used before a job runs".to_owned())
    }
}

impl Compiler<'_> {
    fn find(&mut self, severity: Severity, location: impl Into<String>, message: impl Into<String>) {
        self.findings.push(Finding { severity, location: location.into(), message: message.into() });
    }

    pub(crate) fn compile(&mut self, workflow: &Value) -> Package {
        let mut package = Package::default();
        let Some(document) = workflow.as_object() else {
            self.find(Severity::Unsupported, "workflow", "a workflow must be a mapping");
            return package;
        };
        let name = document.get("name").map(to_text).unwrap_or_else(|| self.package_path.to_owned());
        self.note_workflow_keys(document);
        let workflow_env = text_map(document.get("env"));
        let workflow_defaults = run_defaults(document.get("defaults"));
        let Some(jobs) = document.get("jobs").and_then(Value::as_object) else {
            self.find(Severity::Unsupported, "workflow", "the workflow has no jobs");
            return package;
        };
        let mut labels_of_job: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let order = job_order(jobs);
        for job_id in order {
            let job = &jobs[&job_id];
            let compiled = self.compile_job(&name, &job_id, job, &workflow_env, &workflow_defaults, &labels_of_job);
            let labels: Vec<String> = compiled.jobs.iter().map(|(label, _)| label.clone()).collect();
            for (label, spec) in compiled.jobs.into_iter().chain(compiled.parts) {
                package.workspace_views.insert(label.clone(), job_workspace_view());
                package.actions.insert(label, spec);
            }
            package.calls.extend(compiled.calls);
            package.titles.extend(compiled.titles);
            package.goals.insert(label(self.package_path, &job_id), labels.iter().cloned().collect());
            labels_of_job.insert(job_id, labels);
        }
        let all: BTreeSet<String> = package.actions.keys().cloned().collect();
        if !all.is_empty() {
            package.goals.insert(label(self.package_path, ALL_GOAL), all);
        }
        package
    }

    fn note_workflow_keys(&mut self, document: &Map<String, Value>) {
        if document.contains_key("on") {
            self.find(Severity::Different, "workflow", "triggers (on:) are not read; Azad runs the jobs as for a push");
        }
        for key in ["concurrency", "permissions"] {
            if document.contains_key(key) {
                self.find(Severity::Ignored, "workflow", format!("{key}: is not used"));
            }
        }
    }

    fn compile_job(
        &mut self,
        workflow: &str,
        job_id: &str,
        job: &Value,
        workflow_env: &BTreeMap<String, String>,
        workflow_defaults: &RunDefaults,
        labels_of_job: &BTreeMap<String, Vec<String>>,
    ) -> CompiledJob {
        let location = format!("job {job_id}");
        let Some(job) = job.as_object() else {
            self.find(Severity::Unsupported, location, "a job must be a mapping");
            return CompiledJob::default();
        };
        if job.contains_key("uses") {
            self.find(Severity::Unsupported, &location, "reusable workflows (jobs with uses:) are not supported yet");
            return CompiledJob::default();
        }
        for key in ["container", "services"] {
            if job.contains_key(key) {
                self.find(Severity::Unsupported, &location, format!("{key}: needs Docker, which jobs do not have yet"));
            }
        }
        for key in ["concurrency", "permissions", "environment", "timeout-minutes"] {
            if job.contains_key(key) {
                self.find(Severity::Ignored, &location, format!("{key}: is not used"));
            }
        }
        let condition = job.get("if").map(to_text);
        if condition.as_deref().is_some_and(runs_after_failure) {
            self.find(
                Severity::Unsupported,
                &location,
                "a job condition with always(), failure() or cancelled() runs after a failure, which Azad does not do yet",
            );
        }
        let needs = string_list(job.get("needs"));
        for need in needs.iter().filter(|n| !labels_of_job.contains_key(*n)) {
            self.find(Severity::Unsupported, &location, format!("needs: {need} is not a job before this one"));
        }
        let combinations = self.combinations(&location, job_id, job);
        let total = combinations.len();
        let steps = self.steps(&location, job.get("steps"));
        let job_env = text_map(job.get("env"));
        let defaults = merge_defaults(workflow_defaults, &run_defaults(job.get("defaults")));
        let outputs = text_map(job.get("outputs"));
        let actions = remote_actions(&steps);
        let checks_out = steps.iter().any(|s| uses_any(s, &[CHECKOUT]) || uses_local(s));
        let hinted: Vec<usize> = steps.iter().enumerate().filter(|(_, s)| s.hint.is_some()).map(|(i, _)| i).collect();
        self.note_after_hints(&location, &steps, &hinted);
        let mut compiled = CompiledJob::default();
        let many = combinations.len() > 1;
        for (index, combination) in combinations.into_iter().enumerate() {
            let name = match many {
                true => format!("{job_id}.{index}"),
                false => job_id.to_owned(),
            };
            let mut contexts = BTreeMap::new();
            let mut github = self.github.clone();
            github.insert("job".to_owned(), Value::String(job_id.to_owned()));
            contexts.insert("github".to_owned(), Value::Object(github));
            contexts.insert("matrix".to_owned(), Value::Object(combination.to_map()));
            contexts.insert("strategy".to_owned(), serde_json::json!({"job-index": index, "job-total": total}));
            for empty in ["vars", "inputs", "secrets"] {
                contexts.insert(empty.to_owned(), Value::Object(Map::new()));
            }
            let Some(image) = self.image(&location, job.get("runs-on"), &contexts) else { continue };
            let mut plan = JobPlan {
                version: PLAN_VERSION,
                workflow: workflow.to_owned(),
                job: job_id.to_owned(),
                condition: condition.clone(),
                needs: needs.clone(),
                contexts,
                workflow_env: workflow_env.clone(),
                job_env: job_env.clone(),
                defaults: defaults.clone(),
                steps: steps.clone(),
                outputs: outputs.clone(),
                actions: actions.iter().map(|(uses, (dir, _))| (uses.clone(), dir.clone())).collect(),
                part: None,
            };
            let (calls, waits) = self.module_calls(&mut plan, &name, &image);
            compiled.calls.extend(calls);
            let job_spec = |plan: &JobPlan| {
                let mut spec = self.job_spec(plan, image.clone(), checks_out, &actions, &needs, labels_of_job);
                spec.inputs.extend(waits.clone());
                spec
            };
            let before = (compiled.jobs.len(), compiled.parts.len());
            match hinted.is_empty() {
                true => compiled.jobs.push((label(self.package_path, &name), job_spec(&plan))),
                false => self.split(&plan, &name, &image, &hinted, job_spec, &mut compiled),
            }
            let values = combination.title();
            if !values.is_empty() {
                compiled.title(before, &values);
            }
        }
        compiled
    }

    /// The chain of a job that hinted steps split (decision 0013): part 0, hinted step, part 1, ..., and the last part
    /// with the job's name.
    fn split(
        &self,
        plan: &JobPlan,
        name: &str,
        image: &str,
        hinted: &[usize],
        job_spec: impl Fn(&JobPlan) -> Spec<SourceSpec>,
        compiled: &mut CompiledJob,
    ) {
        let part_label = |j: usize| label(self.package_path, &format!("{name}.part{j}"));
        let step_label = |index: usize| {
            let step = match &plan.steps[index].id {
                Some(id) => format!("{name}.{id}"),
                None => format!("{name}.step{}", index + 1),
            };
            label(self.package_path, &step)
        };
        for (j, prepares) in hinted.iter().map(Some).chain([None]).enumerate() {
            let first = match j {
                0 => 0,
                _ => hinted[j - 1] + 1,
            };
            let end = prepares.copied().unwrap_or(plan.steps.len());
            let part = Part { first, end, resumes: j > 0, prepares: prepares.copied() };
            let mut spec = job_spec(&JobPlan { part: Some(part), ..plan.clone() });
            if j > 0 {
                let state = SourceSpec::Output { action: part_label(j - 1), path: paths::STATE_OUTPUT.to_owned() };
                let result = SourceSpec::Output {
                    action: step_label(hinted[j - 1]),
                    path: paths::STEP_RESULT_OUTPUT.to_owned(),
                };
                spec.inputs.insert(paths::STATE_DIR.to_owned(), state);
                spec.inputs.insert(paths::HINTED_DIR.to_owned(), result);
            }
            let Some(&index) = prepares else {
                compiled.jobs.push((label(self.package_path, name), spec));
                continue;
            };
            spec.outputs = [paths::STATE_OUTPUT, paths::HINT_OUTPUT, paths::TOOLS_OUTPUT].map(str::to_owned).into();
            let kind = plan.steps[index].hint.as_ref().expect("a hinted step has a hint").kind;
            compiled.parts.push((step_label(index), self.hinted_spec(image, kind, &part_label(j))));
            compiled.parts.push((part_label(j), spec));
        }
    }

    /// The calls of the module steps of a job (decision 0014), and the inputs by which the job waits for them. Each
    /// module step in the plan gets its call's label, so the runner does not run the action.
    fn module_calls(&self, plan: &mut JobPlan, name: &str, image: &str) -> (Vec<Call>, BTreeMap<String, SourceSpec>) {
        let mut calls = Vec::new();
        let mut waits = BTreeMap::new();
        for (index, step) in plan.steps.iter_mut().enumerate() {
            let StepKind::Uses { action, with } = &step.kind else { continue };
            let Some(module) = recognized_module(action) else { continue };
            let call_name = match &step.id {
                Some(id) => format!("{name}.{id}"),
                None => format!("{name}.step{}", index + 1),
            };
            let mut args: BTreeMap<String, Value> =
                with.iter().map(|(key, value)| (key.replace('-', "_"), Value::String(value.clone()))).collect();
            if module.takes_image {
                args.entry(IMAGE_PARAM.to_owned()).or_insert_with(|| Value::String(image.to_owned()));
            }
            let call_label = label(self.package_path, &call_name);
            calls.push(Call {
                package: self.package_path.to_owned(),
                // The workspace of a job is the repository's root.
                dir: String::new(),
                name: call_name,
                file: module.file.to_owned(),
                symbol: module.symbol.to_owned(),
                args,
            });
            let output = SourceSpec::Output { action: call_label.clone(), path: module.output.to_owned() };
            waits.insert(format!("{}/{}", paths::MODULES_DIR, index + 1), output);
            step.module = Some(call_label);
        }
        (calls, waits)
    }

    /// A hinted step: the runner in step mode, with only the hint and the tool cache that the part before wrote.
    fn hinted_spec(&self, image: &str, kind: HintKind, part: &str) -> Spec<SourceSpec> {
        let output = |path: &str| SourceSpec::Output { action: part.to_owned(), path: path.to_owned() };
        let inputs = BTreeMap::from([
            (paths::RUNNER_DIR.to_owned(), SourceSpec::Tool { name: paths::RUNNER_BINARY.to_owned() }),
            (paths::HINT_DIR.to_owned(), output(paths::HINT_OUTPUT)),
            (paths::TOOL_CACHE_DIR.to_owned(), output(paths::TOOLS_OUTPUT)),
        ]);
        let kind = match kind {
            HintKind::Hermetic => Kind::Hermetic,
            HintKind::Fetch => Kind::Fetch,
        };
        Spec {
            kind,
            platform: self.platform.to_owned(),
            root: SourceSpec::Image { reference: image.to_owned(), env: BTreeMap::new() },
            command: vec![format!("{}/{}", paths::RUNNER_DIR, paths::RUNNER_BINARY), STEP_FLAG.to_owned()],
            env: BTreeMap::new(),
            inputs,
            outputs: [paths::STEP_RESULT_OUTPUT.to_owned()].into(),
            caches: BTreeMap::new(),
            work: BTreeMap::new(),
            dir: String::new(),
            secrets: Default::default(),
        }
    }

    /// Steps after a hinted step that run after a failure do not run when the hinted step fails: its action fails, and
    /// the parts after it are skipped.
    fn note_after_hints(&mut self, location: &str, steps: &[Step], hinted: &[usize]) {
        let Some(&first) = hinted.first() else { return };
        for (index, step) in steps.iter().enumerate().skip(first + 1) {
            if step.condition.as_deref().is_some_and(runs_after_failure) {
                self.find(
                    Severity::Different,
                    format!("{location}, step {}", index + 1),
                    "this step does not run when a hinted step before it fails",
                );
            }
        }
    }

    /// A GHA+ hint in a step's `env:` (decision 0013). The variables are taken out of the env, so that they do not
    /// change what the step runs. A hint that Azad cannot use is reported, and the step runs in its job as before.
    fn hint(&mut self, location: &str, env: &mut BTreeMap<String, String>, step: &Map<String, Value>) -> Option<Hint> {
        let kind = env.remove(HINT_KIND_ENV);
        let inputs = env.remove(HINT_INPUTS_ENV);
        let outputs = env.remove(HINT_OUTPUTS_ENV);
        if kind.is_none() && inputs.is_none() && outputs.is_none() {
            return None;
        }
        let script = step.get("run").map(to_text).unwrap_or_default();
        let hidden = HIDDEN_FROM_HINTED.iter().find(|name| reads_variable(&script, name));
        let problem = match (&kind, step.get("run")) {
            (None, _) => Some(format!("{HINT_INPUTS_ENV} and {HINT_OUTPUTS_ENV} need {HINT_KIND_ENV}")),
            (Some(_), None) => Some("hints are only for run: steps".to_owned()),
            _ if let Some(name) = hidden => Some(format!(
                "the script reads ${name}, which a hinted step does not see; ${{{{ github.* }}}} in the script is \
                 resolved before and works, but the step then runs again when its value changes"
            )),
            _ if step.contains_key("continue-on-error") => {
                Some("a hinted step cannot continue on error yet".to_owned())
            }
            _ if [&kind, &inputs, &outputs].into_iter().flatten().any(|v| v.contains("${{")) => {
                Some("hints are fixed text, not expressions".to_owned())
            }
            _ => None,
        };
        let kind = match kind.as_deref() {
            Some(HINT_KIND_HERMETIC) => Some(HintKind::Hermetic),
            Some(HINT_KIND_FETCH) => Some(HintKind::Fetch),
            _ => None,
        };
        let (Some(kind), None) = (kind, &problem) else {
            let message =
                problem.unwrap_or_else(|| format!("{HINT_KIND_ENV} is {HINT_KIND_HERMETIC} or {HINT_KIND_FETCH}"));
            self.find(Severity::Different, location, format!("{message}; the step runs without its hint"));
            return None;
        };
        self.find(
            Severity::Different,
            location,
            "a hinted step runs as its own action, with only the files of AZAD_INPUTS and the tool cache; it does not \
             see GITHUB_SHA, GITHUB_REF or the run's ids",
        );
        let patterns = |text: Option<String>| text.unwrap_or_default().split_whitespace().map(str::to_owned).collect();
        Some(Hint { kind, inputs: patterns(inputs), outputs: patterns(outputs) })
    }

    fn combinations(&mut self, location: &str, job_id: &str, job: &Map<String, Value>) -> Vec<matrix::Combination> {
        let Some(strategy) = job.get("strategy").and_then(Value::as_object) else {
            return vec![matrix::Combination::default()];
        };
        for key in ["fail-fast", "max-parallel"] {
            if strategy.contains_key(key) {
                self.find(Severity::Ignored, location, format!("strategy.{key} is not used"));
            }
        }
        let Some(matrix_value) = strategy.get("matrix") else {
            return vec![matrix::Combination::default()];
        };
        let Some(matrix_map) = matrix_value.as_object() else {
            self.find(Severity::Unsupported, location, "a matrix that an expression makes is not supported yet");
            return Vec::new();
        };
        let has_expression = matrix_map.values().any(|v| v.as_str().is_some_and(|s| s.contains("${{")));
        if has_expression {
            self.find(Severity::Unsupported, location, "a matrix that an expression makes is not supported yet");
            return Vec::new();
        }
        let path = [JOBS_KEY, job_id, STRATEGY_KEY, MATRIX_KEY].map(str::to_owned);
        match matrix::expand(matrix_map, self.key_order, &path) {
            Ok(combinations) if combinations.is_empty() => vec![matrix::Combination::default()],
            Ok(combinations) => combinations,
            Err(message) => {
                self.find(Severity::Unsupported, location, message);
                Vec::new()
            }
        }
    }

    fn image(&mut self, location: &str, runs_on: Option<&Value>, contexts: &BTreeMap<String, Value>) -> Option<String> {
        let value = match runs_on {
            Some(Value::String(text)) => match evaluate_template(text, &StaticContext { contexts }) {
                Ok(value) => value,
                Err(error) => {
                    self.find(Severity::Unsupported, location, format!("runs-on: {error}"));
                    return None;
                }
            },
            Some(other) => other.clone(),
            None => {
                self.find(Severity::Unsupported, location, "the job has no runs-on");
                return None;
            }
        };
        let label = match &value {
            Value::Array(labels) => labels.first().map(to_text).unwrap_or_default(),
            other => to_text(other),
        };
        let image = self.images.get(&label).cloned();
        if image.is_none() {
            let known: Vec<&str> = self.images.keys().map(String::as_str).collect();
            self.find(
                Severity::Unsupported,
                location,
                format!("runs-on: {label} has no image; known labels: {}", known.join(", ")),
            );
        }
        image
    }

    fn steps(&mut self, location: &str, steps: Option<&Value>) -> Vec<Step> {
        let Some(Value::Array(items)) = steps else {
            self.find(Severity::Unsupported, location, "the job has no steps");
            return Vec::new();
        };
        let mut compiled = Vec::new();
        for (index, item) in items.iter().enumerate() {
            let step_location = format!("{location}, step {}", index + 1);
            let Some(step) = item.as_object() else {
                self.find(Severity::Unsupported, step_location, "a step must be a mapping");
                continue;
            };
            if let Some(step) = self.step(&step_location, step) {
                compiled.push(step);
            }
        }
        compiled
    }

    fn step(&mut self, location: &str, step: &Map<String, Value>) -> Option<Step> {
        let shell = step.get("shell").map(to_text);
        if shell.as_deref().is_some_and(|s| UNSUPPORTED_SHELLS.contains(&s)) {
            self.find(Severity::Unsupported, location, "PowerShell steps are not supported");
        }
        let kind = match (step.get("run"), step.get("uses")) {
            (Some(run), None) => StepKind::Run { script: to_text(run) },
            (None, Some(uses)) => {
                let action = to_text(uses);
                self.note_action(location, &action);
                StepKind::Uses { action, with: text_map(step.get("with")) }
            }
            _ => {
                self.find(Severity::Unsupported, location, "a step needs exactly one of run: and uses:");
                return None;
            }
        };
        self.note_secrets(location, step);
        self.note_module_step(location, &kind, step);
        let mut env = text_map(step.get("env"));
        let hint = self.hint(location, &mut env, step);
        Some(Step {
            id: step.get("id").map(to_text),
            name: step.get("name").map(to_text),
            condition: step.get("if").map(to_text),
            env,
            continue_on_error: step.get("continue-on-error").map(to_text),
            working_directory: step.get("working-directory").map(to_text),
            shell,
            kind,
            hint,
            module: None,
        })
    }

    fn note_action(&mut self, location: &str, action: &str) {
        let name = action_name(action);
        if recognized_module(action).is_some() {
            self.find(
                Severity::Different,
                location,
                format!("{name} is an Azad module: its actions run outside the job, and the job waits for them"),
            );
        } else if action.starts_with(DOCKER_PREFIX) {
            self.find(Severity::Unsupported, location, "docker:// actions need Docker, which jobs do not have yet");
        } else if CACHE_ACTIONS.contains(&name) {
            self.find(Severity::Different, location, format!("{name} does nothing; Azad's own cache applies"));
        } else if name == CHECKOUT {
            self.find(
                Severity::Different,
                location,
                "actions/checkout copies the working tree, with uncommitted changes",
            );
        } else if !action.starts_with(LOCAL_PREFIX) && !action.contains('@') {
            self.find(Severity::Unsupported, location, format!("{action} has no @ref"));
        }
    }

    /// A module call is expanded before anything runs, so its arguments must be known then, and it runs whatever the
    /// step's condition says.
    fn note_module_step(&mut self, location: &str, kind: &StepKind, step: &Map<String, Value>) {
        let StepKind::Uses { action, with } = kind else { return };
        if recognized_module(action).is_none() {
            return;
        }
        if step.contains_key("if") {
            self.find(Severity::Unsupported, location, "a module step cannot have if: yet; its actions always run");
        }
        if with.values().any(|value| value.contains("${{")) {
            self.find(Severity::Unsupported, location, "the with: inputs of a module are fixed text, not expressions");
        }
    }

    fn note_secrets(&mut self, location: &str, step: &Map<String, Value>) {
        let texts = [step.get("run"), step.get("with"), step.get("env")].into_iter().flatten().map(Value::to_string);
        let reads_secrets = texts.into_iter().any(|text| {
            template_expressions(&text)
                .is_ok_and(|expressions| expressions.iter().any(|e| references(e).contexts.contains("secrets")))
        });
        if reads_secrets {
            self.find(Severity::Different, location, "secrets are empty in Azad, as on a pull request from a fork");
        }
    }

    fn job_spec(
        &self,
        plan: &JobPlan,
        image: String,
        checks_out: bool,
        actions: &BTreeMap<String, (String, SourceSpec)>,
        needs: &[String],
        labels_of_job: &BTreeMap<String, Vec<String>>,
    ) -> Spec<SourceSpec> {
        let plan_json = serde_json::to_vec_pretty(plan).expect("a job plan always serializes");
        let mut inputs = BTreeMap::new();
        inputs.insert(paths::RUNNER_DIR.to_owned(), SourceSpec::Tool { name: paths::RUNNER_BINARY.to_owned() });
        let files =
            BTreeMap::from([(paths::PLAN_FILE.to_owned(), InlineFile { content: plan_json, executable: false })]);
        inputs.insert(paths::PLAN_DIR.to_owned(), SourceSpec::Inline { files });
        if checks_out {
            inputs.insert(paths::SOURCE_DIR.to_owned(), SourceSpec::GitFiles { dir: String::new() });
        }
        for (dir, source) in actions.values() {
            inputs.insert(dir.clone(), source.clone());
        }
        for need in needs {
            let Some(labels) = labels_of_job.get(need) else { continue };
            for (index, need_label) in labels.iter().enumerate() {
                let dir = match index {
                    0 => format!("{}/{need}", paths::NEEDS_DIR),
                    n => format!("{}/{need}.{n}", paths::NEEDS_DIR),
                };
                for output in [paths::META_OUTPUT, paths::ARTIFACTS_OUTPUT] {
                    let source = SourceSpec::Output { action: need_label.clone(), path: output.to_owned() };
                    inputs.insert(format!("{dir}/{output}"), source);
                }
            }
        }
        Spec {
            kind: Kind::Impure,
            platform: self.platform.to_owned(),
            root: SourceSpec::Image { reference: image, env: BTreeMap::new() },
            command: vec![
                format!("{}/{}", paths::RUNNER_DIR, paths::RUNNER_BINARY),
                format!("{}/{}", paths::PLAN_DIR, paths::PLAN_FILE),
            ],
            env: BTreeMap::new(),
            inputs,
            outputs: [paths::META_OUTPUT, paths::ARTIFACTS_OUTPUT].into_iter().map(str::to_owned).collect(),
            caches: BTreeMap::new(),
            work: BTreeMap::new(),
            dir: String::new(),
            secrets: Default::default(),
        }
    }
}

/// Jobs in an order in which each job comes after the jobs it needs; a job in a cycle comes last.
fn job_order(jobs: &Map<String, Value>) -> Vec<String> {
    let mut remaining: BTreeMap<String, BTreeSet<String>> = jobs
        .iter()
        .map(|(id, job)| {
            (id.clone(), string_list(job.get("needs")).into_iter().filter(|n| jobs.contains_key(n)).collect())
        })
        .collect();
    let mut order = Vec::new();
    while !remaining.is_empty() {
        let ready: Vec<String> =
            remaining.iter().filter(|(_, needs)| needs.is_empty()).map(|(id, _)| id.clone()).collect();
        if ready.is_empty() {
            order.extend(remaining.into_keys());
            break;
        }
        for id in &ready {
            remaining.remove(id);
        }
        for needs in remaining.values_mut() {
            needs.retain(|n| !ready.contains(n));
        }
        order.extend(ready);
    }
    order
}

/// The remote actions of the steps: each `uses:` value, its directory in the sandbox, and its repository.
fn remote_actions(steps: &[Step]) -> BTreeMap<String, (String, SourceSpec)> {
    let mut actions = BTreeMap::new();
    for step in steps {
        let StepKind::Uses { action, .. } = &step.kind else { continue };
        let skipped = action.starts_with(LOCAL_PREFIX) || action.starts_with(DOCKER_PREFIX) || is_built_in(action);
        if skipped || recognized_module(action).is_some() {
            continue;
        }
        let Some((path, rev)) = action.split_once('@') else { continue };
        let mut parts = path.splitn(3, '/');
        let (Some(owner), Some(repo)) = (parts.next(), parts.next()) else { continue };
        let dir = format!("{}/{owner}/{repo}@{rev}", paths::ACTIONS_DIR);
        let source = SourceSpec::Git { url: format!("{GITHUB_URL}/{owner}/{repo}"), rev: rev.to_owned() };
        actions.insert(action.clone(), (dir, source));
    }
    actions
}

/// Whether a script names a variable as `$NAME` or `${NAME`.
fn reads_variable(script: &str, name: &str) -> bool {
    let plain = format!("${name}");
    let braced = format!("${{{name}");
    script.match_indices(&plain).any(|(at, _)| {
        let next = script[at + plain.len()..].chars().next();
        !next.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
    }) || script.contains(&braced)
}

/// Whether a job condition can make the job run after a dependency failed.
fn runs_after_failure(condition: &str) -> bool {
    let compact = condition.replace(' ', "");
    STATUS_FUNCTIONS.iter().any(|function| compact.contains(function))
}

fn action_name(action: &str) -> &str {
    action.split_once('@').map_or(action, |(name, _)| name)
}

fn is_built_in(action: &str) -> bool {
    let name = action_name(action);
    [CHECKOUT, UPLOAD_ARTIFACT, DOWNLOAD_ARTIFACT].contains(&name) || CACHE_ACTIONS.contains(&name)
}

fn uses_any(step: &Step, names: &[&str]) -> bool {
    matches!(&step.kind, StepKind::Uses { action, .. } if names.contains(&action_name(action)))
}

fn uses_local(step: &Step) -> bool {
    matches!(&step.kind, StepKind::Uses { action, .. } if action.starts_with(LOCAL_PREFIX))
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => items.iter().map(to_text).collect(),
        Some(Value::Null) | None => Vec::new(),
        Some(other) => vec![to_text(other)],
    }
}

fn text_map(value: Option<&Value>) -> BTreeMap<String, String> {
    value
        .and_then(Value::as_object)
        .map(|map| map.iter().map(|(k, v)| (k.clone(), to_text(v))).collect())
        .unwrap_or_default()
}

fn run_defaults(value: Option<&Value>) -> RunDefaults {
    let run = value.and_then(|d| d.get("run"));
    RunDefaults {
        shell: run.and_then(|r| r.get("shell")).map(to_text),
        working_directory: run.and_then(|r| r.get("working-directory")).map(to_text),
    }
}

fn merge_defaults(workflow: &RunDefaults, job: &RunDefaults) -> RunDefaults {
    RunDefaults {
        shell: job.shell.clone().or_else(|| workflow.shell.clone()),
        working_directory: job.working_directory.clone().or_else(|| workflow.working_directory.clone()),
    }
}
