//! Running a command on this host, outside a sandbox, for the host actions of decision 0015: the developer's tools,
//! environment and working tree.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::io;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::log::LogSink;
use crate::sandbox::process::OutputFiles;
use crate::sandbox::{ContainerRun, Exit, Limits, PATH_VARIABLE, SandboxError};

/// What a command on the host needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostSpec {
    /// The directory in which the command starts.
    pub dir: PathBuf,
    pub command: Vec<String>,
    /// Variables in place of the host's variables of the same name; the command gets the rest of the host's
    /// environment too.
    pub env: BTreeMap<String, String>,
    /// Variables of this process that the command does not get, such as Azad's own secrets.
    pub withheld: BTreeSet<String>,
    /// The image in which the command runs, through the host's container runtime; `None` to run it on the host.
    pub container: Option<ContainerRun>,
    pub limits: Limits,
    pub stdout: PathBuf,
    pub stderr: PathBuf,
    /// Both outputs, in the order in which their lines arrived.
    pub log: PathBuf,
}

/// Runs the command. Its output goes to the files of `spec`, and each line also to `logs` while it runs.
pub fn run_on_host(spec: &HostSpec, logs: &dyn LogSink) -> Result<Exit, SandboxError> {
    let output = OutputFiles::create(&spec.stdout, &spec.stderr, &spec.log)?;
    let (program, args) =
        spec.command.split_first().ok_or_else(|| SandboxError::Setup("the command is empty".into()))?;
    let mut command = match &spec.container {
        Some(container) => container.command(&spec.dir, &spec.command, &spec.env)?,
        None => {
            let mut command = Command::new(program);
            command.args(args).current_dir(&spec.dir).envs(&spec.env);
            command
        }
    };
    for name in &spec.withheld {
        command.env_remove(name);
    }
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|error| match spec.container {
        Some(_) => SandboxError::Spawn(error),
        None => start_error(spec, program, error),
    })?;
    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");
    if !output.copy_until_exit(&mut child, stdout, stderr, spec.limits.timeout, logs)? {
        if let Some(container) = &spec.container {
            container.remove();
        }
        return Ok(Exit::TimedOut);
    }
    let status = child.wait().map_err(SandboxError::Spawn)?;
    Ok(Exit::Code(exit_code(status)))
}

fn start_error(spec: &HostSpec, program: &str, error: io::Error) -> SandboxError {
    if error.kind() != io::ErrorKind::NotFound {
        return SandboxError::Spawn(error);
    }
    if !spec.dir.is_dir() {
        return SandboxError::Io { path: spec.dir.clone(), error };
    }
    let path = spec.env.get(PATH_VARIABLE).cloned().or_else(|| env::var(PATH_VARIABLE).ok());
    SandboxError::CommandNotFound { command: program.to_owned(), path }
}

/// The exit code of a process; a process that a signal stopped gives 128 and the signal, as shells report it.
fn exit_code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    const SIGNAL_BASE: i32 = 128;
    match (status.code(), status.signal()) {
        (Some(code), _) => code,
        (None, Some(signal)) => SIGNAL_BASE + signal,
        (None, None) => SIGNAL_BASE,
    }
}
