//! What the sandbox and host runs share: the output of a running command, copied to files and to a log sink line by
//! line, and a deadline for the command.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStderr, ChildStdout};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use crate::log::{LogSink, Stream};
use crate::sandbox::SandboxError;

const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// The files that a command's output goes to: its standard output, its standard error, and both in the order in which
/// their lines arrived.
pub(crate) struct OutputFiles {
    stdout: File,
    stderr: File,
    log: Mutex<File>,
}

impl OutputFiles {
    pub(crate) fn create(stdout: &Path, stderr: &Path, log: &Path) -> Result<Self, SandboxError> {
        let create =
            |path: &Path| File::create(path).map_err(|error| SandboxError::Io { path: path.to_owned(), error });
        Ok(OutputFiles { stdout: create(stdout)?, stderr: create(stderr)?, log: Mutex::new(create(log)?) })
    }

    /// Writes one line of output that arrives other than through a pipe, such as from a VM.
    pub(crate) fn write_line(&mut self, stream: Stream, line: &[u8], logs: &dyn LogSink) -> io::Result<()> {
        let file = match stream {
            Stream::Stdout => &mut self.stdout,
            Stream::Stderr => &mut self.stderr,
        };
        record(line, file, &self.log, stream, logs)
    }

    /// Copies the output of `child` until its pipes close, and kills it when it runs longer than `timeout`. Returns
    /// whether it exited by itself.
    pub(crate) fn copy_until_exit(
        self,
        child: &mut Child,
        stdout: ChildStdout,
        stderr: ChildStderr,
        timeout: Duration,
        logs: &dyn LogSink,
    ) -> Result<bool, SandboxError> {
        let OutputFiles { stdout: stdout_file, stderr: stderr_file, log } = self;
        thread::scope(|scope| {
            let log = &log;
            let out = scope.spawn(move || copy_lines(stdout, stdout_file, log, Stream::Stdout, logs));
            let err = scope.spawn(move || copy_lines(stderr, stderr_file, log, Stream::Stderr, logs));
            let exited = wait_until(child, Instant::now() + timeout);
            // The pipes close when the command's processes end, so the copies end too.
            for result in [out.join(), err.join()] {
                result
                    .map_err(|_| SandboxError::Setup("a log copy thread panicked".to_owned()))?
                    .map_err(SandboxError::Spawn)?;
            }
            exited
        })
    }
}

/// Waits for the child, and kills it at the deadline. Returns whether it exited by itself.
fn wait_until(child: &mut Child, deadline: Instant) -> Result<bool, SandboxError> {
    loop {
        if child.try_wait().map_err(SandboxError::Spawn)?.is_some() {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            child.kill().map_err(SandboxError::Spawn)?;
            child.wait().map_err(SandboxError::Spawn)?;
            return Ok(false);
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// Copies a pipe into its file and into the shared log, and gives each complete line to `logs` as it arrives.
fn copy_lines(
    pipe: impl Read,
    mut file: File,
    log: &Mutex<File>,
    stream: Stream,
    logs: &dyn LogSink,
) -> io::Result<()> {
    let mut reader = BufReader::new(pipe);
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = reader.read_until(b'\n', &mut line)?;
        if n == 0 {
            return Ok(());
        }
        record(&line, &mut file, log, stream, logs)?;
    }
}

fn record(line: &[u8], file: &mut File, log: &Mutex<File>, stream: Stream, logs: &dyn LogSink) -> io::Result<()> {
    file.write_all(line)?;
    log.lock().expect("the log lock is not poisoned").write_all(line)?;
    let text = String::from_utf8_lossy(line);
    logs.line(stream, text.trim_end_matches(['\n', '\r']));
    Ok(())
}
