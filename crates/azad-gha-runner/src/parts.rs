//! GHA+ (decision 0013): a job that hinted steps split into several actions.
//!
//! A part saves the job's state for the part after it, and prepares the hinted step between them: the resolved step,
//! the files it reads, and the tool cache. The hinted step runs alone, in its own action. The part after it starts
//! from the saved state, with what the hinted step gave.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::{Error, WithContext};
use crate::expr::interpolate;
use crate::files::{copy_file, copy_tree, find_files, find_program};
use crate::job::{
    BASH_PROGRAM, EXIT_SUCCESS, FILE_COMMANDS_DIR, Job, JobOutcome, OUTCOME_SKIPPED, OUTCOME_SUCCESS, PostAction,
    SCRIPTS_DIR, Scope, StepRun, WORKFLOW_DIR, record_step,
};
use crate::layout::Layout;
use crate::plan::{HintedResult, HintedStep, JobPlan, PLAN_VERSION, Part, RunDefaults, StepKind, paths};
use crate::shell::resolve_shell;

const RUNNER_STATE_FILE: &str = "runner.json";
const GITHUB_STATE_DIR: &str = "github";
const ARTIFACTS_STATE_DIR: &str = "artifacts";
const PATH: &str = "PATH";

/// What a part keeps of the runner's memory for the part after it.
#[derive(Serialize, Deserialize)]
struct SavedJob {
    steps: Map<String, Value>,
    failure: Option<u8>,
    skipped: bool,
    env: BTreeMap<String, String>,
    added_paths: Vec<String>,
    posts: Vec<PostAction>,
    file_counter: usize,
}

fn read_json<T: DeserializeOwned>(file: &Path) -> Result<T, Error> {
    let text = fs::read_to_string(file).with_context(|| format!("cannot read {}", file.display()))?;
    serde_json::from_str(&text).with_context(|| format!("invalid {}", file.display()))
}

fn write_json(file: &Path, value: &impl Serialize) -> Result<(), Error> {
    let text = serde_json::to_string_pretty(value).with_context(|| format!("cannot encode {}", file.display()))?;
    fs::write(file, text).with_context(|| format!("cannot write {}", file.display()))
}

fn create_dir(dir: &Path) -> Result<(), Error> {
    fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))
}

fn remove_dir(dir: &Path) -> Result<(), Error> {
    match dir.exists() {
        true => fs::remove_dir_all(dir).with_context(|| format!("cannot remove {}", dir.display())),
        false => Ok(()),
    }
}

/// Copies a directory when it exists, else makes the target empty.
fn copy_or_create(source: &Path, target: &Path) -> Result<(), Error> {
    match source.exists() {
        true => copy_tree(source, target),
        false => create_dir(target),
    }
}

impl Job<'_> {
    /// Writes the job's state to the `state` output.
    pub(crate) fn save(&self, scope: &Scope) -> Result<(), Error> {
        let dir = self.layout.work_dir.join(paths::STATE_OUTPUT);
        create_dir(&dir)?;
        let saved = SavedJob {
            steps: scope.steps.clone(),
            failure: scope.failure,
            skipped: self.skipped,
            env: self.env.clone(),
            added_paths: self.added_paths.clone(),
            posts: self.posts.clone(),
            file_counter: self.file_counter,
        };
        write_json(&dir.join(RUNNER_STATE_FILE), &saved)?;
        copy_or_create(&self.layout.github_dir, &dir.join(GITHUB_STATE_DIR))?;
        copy_or_create(&self.layout.artifacts_dir(), &dir.join(ARTIFACTS_STATE_DIR))?;
        // Only the last part gives the job's result and artifacts; the artifacts so far are in the state.
        for output in [self.layout.meta_dir(), self.layout.artifacts_dir()] {
            remove_dir(&output)?;
        }
        Ok(())
    }

    /// Takes up the job where the part before saved it, and applies the result of the hinted step between.
    pub(crate) fn resume(&mut self, part: &Part) -> Result<Scope, Error> {
        let state = &self.layout.state_dir;
        let saved: SavedJob = read_json(&state.join(RUNNER_STATE_FILE))?;
        copy_tree(&state.join(GITHUB_STATE_DIR), &self.layout.github_dir)?;
        copy_tree(&state.join(ARTIFACTS_STATE_DIR), &self.layout.artifacts_dir())?;
        create_dir(&self.layout.meta_dir())?;
        self.env = saved.env;
        self.added_paths = saved.added_paths;
        self.posts = saved.posts;
        self.file_counter = saved.file_counter;
        self.skipped = saved.skipped;
        let mut scope = Scope::job(self.plan.defaults.clone());
        scope.steps = saved.steps;
        scope.failure = saved.failure;
        let hinted = part.first.checked_sub(1).ok_or_else(|| Error::new("a part that resumes follows a step"))?;
        self.apply_hinted(hinted, &mut scope)?;
        Ok(scope)
    }

    fn apply_hinted(&mut self, index: usize, scope: &mut Scope) -> Result<(), Error> {
        let step = self.plan.steps[index].clone();
        println!("==> {} (hinted: ran as its own action)", self.display_name(&step, scope));
        let result: HintedResult = read_json(&self.layout.hinted_dir.join(paths::STEP_RESULT_FILE))?;
        if result.skipped {
            record_step(scope, &step, &BTreeMap::new(), OUTCOME_SKIPPED, OUTCOME_SKIPPED);
            return Ok(());
        }
        let files = self.layout.hinted_dir.join(paths::FILES_DIR);
        if files.exists() {
            copy_tree(&files, &self.workspace_dir)?;
        }
        self.env.extend(result.env);
        for path in result.added_paths {
            self.added_paths.retain(|added| added != &path);
            self.added_paths.push(path);
        }
        record_step(scope, &step, &result.outputs, OUTCOME_SUCCESS, OUTCOME_SUCCESS);
        Ok(())
    }

    /// Writes the `hint` and `tools` outputs for the hinted step `index`: the step resolved now, with nothing that
    /// changes per run, and the files that it reads.
    pub(crate) fn prepare_hint(&self, index: usize, scope: &Scope) -> Result<(), Error> {
        let step = &self.plan.steps[index];
        let (Some(hint), StepKind::Run { script }) = (&step.hint, &step.kind) else {
            return Err(Error::new(format!("step {} is not a hinted run: step", index + 1)));
        };
        let dir = self.layout.work_dir.join(paths::HINT_OUTPUT);
        create_dir(&dir.join(paths::FILES_DIR))?;
        copy_or_create(&self.layout.tool_cache(), &self.layout.work_dir.join(paths::TOOLS_OUTPUT))?;
        let mut hinted = HintedStep {
            workflow: self.plan.workflow.clone(),
            job: self.plan.job.clone(),
            name: self.display_name(step, scope),
            skip: true,
            script: String::new(),
            shell: None,
            working_directory: None,
            env: BTreeMap::new(),
            added_paths: Vec::new(),
            outputs: hint.outputs.clone(),
        };
        if !self.skipped && self.condition_holds(step, scope)? {
            let mut layer = scope.inherited_env.clone();
            layer.extend(self.interpolate_map(&step.env, &self.env_context(&scope.inherited_env), scope)?);
            let env = self.env_context(&layer);
            let expressions = self.expressions(&env, scope);
            hinted.script = interpolate(script, &expressions)?;
            let directory = step.working_directory.as_ref().or(scope.defaults.working_directory.as_ref());
            hinted.working_directory = directory.map(|template| interpolate(template, &expressions)).transpose()?;
            hinted.shell = step.shell.clone().or_else(|| scope.defaults.shell.clone());
            hinted.env = env;
            hinted.added_paths = self.added_paths.clone();
            hinted.skip = false;
            for file in find_files(&self.workspace_dir, &hint.inputs)? {
                let relative = file.strip_prefix(&self.workspace_dir).unwrap_or(&file);
                copy_file(&file, &dir.join(paths::FILES_DIR).join(relative))?;
            }
        }
        write_json(&dir.join(paths::HINTED_STEP_FILE), &hinted)
    }
}

/// Runs a hinted step in its own action: `azad-gha-runner --step`.
pub fn run_hinted(layout: &Layout) -> JobOutcome {
    match hinted(layout) {
        Ok(EXIT_SUCCESS) => JobOutcome::Success,
        Ok(exit_code) => JobOutcome::Failure { exit_code },
        Err(error) => {
            println!("::error::{error}");
            JobOutcome::runner_failure()
        }
    }
}

/// Runs the hinted step, and gives its exit code.
fn hinted(layout: &Layout) -> Result<u8, Error> {
    let step: HintedStep = read_json(&layout.hint_dir.join(paths::HINTED_STEP_FILE))?;
    let result_dir = layout.work_dir.join(paths::STEP_RESULT_OUTPUT);
    create_dir(&result_dir)?;
    println!("==> {}", step.name);
    if step.skip {
        println!("Skipped: the condition is false");
        let skipped = HintedResult { skipped: true, ..HintedResult::default() };
        return write_json(&result_dir.join(paths::STEP_RESULT_FILE), &skipped).map(|()| EXIT_SUCCESS);
    }
    let mut job = Job::for_hinted(layout, &step)?;
    copy_tree(&layout.hint_dir.join(paths::FILES_DIR), &layout.workspace())?;
    let run = job.run_resolved(&step)?;
    if !run.succeeded() {
        return Ok(run.exit_code);
    }
    for file in find_files(&layout.workspace(), &step.outputs)? {
        let relative = file.strip_prefix(layout.workspace()).unwrap_or(&file);
        copy_file(&file, &result_dir.join(paths::FILES_DIR).join(relative))?;
    }
    let env = job.env.iter().filter(|(name, value)| step.env.get(*name) != Some(*value));
    let result = HintedResult {
        skipped: false,
        outputs: run.outputs,
        env: env.map(|(name, value)| (name.clone(), value.clone())).collect(),
        added_paths: job.added_paths.iter().filter(|path| !step.added_paths.contains(path)).cloned().collect(),
    };
    write_json(&result_dir.join(paths::STEP_RESULT_FILE), &result)?;
    Ok(EXIT_SUCCESS)
}

impl<'a> Job<'a> {
    /// A job with only what a hinted step may see: its own env and path, and no context that changes per run.
    fn for_hinted(layout: &'a Layout, step: &HintedStep) -> Result<Self, Error> {
        let plan = JobPlan {
            version: PLAN_VERSION,
            workflow: step.workflow.clone(),
            job: step.job.clone(),
            condition: None,
            needs: Vec::new(),
            contexts: BTreeMap::new(),
            workflow_env: BTreeMap::new(),
            job_env: BTreeMap::new(),
            defaults: RunDefaults::default(),
            steps: Vec::new(),
            outputs: BTreeMap::new(),
            actions: BTreeMap::new(),
            part: None,
        };
        let mut job = Job::from_plan(layout, plan);
        job.env = step.env.clone();
        job.added_paths = step.added_paths.clone();
        let temp = layout.temp();
        for dir in [
            layout.workspace(),
            layout.home(),
            temp.join(FILE_COMMANDS_DIR),
            temp.join(SCRIPTS_DIR),
            temp.join(WORKFLOW_DIR),
        ] {
            create_dir(&dir)?;
        }
        fs::write(job.event_file(), "{}").with_context(|| "cannot write the event file".to_string())?;
        Ok(job)
    }

    fn run_resolved(&mut self, step: &HintedStep) -> Result<StepRun, Error> {
        let directory = match &step.working_directory {
            Some(relative) => self.workspace_dir.join(relative),
            None => self.workspace_dir.clone(),
        };
        if !directory.is_dir() {
            return Err(Error::new(format!("the working directory {} does not exist", directory.display())));
        }
        let files = self.new_step_files()?;
        let process_env = self.process_env(&step.env, &files, None);
        let path_value = process_env.get(PATH).cloned().unwrap_or_default();
        let shell = resolve_shell(step.shell.as_deref(), find_program(BASH_PROGRAM, &path_value).is_some())?;
        let script_file = self.script_file(shell.extension);
        fs::write(&script_file, &step.script).with_context(|| format!("cannot write {}", script_file.display()))?;
        let (program, arguments) = shell.command(&script_file);
        let run = self.run_process(&program, &arguments, &directory, &process_env)?;
        self.finish_step(run, &files)
    }
}
