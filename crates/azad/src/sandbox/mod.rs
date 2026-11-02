//! The sandbox, which enforces what an action claims: what it can read, whether it has network, and how long and how
//! much it may run. See decision 0004.

mod bubblewrap;
mod container;
mod host;
mod process;
mod spawn;
mod vm;

pub use bubblewrap::Bubblewrap;
pub use container::{
    ContainerRun, Dockerfile, ImageContent, RUNTIME_ENV, build_image, export_image, find_runtime, image_content,
};
pub use host::{HostSpec, run_on_host};
pub use vm::{MicroVm, VmConfig};

use std::collections::BTreeMap;

use crate::log::LogSink;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

/// The variable that holds the directories in which a command name is searched.
pub(crate) const PATH_VARIABLE: &str = "PATH";

/// The path at which the work directory appears in the sandbox.
pub const WORK_DIR: &str = "/work";

/// A host directory that the sandbox shows read-only at `target`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mount {
    pub host: PathBuf,
    pub target: String,
}

/// A cache volume that the command may write at `target`. A volume has two forms, and each sandbox uses its own:
/// bubblewrap mounts the directory, and a VM attaches the disk, which it makes when it does not exist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Volume {
    pub target: String,
    pub dir: PathBuf,
    pub disk: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    pub timeout: Duration,
    /// The address space of each process, in bytes.
    pub memory_bytes: Option<u64>,
    /// The number of processes of the user. Linux counts all processes of the user, also outside the sandbox.
    pub processes: Option<u64>,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { timeout: Duration::from_secs(3600), memory_bytes: None, processes: None }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SandboxSpec {
    /// The root file system: its entries appear read-only at `/`.
    pub root: PathBuf,
    pub mounts: Vec<Mount>,
    /// A writable host directory, shown at `WORK_DIR`.
    pub work: PathBuf,
    /// The directory in which the command starts, relative to `WORK_DIR`; empty for `WORK_DIR`.
    pub dir: String,
    /// A writable host directory, shown at `/tmp`. It is on disk, so a large build does not fill memory.
    pub tmp: PathBuf,
    /// The cache volumes. They are mounted after `/tmp` and the read-only mounts, so they can be below either; those
    /// below `WORK_DIR` are mounted after it.
    pub volumes: Vec<Volume>,
    pub command: Vec<String>,
    /// The complete environment of the command; nothing comes from the host.
    pub env: BTreeMap<String, String>,
    pub network: bool,
    pub limits: Limits,
    pub stdout: PathBuf,
    pub stderr: PathBuf,
    /// Both outputs, in the order in which their lines arrived.
    pub log: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Exit {
    Code(i32),
    TimedOut,
}

#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error("cannot start the sandbox: {0}")]
    Spawn(io::Error),
    #[error("the sandbox could not be set up: {0}")]
    Setup(String),
    #[error("{}", command_not_found(command, path.as_deref()))]
    CommandNotFound { command: String, path: Option<String> },
    #[error("{path}: {error}")]
    Io { path: PathBuf, error: io::Error },
}

fn command_not_found(command: &str, path: Option<&str>) -> String {
    if command.contains('/') {
        return format!("the command {command} is not found in the root file system or the inputs of the action");
    }
    match path {
        Some(path) => format!("the command {command} is not found in the PATH of the action: {path}"),
        None => format!("the command {command} is not found, and the action has no PATH"),
    }
}

pub trait Sandbox: Send + Sync {
    /// Runs the command. Its output goes to `spec.stdout`, `spec.stderr` and `spec.log`, and each line also to `logs`
    /// while it runs.
    fn run(&self, spec: &SandboxSpec, logs: &dyn LogSink) -> Result<Exit, SandboxError>;
}
