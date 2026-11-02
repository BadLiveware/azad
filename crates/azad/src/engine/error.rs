use crate::frontend::FrontendError;
use crate::ir::{CanonicalError, VerifyError};
use crate::oci::PullError;
use crate::scheduler::{BackendError, RunError};
use crate::store::StoreError;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("{0}")]
    Config(String),
    #[error("{0}")]
    Git(String),
    #[error("tool {name} is not found; {hint}")]
    MissingTool { name: String, hint: String },
    #[error(
        "no frontend reads {0:?}; a target is a label such as //services/orders:check, a workflow such as ci.yml or ci.yml:test, or a file such as pipeline.azad:check"
    )]
    NoFrontend(String),
    #[error("{error}")]
    Frontend { frontend: &'static str, error: FrontendError },
    #[error("the graph is not valid:\n  {}", .0.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n  "))]
    Invalid(Vec<VerifyError>),
    #[error("there is no goal {goal}; known goals: {}", .known.join(", "))]
    UnknownGoal { goal: String, known: Vec<String> },
    #[error("no package at or below //{dir} defines {name}")]
    NoPackageDefines { dir: String, name: String },
    #[error("{target} selects nothing to run: {excluded} excluded by their tags ({origin})")]
    AllExcluded { target: String, excluded: String, origin: String },
    #[error("there is no action {0}")]
    UnknownAction(String),
    #[error("{0} was not part of that run; explain the next run instead")]
    NotInRun(String),
    #[error("cannot store {what}: {error}")]
    Store { what: String, error: StoreError },
    #[error("cannot pull {reference}: {error}")]
    Pull { reference: String, error: PullError },
    #[error(transparent)]
    Run(#[from] RunError),
    #[error("the backend failed: {0}")]
    Backend(BackendError),
    #[error("a run record cannot be read: {0}")]
    Record(CanonicalError),
}

impl From<CanonicalError> for EngineError {
    fn from(error: CanonicalError) -> Self {
        EngineError::Record(error)
    }
}

impl EngineError {
    pub(crate) fn store(what: impl Into<String>) -> impl FnOnce(StoreError) -> EngineError {
        let what = what.into();
        move |source| EngineError::Store { what, error: source }
    }
}
