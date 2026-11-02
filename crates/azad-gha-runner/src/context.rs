use std::collections::BTreeMap;
use std::path::Path;

use crate::expr::{Context, JobStatus, Value};
use serde_json::{Map, json};

use crate::files;

pub(crate) const ENV: &str = "env";
pub(crate) const STEPS: &str = "steps";
pub(crate) const INPUTS: &str = "inputs";
pub(crate) const JOB: &str = "job";
pub(crate) const GITHUB: &str = "github";
const ACTION_PATH: &str = "action_path";
const STATUS_SUCCESS: &str = "success";
const STATUS_FAILURE: &str = "failure";
const STATUS_CANCELLED: &str = "cancelled";

/// What an expression in a step can read: the contexts known before the job, and the state of the run so far.
#[derive(Debug)]
pub(crate) struct Expressions<'a> {
    pub(crate) contexts: &'a BTreeMap<String, Value>,
    pub(crate) env: &'a BTreeMap<String, String>,
    pub(crate) steps: &'a Map<String, Value>,
    pub(crate) inputs: Option<&'a Map<String, Value>>,
    pub(crate) action_path: Option<&'a Path>,
    pub(crate) status: JobStatus,
    pub(crate) workspace: &'a Path,
}

impl Context for Expressions<'_> {
    fn context(&self, name: &str) -> Option<Value> {
        match name {
            ENV => Some(text_map_value(self.env)),
            STEPS => Some(Value::Object(self.steps.clone())),
            INPUTS => {
                self.inputs.map(|inputs| Value::Object(inputs.clone())).or_else(|| self.contexts.get(INPUTS).cloned())
            }
            JOB => Some(json!({ "status": status_text(self.status) })),
            GITHUB => self.github(),
            other => self.contexts.get(other).cloned(),
        }
    }

    fn job_status(&self) -> JobStatus {
        self.status
    }

    fn hash_files(&self, patterns: &[String]) -> Result<String, String> {
        files::hash_files(self.workspace, patterns)
    }
}

impl Expressions<'_> {
    fn github(&self) -> Option<Value> {
        let mut github = self.contexts.get(GITHUB).cloned()?;
        if let (Some(path), Value::Object(fields)) = (self.action_path, &mut github) {
            fields.insert(ACTION_PATH.to_string(), Value::String(path.to_string_lossy().into_owned()));
        }
        Some(github)
    }
}

pub(crate) fn text_map_value(map: &BTreeMap<String, String>) -> Value {
    Value::Object(map.iter().map(|(key, value)| (key.clone(), Value::String(value.clone()))).collect())
}

pub(crate) fn status_text(status: JobStatus) -> &'static str {
    match status {
        JobStatus::Success => STATUS_SUCCESS,
        JobStatus::Failure => STATUS_FAILURE,
        JobStatus::Cancelled => STATUS_CANCELLED,
    }
}
