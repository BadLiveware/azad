//! The run of one job: its condition, its steps in order, the post parts of its actions, and its result.

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use crate::expr::{JobStatus, Value, condition, interpolate};
use crate::plan::{
    JobPlan, JobResult, PLAN_VERSION, Part, RESULT_FAILURE, RESULT_SKIPPED, RESULT_SUCCESS, RunDefaults, Step, StepKind,
};
use serde_json::{Map, json};

use crate::commands::{Captured, Masker, process_line};
use crate::context::{self, Expressions};
use crate::error::{Error, WithContext};
use crate::file_commands::{parse_key_values, parse_paths};
use crate::files::find_program;
use crate::layout::Layout;
use crate::shell::resolve_shell;

pub(crate) const DEFAULT_CONDITION: &str = "success()";
pub(crate) const PATH: &str = "PATH";
pub(crate) const BASH_PROGRAM: &str = "bash";
pub(crate) const OUTCOME_SUCCESS: &str = "success";
const OUTCOME_FAILURE: &str = "failure";
pub(crate) const OUTCOME_SKIPPED: &str = "skipped";
const TRUE_TEXT: &str = "true";
pub(crate) const FILE_COMMANDS_DIR: &str = "_runner_file_commands";
pub(crate) const WORKFLOW_DIR: &str = "_github_workflow";
const EVENT_FILE: &str = "event.json";
pub(crate) const SCRIPTS_DIR: &str = "_scripts";
const RUN_PREFIX: &str = "Run ";
pub(crate) const EXIT_SUCCESS: u8 = 0;
pub(crate) const EXIT_FAILURE: u8 = 1;
/// A process that a signal stopped exits with this plus the signal's number, as shells report it.
const SIGNAL_EXIT_BASE: i32 = 128;

const MATRIX: &str = "matrix";
const STRATEGY: &str = "strategy";
const VARS: &str = "vars";
const SECRETS: &str = "secrets";
const NEEDS: &str = "needs";
const RUNNER: &str = "runner";
const GITHUB_TOKEN_SECRET: &str = "GITHUB_TOKEN";

const RUNNER_OS: &str = "Linux";
const RUNNER_ARCH: &str = "X64";
const RUNNER_NAME: &str = "azad";

/// Keys of the `github` context that are also `GITHUB_<KEY>` variables.
const GITHUB_VARIABLE_KEYS: [&str; 16] = [
    "repository",
    "repository_owner",
    "sha",
    "ref",
    "ref_name",
    "ref_type",
    "head_ref",
    "base_ref",
    "event_name",
    "actor",
    "run_id",
    "run_number",
    "run_attempt",
    "server_url",
    "api_url",
    "graphql_url",
];

/// The end state of a job.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobOutcome {
    Success,
    /// The exit code is the code of the first step that failed the job, or 1 when the runner itself failed it.
    Failure {
        exit_code: u8,
    },
    Skipped,
}

impl JobOutcome {
    pub fn result(self) -> &'static str {
        match self {
            Self::Success => RESULT_SUCCESS,
            Self::Failure { .. } => RESULT_FAILURE,
            Self::Skipped => RESULT_SKIPPED,
        }
    }

    pub fn exit_code(self) -> u8 {
        match self {
            Self::Success | Self::Skipped => EXIT_SUCCESS,
            Self::Failure { exit_code } => exit_code,
        }
    }

    pub(crate) fn of(scope: &Scope) -> Self {
        match scope.failure {
            Some(exit_code) => Self::Failure { exit_code },
            None => Self::Success,
        }
    }

    pub(crate) fn runner_failure() -> Self {
        Self::Failure { exit_code: EXIT_FAILURE }
    }
}

/// How a run of the runner ends: a whole job, or a part that the next action continues (decision 0013).
enum Finish {
    Job(JobOutcome, BTreeMap<String, String>),
    Part,
}

/// Runs the job of the plan in the layout and writes its result. A problem of the runner itself fails the job.
pub fn run(layout: &Layout) -> JobOutcome {
    let result = Job::start(layout).and_then(Job::run);
    let (outcome, outputs) = match result {
        Ok(Finish::Job(outcome, outputs)) => (outcome, outputs),
        Ok(Finish::Part) => {
            println!("==> Part done; the job goes on in the next action");
            return JobOutcome::Success;
        }
        Err(error) => {
            println!("::error::{error}");
            (JobOutcome::runner_failure(), BTreeMap::new())
        }
    };
    let job_result = JobResult { result: outcome.result().to_string(), outputs };
    if let Err(error) = write_result(layout, &job_result) {
        println!("::error::{error}");
        return JobOutcome::runner_failure();
    }
    println!("==> Job {}", outcome.result());
    outcome
}

fn write_result(layout: &Layout, result: &JobResult) -> Result<(), Error> {
    let file = layout.result_file();
    if let Some(directory) = file.parent() {
        fs::create_dir_all(directory).with_context(|| format!("cannot create {}", directory.display()))?;
    }
    let text = serde_json::to_string_pretty(result).with_context(|| "cannot encode the job result".to_string())?;
    fs::write(&file, text).with_context(|| format!("cannot write {}", file.display()))
}

pub fn read_plan(file: &Path) -> Result<JobPlan, Error> {
    let text = fs::read_to_string(file).with_context(|| format!("cannot read the job plan {}", file.display()))?;
    let plan: JobPlan =
        serde_json::from_str(&text).with_context(|| format!("invalid job plan in {}", file.display()))?;
    if plan.version != PLAN_VERSION {
        return Err(Error::new(format!(
            "the job plan has version {}, but this runner reads version {PLAN_VERSION}",
            plan.version
        )));
    }
    Ok(plan)
}

/// The results of the jobs in `needs`, and the status that `success()` reads at job level.
fn read_needs(layout: &Layout, needs: &[String]) -> Result<(Value, JobStatus), Error> {
    let mut context = Map::new();
    let mut status = JobStatus::Success;
    for job in needs {
        let file = layout.need_result(job);
        let text = fs::read_to_string(&file).with_context(|| format!("cannot read the result of `{job}`"))?;
        let result: JobResult =
            serde_json::from_str(&text).with_context(|| format!("invalid result of `{job}` in {}", file.display()))?;
        if result.result != RESULT_SUCCESS {
            status = JobStatus::Failure;
        }
        context.insert(job.clone(), json!({ "result": result.result, "outputs": result.outputs }));
    }
    Ok((Value::Object(context), status))
}

/// What one step, or one part of an action, did.
#[derive(Clone, Debug)]
pub(crate) struct StepRun {
    pub outputs: BTreeMap<String, String>,
    pub state: BTreeMap<String, String>,
    /// The exit code of the step's process; 1 when the runner failed the step.
    pub exit_code: u8,
}

impl StepRun {
    pub(crate) fn success(outputs: BTreeMap<String, String>) -> Self {
        Self { outputs, state: BTreeMap::new(), exit_code: EXIT_SUCCESS }
    }

    fn failed() -> Self {
        Self { outputs: BTreeMap::new(), state: BTreeMap::new(), exit_code: EXIT_FAILURE }
    }

    pub(crate) fn succeeded(&self) -> bool {
        self.exit_code == EXIT_SUCCESS
    }
}

/// The steps of the job, or of one composite action.
#[derive(Debug)]
pub(crate) struct Scope {
    pub steps: Map<String, Value>,
    /// The status when the scope started; a composite action can run after a failed step.
    pub inherited_status: JobStatus,
    /// The exit code of the first step of this scope that failed, when its `continue-on-error` did not hold.
    pub failure: Option<u8>,
    pub inputs: Option<Map<String, Value>>,
    pub action_path: Option<PathBuf>,
    /// The `env:` of the step that called the composite action, which its steps inherit.
    pub inherited_env: BTreeMap<String, String>,
    pub defaults: RunDefaults,
    /// Composite actions must name the shell of each `run:` step.
    pub requires_shell: bool,
}

impl Scope {
    pub(crate) fn job(defaults: RunDefaults) -> Self {
        Self {
            steps: Map::new(),
            inherited_status: JobStatus::Success,
            failure: None,
            inputs: None,
            action_path: None,
            inherited_env: BTreeMap::new(),
            defaults,
            requires_shell: false,
        }
    }

    pub(crate) fn composite(
        parent: &Scope,
        action_path: PathBuf,
        inputs: Map<String, Value>,
        env: BTreeMap<String, String>,
    ) -> Self {
        Self {
            steps: Map::new(),
            inherited_status: parent.status(),
            failure: None,
            inputs: Some(inputs),
            action_path: Some(action_path),
            inherited_env: env,
            defaults: RunDefaults::default(),
            requires_shell: true,
        }
    }

    pub(crate) fn status(&self) -> JobStatus {
        match self.failure {
            Some(_) => JobStatus::Failure,
            None => self.inherited_status,
        }
    }

    /// Fails the scope; the first failure gives the exit code.
    pub(crate) fn fail(&mut self, exit_code: u8) {
        self.failure.get_or_insert(exit_code);
    }
}

/// The files that a step writes its outputs, environment, path, summary and state to.
pub(crate) struct StepFiles {
    output: PathBuf,
    env: PathBuf,
    path: PathBuf,
    summary: PathBuf,
    state: PathBuf,
}

impl StepFiles {
    fn variables(&self) -> [(&'static str, &Path); 5] {
        [
            ("GITHUB_OUTPUT", &self.output),
            ("GITHUB_ENV", &self.env),
            ("GITHUB_PATH", &self.path),
            ("GITHUB_STEP_SUMMARY", &self.summary),
            ("GITHUB_STATE", &self.state),
        ]
    }
}

/// A post part of a Node action, which runs after all steps.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct PostAction {
    pub name: String,
    pub script: PathBuf,
    pub condition: String,
    pub action_path: PathBuf,
    pub env: BTreeMap<String, String>,
    pub process_env: BTreeMap<String, String>,
}

/// The state of a running job.
#[derive(Debug)]
pub(crate) struct Job<'a> {
    pub layout: &'a Layout,
    pub plan: JobPlan,
    pub contexts: BTreeMap<String, Value>,
    pub(crate) needs_status: JobStatus,
    pub(crate) runner_env: BTreeMap<String, String>,
    /// The `env` context: the workflow and job `env:`, then what steps wrote to `GITHUB_ENV`.
    pub(crate) env: BTreeMap<String, String>,
    /// Directories from `GITHUB_PATH`, in the order they were added; the last one is first in `PATH`.
    pub(crate) added_paths: Vec<String>,
    pub(crate) masker: Masker,
    pub posts: Vec<PostAction>,
    pub(crate) file_counter: usize,
    pub(crate) workspace_dir: PathBuf,
    /// The job's condition was false: no step runs.
    pub(crate) skipped: bool,
}

impl<'a> Job<'a> {
    fn start(layout: &'a Layout) -> Result<Self, Error> {
        let plan = read_plan(&layout.plan_file)?;
        let (needs, needs_status) = read_needs(layout, &plan.needs)?;
        let contexts = base_contexts(layout, &plan, needs);
        let mut job = Job::from_plan(layout, plan);
        if let Some(Value::Object(secrets)) = contexts.get(SECRETS) {
            secrets.values().filter_map(Value::as_str).for_each(|secret| job.masker.add(secret));
        }
        job.contexts = contexts;
        job.needs_status = needs_status;
        Ok(job)
    }

    /// A job of a plan, with no contexts yet.
    pub(crate) fn from_plan(layout: &'a Layout, plan: JobPlan) -> Self {
        Self {
            layout,
            plan,
            contexts: BTreeMap::new(),
            needs_status: JobStatus::Success,
            runner_env: runner_environment(),
            env: BTreeMap::new(),
            added_paths: Vec::new(),
            masker: Masker::default(),
            posts: Vec::new(),
            file_counter: 0,
            workspace_dir: layout.workspace(),
            skipped: false,
        }
    }

    fn run(mut self) -> Result<Finish, Error> {
        let whole = Part { first: 0, end: self.plan.steps.len(), resumes: false, prepares: None };
        let part = self.plan.part.clone().unwrap_or(whole);
        let mut scope = match part.resumes {
            true => self.resume(&part)?,
            false => self.begin()?,
        };
        if !self.skipped {
            let steps = self.plan.steps[part.first..part.end].to_vec();
            for step in &steps {
                self.run_step(step, &mut scope);
            }
        }
        if let Some(next) = part.prepares {
            self.save(&scope)?;
            self.prepare_hint(next, &scope)?;
            return Ok(Finish::Part);
        }
        if self.skipped {
            return Ok(Finish::Job(JobOutcome::Skipped, BTreeMap::new()));
        }
        self.run_posts(&mut scope);
        let outputs = self.job_outputs(&mut scope);
        Ok(Finish::Job(JobOutcome::of(&scope), outputs))
    }

    /// Starts the job: its condition, its directories and its `env:`. A job whose condition is false is skipped.
    fn begin(&mut self) -> Result<Scope, Error> {
        let mut scope = Scope::job(RunDefaults::default());
        scope.inherited_status = self.needs_status;
        let job_condition = self.plan.condition.clone().unwrap_or_else(|| DEFAULT_CONDITION.to_string());
        let should_run = condition(&job_condition, &self.expressions(&BTreeMap::new(), &scope))
            .with_context(|| format!("cannot evaluate the job condition `{job_condition}`"))?;
        if !should_run {
            println!("The job condition `{job_condition}` is false");
            self.skipped = true;
            return Ok(scope);
        }
        self.prepare_directories()?;
        let scope = Scope::job(self.plan.defaults.clone());
        self.env = self.job_env(&scope)?;
        Ok(scope)
    }

    pub(crate) fn prepare_directories(&self) -> Result<(), Error> {
        let directories = [
            self.layout.workspace(),
            self.layout.home(),
            self.layout.temp(),
            self.layout.tool_cache(),
            self.layout.meta_dir(),
            self.layout.artifacts_dir(),
            self.layout.temp().join(FILE_COMMANDS_DIR),
            self.layout.temp().join(SCRIPTS_DIR),
            self.layout.temp().join(WORKFLOW_DIR),
        ];
        for directory in directories {
            fs::create_dir_all(&directory).with_context(|| format!("cannot create {}", directory.display()))?;
        }
        let event = self.contexts.get(context::GITHUB).and_then(|github| github.get("event"));
        let event_text = serde_json::to_string(event.unwrap_or(&json!({})))
            .with_context(|| "cannot encode `github.event`".to_string())?;
        fs::write(self.event_file(), event_text).with_context(|| "cannot write the event file".to_string())
    }

    pub(crate) fn event_file(&self) -> PathBuf {
        self.layout.temp().join(WORKFLOW_DIR).join(EVENT_FILE)
    }

    /// The workflow `env:`, then the job `env:`; each layer can read the `env` context of the one before.
    fn job_env(&self, scope: &Scope) -> Result<BTreeMap<String, String>, Error> {
        let mut env = BTreeMap::new();
        for layer in [&self.plan.workflow_env, &self.plan.job_env] {
            let values = self.interpolate_map(layer, &env, scope)?;
            env.extend(values);
        }
        Ok(env)
    }

    pub(crate) fn interpolate_map(
        &self,
        templates: &BTreeMap<String, String>,
        env: &BTreeMap<String, String>,
        scope: &Scope,
    ) -> Result<BTreeMap<String, String>, Error> {
        let expressions = self.expressions(env, scope);
        templates
            .iter()
            .map(|(name, template)| {
                let value = interpolate(template, &expressions).with_context(|| format!("cannot evaluate `{name}`"))?;
                Ok((name.clone(), value))
            })
            .collect()
    }

    pub(crate) fn expressions<'s>(&'s self, env: &'s BTreeMap<String, String>, scope: &'s Scope) -> Expressions<'s> {
        Expressions {
            contexts: &self.contexts,
            env,
            steps: &scope.steps,
            inputs: scope.inputs.as_ref(),
            action_path: scope.action_path.as_deref(),
            status: scope.status(),
            workspace: self.workspace(),
        }
    }

    pub(crate) fn workspace(&self) -> &Path {
        &self.workspace_dir
    }

    /// The `env` context of a step: the job's, then the inherited and step layers.
    pub(crate) fn env_context(&self, layer: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        let mut env = self.env.clone();
        env.extend(layer.iter().map(|(name, value)| (name.clone(), value.clone())));
        env
    }

    pub(crate) fn run_step(&mut self, step: &Step, scope: &mut Scope) {
        println!("==> {}", self.display_name(step, scope));
        let attempt = self.attempt(step, scope).unwrap_or_else(|error| {
            self.report_error(&error);
            Some(StepRun::failed())
        });
        let Some(run) = attempt else {
            println!("Skipped: the condition is false");
            record_step(scope, step, &BTreeMap::new(), OUTCOME_SKIPPED, OUTCOME_SKIPPED);
            return;
        };
        let continues = !run.succeeded() && self.continue_on_error(step, scope);
        let outcome = outcome_text(run.succeeded());
        let conclusion = outcome_text(run.succeeded() || continues);
        if continues {
            println!("The step failed, and continue-on-error holds: the job goes on");
        }
        if !run.succeeded() && !continues {
            scope.fail(run.exit_code);
        }
        record_step(scope, step, &run.outputs, outcome, conclusion);
    }

    pub(crate) fn condition_holds(&self, step: &Step, scope: &Scope) -> Result<bool, Error> {
        let step_condition = step.condition.as_deref().unwrap_or(DEFAULT_CONDITION);
        let env = self.env_context(&scope.inherited_env);
        condition(step_condition, &self.expressions(&env, scope))
            .with_context(|| format!("cannot evaluate the condition `{step_condition}`"))
    }

    fn attempt(&mut self, step: &Step, scope: &mut Scope) -> Result<Option<StepRun>, Error> {
        if !self.condition_holds(step, scope)? {
            return Ok(None);
        }
        if let Some(call) = &step.module {
            println!("Ran before the job as the actions of the module call {call}");
            return Ok(Some(StepRun::success(BTreeMap::new())));
        }
        let env = self.env_context(&scope.inherited_env);
        let mut layer = scope.inherited_env.clone();
        layer.extend(self.interpolate_map(&step.env, &env, scope)?);
        let run = match &step.kind {
            StepKind::Run { script } => self.run_script(step, script, scope, &layer)?,
            StepKind::Uses { action, with } => self.run_uses(step, action, with, scope, &layer)?,
        };
        Ok(Some(run))
    }

    fn continue_on_error(&self, step: &Step, scope: &Scope) -> bool {
        let Some(template) = &step.continue_on_error else {
            return false;
        };
        let env = self.env_context(&scope.inherited_env);
        match crate::expr::evaluate_template(template, &self.expressions(&env, scope)) {
            Ok(Value::String(text)) => text.trim().eq_ignore_ascii_case(TRUE_TEXT),
            Ok(value) => crate::expr::truthy(&value),
            Err(error) => {
                self.report_error(&Error::new(format!("cannot evaluate `continue-on-error`: {error}")));
                false
            }
        }
    }

    pub(crate) fn display_name(&self, step: &Step, scope: &Scope) -> String {
        let env = self.env_context(&scope.inherited_env);
        if let Some(name) = &step.name {
            return interpolate(name, &self.expressions(&env, scope)).unwrap_or_else(|_| name.clone());
        }
        match &step.kind {
            StepKind::Run { script } => format!("{RUN_PREFIX}{}", script.lines().next().unwrap_or_default()),
            StepKind::Uses { action, .. } => format!("{RUN_PREFIX}{action}"),
        }
    }

    pub(crate) fn report_error(&self, error: &Error) {
        println!("::error::{}", self.masker.mask(&error.to_string()));
    }

    fn run_script(
        &mut self,
        step: &Step,
        template: &str,
        scope: &Scope,
        layer: &BTreeMap<String, String>,
    ) -> Result<StepRun, Error> {
        let shell = match (&step.shell, scope.requires_shell) {
            (Some(shell), _) => Some(shell.as_str()),
            (None, true) => return Err(Error::new("a `run:` step in a composite action must name its `shell:`")),
            (None, false) => scope.defaults.shell.as_deref(),
        };
        let env = self.env_context(layer);
        let script = interpolate(template, &self.expressions(&env, scope))?;
        let directory = self.working_directory(step, scope, &env)?;
        let files = self.new_step_files()?;
        let process_env = self.process_env(&env, &files, scope.action_path.as_deref());
        let path_value = process_env.get(PATH).cloned().unwrap_or_default();
        let shell = resolve_shell(shell, find_program(BASH_PROGRAM, &path_value).is_some())?;
        let script_file = self.script_file(shell.extension);
        fs::write(&script_file, script).with_context(|| format!("cannot write {}", script_file.display()))?;
        let (program, arguments) = shell.command(&script_file);
        let run = self.run_process(&program, &arguments, &directory, &process_env)?;
        self.finish_step(run, &files)
    }

    fn working_directory(&self, step: &Step, scope: &Scope, env: &BTreeMap<String, String>) -> Result<PathBuf, Error> {
        let Some(template) = step.working_directory.as_ref().or(scope.defaults.working_directory.as_ref()) else {
            return Ok(self.workspace().to_path_buf());
        };
        let relative = interpolate(template, &self.expressions(env, scope))?;
        let directory = self.workspace().join(relative);
        match directory.is_dir() {
            true => Ok(directory),
            false => Err(Error::new(format!("the working directory {} does not exist", directory.display()))),
        }
    }

    pub(crate) fn script_file(&mut self, extension: Option<&str>) -> PathBuf {
        self.file_counter += 1;
        let name = match extension {
            Some(extension) => format!("step-{}.{extension}", self.file_counter),
            None => format!("step-{}", self.file_counter),
        };
        self.layout.temp().join(SCRIPTS_DIR).join(name)
    }

    pub(crate) fn new_step_files(&mut self) -> Result<StepFiles, Error> {
        self.file_counter += 1;
        let directory = self.layout.temp().join(FILE_COMMANDS_DIR);
        let file = |kind: &str| directory.join(format!("{kind}_{}", self.file_counter));
        let files = StepFiles {
            output: file("set_output"),
            env: file("set_env"),
            path: file("add_path"),
            summary: file("step_summary"),
            state: file("save_state"),
        };
        for (_, path) in files.variables() {
            fs::write(path, "").with_context(|| format!("cannot create {}", path.display()))?;
        }
        Ok(files)
    }

    /// The environment of a step's process.
    pub(crate) fn process_env(
        &self,
        env: &BTreeMap<String, String>,
        files: &StepFiles,
        action_path: Option<&Path>,
    ) -> BTreeMap<String, String> {
        let mut process_env = self.runner_env.clone();
        process_env.extend(self.runner_variables());
        process_env.extend(env.iter().map(|(name, value)| (name.clone(), value.clone())));
        for (name, path) in files.variables() {
            process_env.insert(name.to_string(), path_text(path));
        }
        if let Some(path) = action_path {
            process_env.insert("GITHUB_ACTION_PATH".to_string(), path_text(path));
        }
        let added = self.added_paths.iter().rev().map(String::as_str);
        let existing = process_env.get(PATH).map(String::as_str).filter(|path| !path.is_empty());
        let path = added.chain(existing).collect::<Vec<_>>().join(":");
        process_env.insert(PATH.to_string(), path);
        process_env
    }

    fn runner_variables(&self) -> BTreeMap<String, String> {
        let mut variables = BTreeMap::from([
            ("CI".to_string(), TRUE_TEXT.to_string()),
            ("GITHUB_ACTIONS".to_string(), TRUE_TEXT.to_string()),
            ("GITHUB_WORKSPACE".to_string(), path_text(self.workspace())),
            ("GITHUB_EVENT_PATH".to_string(), path_text(&self.event_file())),
            ("GITHUB_JOB".to_string(), self.plan.job.clone()),
            ("GITHUB_WORKFLOW".to_string(), self.plan.workflow.clone()),
            ("RUNNER_OS".to_string(), RUNNER_OS.to_string()),
            ("RUNNER_ARCH".to_string(), RUNNER_ARCH.to_string()),
            ("RUNNER_NAME".to_string(), RUNNER_NAME.to_string()),
            ("RUNNER_TEMP".to_string(), path_text(&self.layout.temp())),
            ("RUNNER_TOOL_CACHE".to_string(), path_text(&self.layout.tool_cache())),
            ("HOME".to_string(), path_text(&self.layout.home())),
        ]);
        let github = self.contexts.get(context::GITHUB);
        for key in GITHUB_VARIABLE_KEYS {
            if let Some(value) = github.and_then(|github| github.get(key)) {
                variables.insert(format!("GITHUB_{}", key.to_uppercase()), crate::expr::to_text(value));
            }
        }
        variables
    }

    /// Runs a program; passes its standard output on line by line, and acts on the workflow commands in it.
    pub(crate) fn run_process(
        &mut self,
        program: &str,
        arguments: &[String],
        directory: &Path,
        env: &BTreeMap<String, String>,
    ) -> Result<StepRun, Error> {
        let path_value = env.get(PATH).map(String::as_str).unwrap_or_default();
        let Some(executable) = find_program(program, path_value) else {
            return Err(Error::new(format!("cannot find `{program}` on PATH ({path_value})")));
        };
        let mut child = Command::new(&executable)
            .args(arguments)
            .current_dir(directory)
            .env_clear()
            .envs(env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("cannot start {}", executable.display()))?;
        let mut captured = Captured::default();
        if let Some(stdout) = child.stdout.take() {
            self.pass_on(BufReader::new(stdout), &mut captured)?;
        }
        let status = child.wait()?;
        if !status.success() {
            let reason = match status.code() {
                Some(code) => format!("Process completed with exit code {code}."),
                None => format!("Process stopped: {status}."),
            };
            println!("::error::{reason}");
        }
        Ok(StepRun { outputs: captured.outputs, state: captured.state, exit_code: exit_code(status) })
    }

    fn pass_on(&mut self, reader: impl BufRead, captured: &mut Captured) -> Result<(), Error> {
        for line in reader.split(b'\n') {
            let bytes = line.with_context(|| "cannot read the standard output of the step".to_string())?;
            let text = String::from_utf8_lossy(&bytes);
            let text = text.strip_suffix('\r').unwrap_or(&text);
            if let Some(shown) = process_line(text, &mut self.masker, captured) {
                println!("{shown}");
            }
        }
        Ok(())
    }

    /// Reads the files that a step wrote: outputs and state go to the step's run; env and path go to the job.
    pub(crate) fn finish_step(&mut self, mut run: StepRun, files: &StepFiles) -> Result<StepRun, Error> {
        let read = |path: &Path| fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()));
        run.outputs
            .extend(parse_key_values(&read(&files.output)?).with_context(|| "invalid GITHUB_OUTPUT".to_string())?);
        run.state.extend(parse_key_values(&read(&files.state)?).with_context(|| "invalid GITHUB_STATE".to_string())?);
        self.env.extend(parse_key_values(&read(&files.env)?).with_context(|| "invalid GITHUB_ENV".to_string())?);
        for path in parse_paths(&read(&files.path)?) {
            self.added_paths.retain(|added| added != &path);
            self.added_paths.push(path);
        }
        Ok(run)
    }

    fn run_posts(&mut self, scope: &mut Scope) {
        let posts = std::mem::take(&mut self.posts);
        for post in posts.iter().rev() {
            println!("==> Post {}", post.name);
            match self.run_post(post, scope) {
                Ok(Some(run)) if !run.succeeded() => scope.fail(run.exit_code),
                Ok(Some(_)) => {}
                Ok(None) => println!("Skipped: the condition is false"),
                Err(error) => {
                    self.report_error(&error);
                    scope.fail(EXIT_FAILURE);
                }
            }
        }
    }

    fn run_post(&mut self, post: &PostAction, scope: &Scope) -> Result<Option<StepRun>, Error> {
        let env = self.env_context(&post.env);
        let should_run = condition(&post.condition, &self.expressions(&env, scope))
            .with_context(|| format!("cannot evaluate `post-if: {}`", post.condition))?;
        if !should_run {
            return Ok(None);
        }
        let run = self.run_node_script(&post.script, &post.action_path, &env, &post.process_env)?;
        Ok(Some(run))
    }

    fn job_outputs(&self, scope: &mut Scope) -> BTreeMap<String, String> {
        let env = self.env_context(&BTreeMap::new());
        match self.interpolate_map(&self.plan.outputs, &env, scope) {
            Ok(outputs) => outputs,
            Err(error) => {
                self.report_error(&error.with_prefix("cannot evaluate the job outputs"));
                scope.fail(EXIT_FAILURE);
                BTreeMap::new()
            }
        }
    }
}

impl Error {
    fn with_prefix(self, prefix: &str) -> Self {
        Error::new(format!("{prefix}: {self}"))
    }
}

/// The environment of the runner, which steps get; a variable that is not UTF-8 cannot be in a context, so it is left
/// out.
fn runner_environment() -> BTreeMap<String, String> {
    std::env::vars_os()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
        .collect()
}

/// The exit code of a process as a shell reports it: its own code, or 128 plus the signal that stopped it.
fn exit_code(status: ExitStatus) -> u8 {
    let code = status.code().or_else(|| status.signal().map(|signal| SIGNAL_EXIT_BASE + signal));
    code.and_then(|code| u8::try_from(code).ok()).unwrap_or(EXIT_FAILURE)
}

pub(crate) fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn outcome_text(succeeded: bool) -> &'static str {
    match succeeded {
        true => OUTCOME_SUCCESS,
        false => OUTCOME_FAILURE,
    }
}

pub(crate) fn record_step(
    scope: &mut Scope,
    step: &Step,
    outputs: &BTreeMap<String, String>,
    outcome: &str,
    conclusion: &str,
) {
    let Some(id) = &step.id else {
        return;
    };
    let entry = json!({ "outputs": outputs, "outcome": outcome, "conclusion": conclusion });
    scope.steps.insert(id.clone(), entry);
}

/// The contexts known before the steps run, with what the runner adds: the workspace, an empty token, `needs` and
/// `runner`.
fn base_contexts(layout: &Layout, plan: &JobPlan, needs: Value) -> BTreeMap<String, Value> {
    let mut contexts = plan.contexts.clone();
    for name in [MATRIX, STRATEGY, context::INPUTS, VARS, SECRETS, context::GITHUB] {
        contexts.entry(name.to_string()).or_insert_with(|| json!({}));
    }
    if let Some(Value::Object(github)) = contexts.get_mut(context::GITHUB) {
        github.insert("token".to_string(), json!(""));
        github.insert("workspace".to_string(), json!(path_text(&layout.workspace())));
        github.entry("job").or_insert_with(|| json!(plan.job));
        github.entry("workflow").or_insert_with(|| json!(plan.workflow));
    }
    if let Some(Value::Object(secrets)) = contexts.get_mut(SECRETS) {
        secrets.insert(GITHUB_TOKEN_SECRET.to_string(), json!(""));
    }
    contexts.insert(NEEDS.to_string(), needs);
    contexts.insert(
        RUNNER.to_string(),
        json!({
            "os": RUNNER_OS,
            "arch": RUNNER_ARCH,
            "name": RUNNER_NAME,
            "temp": path_text(&layout.temp()),
            "tool_cache": path_text(&layout.tool_cache()),
        }),
    );
    contexts
}
