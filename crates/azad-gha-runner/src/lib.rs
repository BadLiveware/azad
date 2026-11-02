//! The GHA (GitHub Actions) job runner: it plays the part of GitHub's runner inside an Azad sandbox.
//!
//! The GHA frontend compiles each job to one action whose command is this runner, with the job plan as its input.
//! The runner runs the steps in order, evaluates expressions at run time, and writes the job's result for the jobs
//! that need it.

/// The GHA expression language, shared with the GHA frontend.
pub mod expr;
/// The job plan that the GHA frontend writes and this runner reads.
pub mod plan;

mod action;
mod artifacts;
mod commands;
mod context;
mod error;
mod file_commands;
mod files;
mod job;
mod layout;
mod parts;
mod shell;
mod uses;

pub use commands::{WorkflowCommand, workflow_command};
pub use error::Error;
pub use job::{JobOutcome, read_plan, run};
pub use layout::{Layout, WORK_DIR};
pub use parts::run_hinted;
