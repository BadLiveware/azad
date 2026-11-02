//! The Azad engine as a library: it chooses a frontend for a target, stores the sources, checks the graph, and runs,
//! plans and explains goals through a backend. The `azad` command is a thin layer over it, and other programs (a
//! server, an editor plugin) can use it the same way.

mod api;
mod config;
mod error;
mod git;
mod resolve;
mod tasks;
mod why;

pub use crate::frontend::text::{TEXT_EXTENSION, TextFrontend};
pub use crate::frontend::{Finding, Severity};
pub use crate::scheduler::{Explanation, Outcome, Progress, Status};
pub use api::{Engine, Event, Explained, Goal, RunReport};
pub use config::{EngineConfig, GHA_RUNNER, GUEST, SandboxKind, TagFilter, default_tools, host_platform};
pub use error::EngineError;
pub use tasks::Mode;
