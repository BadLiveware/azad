use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::log::LogSink;
use crate::sandbox::process::OutputFiles;
use crate::sandbox::spawn::{STATUS_FD, spawn};
use crate::sandbox::{Exit, PATH_VARIABLE, Sandbox, SandboxError, SandboxSpec, WORK_DIR};

const PROGRAM: &str = "bwrap";
const TMP_DIR: &str = "/tmp";
const ETC: &str = "etc";
const RESOLV_CONF: &str = "resolv.conf";
const HOST_RESOLV_CONF: &str = "/etc/resolv.conf";
const EXIT_CODE_KEY: &str = "\"exit-code\"";
const STDERR_TAIL_BYTES: u64 = 2048;
const NOT_FOUND_TEXT: &str = "No such file or directory";
/// Root entries that the sandbox makes itself, so the root file system does not provide them.
const RESERVED_ROOT_ENTRIES: [&str; 4] = ["proc", "dev", "tmp", "work"];

/// A sandbox that runs `bwrap` (bubblewrap) for each command.
#[derive(Clone, Debug)]
pub struct Bubblewrap {
    program: PathBuf,
}

impl Default for Bubblewrap {
    fn default() -> Self {
        Bubblewrap { program: PathBuf::from(PROGRAM) }
    }
}

impl Bubblewrap {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Bubblewrap { program: program.into() }
    }

    fn arguments(spec: &SandboxSpec) -> Result<Vec<OsString>, SandboxError> {
        let mut args: Vec<OsString> =
            ["--unshare-all", "--die-with-parent", "--new-session"].into_iter().map(OsString::from).collect();
        if spec.network {
            args.push("--share-net".into());
        }
        let mut reserved = RESERVED_ROOT_ENTRIES.to_vec();
        if spec.network {
            reserved.push(ETC);
        }
        bind_entries(&mut args, &spec.root, "", &reserved)?;
        if spec.network {
            // Images do not ship /etc/resolv.conf; container runtimes add the host's at run time, and so does Azad
            // for an action with network. /etc becomes a tmpfs that holds the image's entries and this one file.
            args.extend(["--dir".into(), format!("/{ETC}").into()]);
            let image_etc = spec.root.join(ETC);
            if image_etc.is_dir() {
                bind_entries(&mut args, &image_etc, &format!("/{ETC}"), &[RESOLV_CONF])?;
            }
            args.extend(["--ro-bind".into(), HOST_RESOLV_CONF.into(), format!("/{ETC}/{RESOLV_CONF}").into()]);
        }
        args.extend(["--proc", "/proc", "--dev", "/dev"].map(OsString::from));
        args.extend(["--bind".into(), spec.tmp.clone().into(), TMP_DIR.into()]);
        let mut mounts: Vec<_> = spec.mounts.iter().collect();
        mounts.sort_by(|a, b| a.target.cmp(&b.target));
        for mount in mounts {
            args.extend(["--ro-bind".into(), mount.host.clone().into(), mount.target.clone().into()]);
        }
        let mut volumes: Vec<_> = spec.volumes.iter().collect();
        volumes.sort_by(|a, b| a.target.cmp(&b.target));
        let (in_work, elsewhere): (Vec<_>, Vec<_>) =
            volumes.into_iter().partition(|volume| volume.target.starts_with(&format!("{WORK_DIR}/")));
        for volume in elsewhere {
            args.extend(["--bind".into(), volume.dir.clone().into(), volume.target.clone().into()]);
        }
        args.extend(["--bind".into(), spec.work.clone().into(), WORK_DIR.into()]);
        for volume in in_work {
            args.extend(["--bind".into(), volume.dir.clone().into(), volume.target.clone().into()]);
        }
        // bubblewrap builds the root on a tmpfs, which stays writable; the root's entries are read-only, so the root
        // is too, as the contract says.
        args.extend(["--remount-ro", "/"].map(OsString::from));
        let start = match spec.dir.is_empty() {
            true => WORK_DIR.to_owned(),
            false => format!("{WORK_DIR}/{}", spec.dir),
        };
        args.extend(["--chdir".into(), start.into()]);
        args.extend(["--json-status-fd".into(), STATUS_FD.to_string().into(), "--".into()]);
        args.extend(spec.command.iter().map(OsString::from));
        Ok(args)
    }
}

enum RootEntry {
    Link(OsString),
    Bind(PathBuf),
}

/// Shows each entry of the host directory `dir` read-only at `<prefix>/<name>`, except the `reserved` names.
fn bind_entries(args: &mut Vec<OsString>, dir: &Path, prefix: &str, reserved: &[&str]) -> Result<(), SandboxError> {
    for (name, entry) in dir_entries(dir, reserved)? {
        let target = format!("{prefix}/{name}");
        match entry {
            RootEntry::Link(link) => args.extend(["--symlink".into(), link, target.into()]),
            RootEntry::Bind(path) => args.extend(["--ro-bind".into(), path.into(), target.into()]),
        }
    }
    Ok(())
}

fn dir_entries(root: &Path, reserved: &[&str]) -> Result<Vec<(String, RootEntry)>, SandboxError> {
    let io_error = |source| SandboxError::Io { path: root.to_owned(), error: source };
    let mut entries = Vec::new();
    for entry in fs::read_dir(root).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        let Ok(name) = entry.file_name().into_string() else {
            return Err(SandboxError::Setup(format!("a root entry name is not UTF-8: {:?}", entry.file_name())));
        };
        if reserved.contains(&name.as_str()) {
            continue;
        }
        let path = entry.path();
        let file_type = entry.file_type().map_err(io_error)?;
        let item = match file_type.is_symlink() {
            true => RootEntry::Link(fs::read_link(&path).map_err(io_error)?.into_os_string()),
            false => RootEntry::Bind(path),
        };
        entries.push((name, item));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(entries)
}

impl Sandbox for Bubblewrap {
    fn run(&self, spec: &SandboxSpec, logs: &dyn LogSink) -> Result<Exit, SandboxError> {
        let output = OutputFiles::create(&spec.stdout, &spec.stderr, &spec.log)?;
        let (mut status_reader, status_writer) = io::pipe().map_err(SandboxError::Spawn)?;
        let mut command = Command::new(&self.program);
        command
            .args(Self::arguments(spec)?)
            .env_clear()
            .envs(&spec.env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = spawn(&mut command, status_writer, &spec.limits).map_err(SandboxError::Spawn)?;
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");
        let exited = output.copy_until_exit(&mut child, stdout, stderr, spec.limits.timeout, logs)?;
        if !exited {
            return Ok(Exit::TimedOut);
        }
        let mut status = String::new();
        status_reader.read_to_string(&mut status).map_err(SandboxError::Spawn)?;
        match exit_code(&status) {
            Some(code) => Ok(Exit::Code(code)),
            None => Err(start_error(spec)),
        }
    }
}

/// Why the command did not start. bwrap reports a failed exec only as a line of text: `bwrap: execvp <command>: <error>`.
fn start_error(spec: &SandboxSpec) -> SandboxError {
    let tail = stderr_tail(&spec.stderr);
    let command = spec.command.first().cloned().unwrap_or_default();
    let not_found = format!("execvp {command}: {NOT_FOUND_TEXT}");
    match tail.lines().any(|line| line.ends_with(&not_found)) {
        true => SandboxError::CommandNotFound { command, path: spec.env.get(PATH_VARIABLE).cloned() },
        false => SandboxError::Setup(tail),
    }
}

/// The exit code of the command, from bwrap's status documents. Without one, the command did not start.
fn exit_code(status: &str) -> Option<i32> {
    let start = status.find(EXIT_CODE_KEY)? + EXIT_CODE_KEY.len();
    let rest = status[start..].trim_start().strip_prefix(':')?.trim_start();
    let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '-')).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn stderr_tail(path: &Path) -> String {
    let read = || -> io::Result<String> {
        let mut file = File::open(path)?;
        let length = file.metadata()?.len();
        file.seek(SeekFrom::Start(length.saturating_sub(STDERR_TAIL_BYTES)))?;
        let mut text = String::new();
        file.read_to_string(&mut text)?;
        Ok(text.trim().to_owned())
    };
    read().unwrap_or_else(|e| format!("(the error output cannot be read: {e})"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_code_is_read_from_the_status_documents() {
        let status = "{ \"child-pid\": 12 }\n{ \"exit-code\": 3 }\n";
        assert_eq!(exit_code(status), Some(3));
        assert_eq!(exit_code("{ \"child-pid\": 12 }\n"), None);
        assert_eq!(exit_code(""), None);
    }
}
