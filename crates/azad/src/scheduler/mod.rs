//! The core of Azad: the rules that derive what must run, the life of one execution, and the run loop.

pub mod backend;
pub mod changes;
pub mod decide;
mod record;
pub mod rules;
pub mod runner;

pub use backend::{Backend, BackendError, BackendExecution};
pub use changes::{Cause, Changes, Part, ValueChange};
pub use runner::{Explanation, Outcome, Progress, RunError, Runner, Status};
