//! The job plan: what the GHA (GitHub Actions) frontend gives the runner for one job. The frontend writes it as
//! `job.json`; the runner reads it inside the sandbox and runs the steps. See decision 0010.
//!
//! Expressions stay as text (`${{ ... }}`); the runner evaluates them when it runs the job, because many of them read
//! what earlier steps did.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The version of this schema; the runner refuses another.
pub const PLAN_VERSION: u32 = 2;

/// Where the runner and its inputs are in the sandbox.
pub mod paths {
    /// The directory that holds the runner binary.
    pub const RUNNER_DIR: &str = "/__azad/runner";
    /// The name of the runner binary.
    pub const RUNNER_BINARY: &str = "azad-gha-runner";
    /// The directory that holds `job.json`.
    pub const PLAN_DIR: &str = "/__azad/job";
    pub const PLAN_FILE: &str = "job.json";
    /// The repository source, for `actions/checkout`.
    pub const SOURCE_DIR: &str = "/__azad/src";
    /// `<NEEDS_DIR>/<job>/meta` and `<NEEDS_DIR>/<job>/artifacts` of each job in `needs`.
    pub const NEEDS_DIR: &str = "/__azad/needs";
    /// The directories of the actions in `JobPlan::actions`.
    pub const ACTIONS_DIR: &str = "/__azad/actions";
    /// `<MODULES_DIR>/<step number>`: the output of a module call that the job waits for.
    pub const MODULES_DIR: &str = "/__azad/modules";
    /// The job's writable scratch space: the workspace, home, temp and tool cache are below it.
    pub const GITHUB_DIR: &str = "/tmp/github";
    /// The output directory (below the work directory) with `result.json`.
    pub const META_OUTPUT: &str = "meta";
    /// The output directory (below the work directory) with one directory per uploaded artifact.
    pub const ARTIFACTS_OUTPUT: &str = "artifacts";
    /// The file in the meta output.
    pub const RESULT_FILE: &str = "result.json";

    // GHA+ (decision 0013): the actions of a job that hinted steps split.
    /// The `state` output of the part before, in a part that resumes the job.
    pub const STATE_DIR: &str = "/__azad/state";
    /// The `result` output of the hinted step before, in a part that resumes the job.
    pub const HINTED_DIR: &str = "/__azad/hinted";
    /// The `hint` output of the part before, in a hinted step.
    pub const HINT_DIR: &str = "/__azad/hint";
    /// Where a hinted step finds the tool cache: where the job had it.
    pub const TOOL_CACHE_DIR: &str = "/tmp/github/toolcache";
    /// The outputs of a part that a hinted step follows: the job's state, the hinted step, and the tool cache.
    pub const STATE_OUTPUT: &str = "state";
    pub const HINT_OUTPUT: &str = "hint";
    pub const TOOLS_OUTPUT: &str = "tools";
    /// The output of a hinted step.
    pub const STEP_RESULT_OUTPUT: &str = "result";
    /// In the hint output: the resolved step, and the files that its `AZAD_INPUTS` name.
    pub const HINTED_STEP_FILE: &str = "step.json";
    pub const FILES_DIR: &str = "files";
    /// In the result output: what the step gave the job; the files that its `AZAD_OUTPUTS` name are in `FILES_DIR`.
    pub const STEP_RESULT_FILE: &str = "result.json";
}

/// The part of a job that one action runs, when GHA+ hints split the job (decision 0013).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Part {
    /// The steps `first..end` run in this part.
    pub first: usize,
    pub end: usize,
    /// The job's state comes from the part before, and the result of the hinted step `first - 1`.
    pub resumes: bool,
    /// The hinted step that comes after this part; this part saves the job's state and prepares that step. The last
    /// part has none: it finishes the job.
    pub prepares: Option<usize>,
}

/// What a GHA+ step says about itself in its `env:`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hint {
    pub kind: HintKind,
    /// Patterns of the files below the workspace that the step reads.
    pub inputs: Vec<String>,
    /// Patterns of the files below the workspace that the step writes for the steps after it.
    pub outputs: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HintKind {
    /// No network.
    Hermetic,
    /// Network, and cached by its inputs as a fetch is.
    Fetch,
}

/// A hinted step as the part before it resolved it: what the step's action runs, and nothing that changes per run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HintedStep {
    pub workflow: String,
    pub job: String,
    pub name: String,
    /// The step's condition was false; the action does nothing.
    pub skip: bool,
    pub script: String,
    pub shell: Option<String>,
    /// Relative to the workspace.
    pub working_directory: Option<String>,
    /// The `env` context of the step.
    pub env: BTreeMap<String, String>,
    /// Directories that steps before added to `PATH`.
    pub added_paths: Vec<String>,
    /// The step's `AZAD_OUTPUTS`.
    pub outputs: Vec<String>,
}

/// What a hinted step gives the part after it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HintedResult {
    pub skipped: bool,
    pub outputs: BTreeMap<String, String>,
    /// What the step wrote to `GITHUB_ENV` and `GITHUB_PATH`.
    pub env: BTreeMap<String, String>,
    pub added_paths: Vec<String>,
}

/// One job, or one combination of a job's matrix.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JobPlan {
    pub version: u32,
    /// The name of the workflow.
    pub workflow: String,
    /// The job's key in `jobs:`.
    pub job: String,
    /// The job-level `if:`, as written.
    pub condition: Option<String>,
    /// The jobs in `needs:`; their results are in `NEEDS_DIR`.
    pub needs: Vec<String>,
    /// Contexts that are known before the job runs: `github`, `matrix`, `strategy`, `inputs`, `vars`, `secrets`.
    pub contexts: BTreeMap<String, Value>,
    /// The workflow's `env:`, then the job's `env:`, each as written (values can hold expressions).
    pub workflow_env: BTreeMap<String, String>,
    pub job_env: BTreeMap<String, String>,
    pub defaults: RunDefaults,
    pub steps: Vec<Step>,
    /// The job's `outputs:`, as written.
    pub outputs: BTreeMap<String, String>,
    /// Each `uses:` value of a remote action, and the directory in the sandbox that holds that action's repository.
    pub actions: BTreeMap<String, String>,
    /// The part of the job that this action runs; none for the whole job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part: Option<Part>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunDefaults {
    pub shell: Option<String>,
    pub working_directory: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Step {
    pub id: Option<String>,
    pub name: Option<String>,
    pub condition: Option<String>,
    pub env: BTreeMap<String, String>,
    /// `true`, `false`, or an expression.
    pub continue_on_error: Option<String>,
    pub working_directory: Option<String>,
    pub shell: Option<String>,
    pub kind: StepKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<Hint>,
    /// A `uses:` of a module (decision 0014): the label of the call that the engine expanded. Its actions ran before
    /// the job, which waits for them; the step does nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    Run {
        script: String,
    },
    /// `uses:` with its `with:` inputs, as written.
    Uses {
        action: String,
        with: BTreeMap<String, String>,
    },
}

/// What the runner writes to `META_OUTPUT/RESULT_FILE` for jobs that need this one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobResult {
    /// `success`, `failure` or `skipped`, as in the `needs` context.
    pub result: String,
    pub outputs: BTreeMap<String, String>,
}

pub const RESULT_SUCCESS: &str = "success";
pub const RESULT_FAILURE: &str = "failure";
pub const RESULT_SKIPPED: &str = "skipped";
