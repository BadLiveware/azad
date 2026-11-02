//! The contract between the run loop and whatever runs actions: the local sandbox today, a fleet of hosts later.

use crate::log::LogSink;
use std::error::Error;

use crate::ir::{Action, ActionResult};

pub type BackendError = Box<dyn Error + Send + Sync>;

/// The result of an action that a backend ran.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendExecution {
    pub result: ActionResult,
    /// Paths that the action left in its work directory without declaring them as outputs.
    pub discarded: Vec<String>,
}

pub trait Backend: Send + Sync {
    /// The known result of an action, without running it. An action that must not be cached has none.
    fn cached(&self, action: &Action) -> Result<Option<ActionResult>, BackendError>;
    /// Runs an action without a cache lookup, gives each line of its output to `logs` while it runs, and caches a
    /// successful result when the action may be cached.
    fn execute(&self, action: &Action, logs: &dyn LogSink) -> Result<BackendExecution, BackendError>;
}
