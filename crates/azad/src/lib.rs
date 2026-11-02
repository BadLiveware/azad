//! Azad as a library: a CI system where work is a function of its inputs.
//!
//! The `azad` command (`azad-cli`) is a thin layer over [`Engine`]; other programs can use the engine the same way.
//!
//! - [`ir`]: digests, the Action IR (intermediate representation), its canonical encoding and text form.
//! - [`store`]: the content-addressed store and the state files (action cache, aliases, run records).
//! - [`scheduler`]: the rules that derive what must run, `decide`, the run loop, and the [`scheduler::Backend`] trait.
//! - [`frontend`]: the frontend contract, and the Starlark, GHA and text frontends.
//! - [`sandbox`] and [`local`]: the sandbox and the local backend that runs actions on this machine.
//! - [`executor`]: the executor as a service behind the plan, and its client.
//! - [`orchestrator`]: webhooks in, runs at the event's trust level, and check runs out.
//! - [`history`]: the history of the workspace's repository, from the local clone or another source.
//! - [`oci`]: image pulls.
//! - [`timings`]: how long the steps of a run took that are not commands.
//! - [`trust`]: trust levels, which keep the state of runs apart.
//! - [`engine`]: the parts composed: load a target, store its sources, run, plan and explain.

pub mod engine;
pub mod executor;
pub mod frontend;
pub mod history;
pub mod ir;
pub mod local;
pub mod log;
pub mod oci;
pub mod orchestrator;
pub mod remote;
pub mod sandbox;
pub mod scheduler;
pub mod store;
pub mod timings;
pub mod trust;

pub use engine::{
    Engine, EngineConfig, EngineError, Event, Explained, Explanation, Finding, Goal, Outcome, Progress, RunReport,
    SandboxKind, Severity, Status, TEXT_EXTENSION, TagFilter, default_tools, host_platform,
};
pub use trust::Trust;
