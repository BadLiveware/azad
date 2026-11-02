//! What the host and the guest exchange: the run's spec on the run disk, and framed messages over vsock.
//!
//! A frame is one byte of kind, the length of the payload as a little-endian `u32`, and the payload.
//!
//! The guest connects to the host's control port once, when it is ready; the host closes that connection to power the
//! VM off. For each run, the host connects to the guest's run port and sends [`Message::Run`]; the guest answers on the
//! same connection, and closes it when it no longer uses the run's disks.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

/// The vsock port on which the host listens for the guest's control connection.
pub const CONTROL_PORT: u32 = 1100;
/// The vsock port on which the guest listens for runs.
pub const RUN_PORT: u32 = 1101;

/// The entries of the run disk.
pub const SPEC_FILE: &str = "spec.json";
pub const WORK_DIR: &str = "work";
pub const TMP_DIR: &str = "tmp";
/// The read-only mounts, each in a directory named by its index in [`RunSpec::mounts`].
pub const MOUNTS_DIR: &str = "mounts";
/// The directory of a cache volume's disk that holds the volume's files.
pub const VOLUME_DATA_DIR: &str = "data";

/// The largest payload of one frame. Larger data, such as the work directory, goes in several frames.
pub const MAX_PAYLOAD: usize = 1 << 20;

/// What the guest runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunSpec {
    pub command: Vec<String>,
    /// The complete environment of the command.
    pub env: BTreeMap<String, String>,
    /// The directory in which the command starts, relative to `/work`; empty for `/work`.
    pub dir: String,
    /// The targets of the read-only mounts, in the order of their directories in [`MOUNTS_DIR`].
    pub mounts: Vec<String>,
    /// The targets of the cache volumes, in the order of their disks in [`RunRequest::volumes`].
    pub volumes: Vec<String>,
    /// The content of `/etc/resolv.conf` for an action with network.
    pub resolv_conf: Option<String>,
    /// The user and group of the command: those of the host user, as in the bubblewrap sandbox.
    pub uid: u32,
    pub gid: u32,
    pub memory_bytes: Option<u64>,
    pub processes: Option<u64>,
}

/// The disks of one run, by the PCI address (such as `0000:00:06.0`) at which the host attached them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRequest {
    /// The read-only EROFS image of the root, on persistent memory.
    pub root: String,
    /// The run disk, which holds [`SPEC_FILE`].
    pub run: String,
    /// The ext4 disks of the cache volumes.
    pub volumes: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    /// The guest is ready for runs.
    Ready,
    /// The host asks for a run.
    Run(RunRequest),
    /// One line of standard output, with its line end.
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    /// The command ended; a command that a signal ended has 128 plus the signal's number, as in a shell.
    Exit(i32),
    /// The command was not found.
    NotFound,
    /// The sandbox could not be set up, or the command could not start.
    Failed(String),
    /// A part of the tar of the work directory after the command.
    Work(Vec<u8>),
    /// The tar of the work directory is complete; nothing follows.
    WorkEnd,
}

const STDOUT: u8 = 1;
const STDERR: u8 = 2;
const EXIT: u8 = 3;
const NOT_FOUND: u8 = 4;
const FAILED: u8 = 5;
const WORK: u8 = 6;
const WORK_END: u8 = 7;
const READY: u8 = 8;
const RUN: u8 = 9;

const HEADER_BYTES: usize = 5;

pub fn write_message(out: &mut impl Write, message: &Message) -> io::Result<()> {
    let (kind, payload): (u8, &[u8]) = match message {
        Message::Ready => (READY, &[]),
        Message::Run(request) => {
            let encoded = serde_json::to_vec(request).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
            return write_frame(out, RUN, &encoded);
        }
        Message::Stdout(line) => (STDOUT, line),
        Message::Stderr(line) => (STDERR, line),
        Message::Exit(code) => return write_frame(out, EXIT, &code.to_le_bytes()),
        Message::NotFound => (NOT_FOUND, &[]),
        Message::Failed(reason) => (FAILED, reason.as_bytes()),
        Message::Work(part) => (WORK, part),
        Message::WorkEnd => (WORK_END, &[]),
    };
    write_frame(out, kind, payload)
}

fn write_frame(out: &mut impl Write, kind: u8, payload: &[u8]) -> io::Result<()> {
    if payload.len() > MAX_PAYLOAD {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "a frame payload is larger than the limit"));
    }
    let length = u32::try_from(payload.len()).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let mut frame = Vec::with_capacity(HEADER_BYTES + payload.len());
    frame.push(kind);
    frame.extend_from_slice(&length.to_le_bytes());
    frame.extend_from_slice(payload);
    out.write_all(&frame)?;
    out.flush()
}

/// The next message, or none at the end of the stream.
pub fn read_message(input: &mut impl Read) -> io::Result<Option<Message>> {
    let mut header = [0u8; HEADER_BYTES];
    match input.read_exact(&mut header) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let length = u32::from_le_bytes([header[1], header[2], header[3], header[4]]) as usize;
    if length > MAX_PAYLOAD {
        return Err(invalid(format!("a frame of {length} bytes is larger than the limit")));
    }
    let mut payload = vec![0u8; length];
    input.read_exact(&mut payload)?;
    let message = match header[0] {
        STDOUT => Message::Stdout(payload),
        STDERR => Message::Stderr(payload),
        EXIT => Message::Exit(i32::from_le_bytes(
            payload.try_into().map_err(|_| invalid("an exit frame of the wrong size"))?,
        )),
        NOT_FOUND => Message::NotFound,
        FAILED => Message::Failed(String::from_utf8_lossy(&payload).into_owned()),
        WORK => Message::Work(payload),
        WORK_END => Message::WorkEnd,
        READY => Message::Ready,
        RUN => Message::Run(serde_json::from_slice(&payload).map_err(|e| invalid(e.to_string()))?),
        other => return Err(invalid(format!("an unknown frame kind {other}"))),
    };
    Ok(Some(message))
}

fn invalid(reason: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason.into())
}

/// Writes data as [`Message::Work`] frames, each at most [`MAX_PAYLOAD`] bytes.
#[derive(Debug)]
pub struct WorkWriter<W: Write> {
    out: W,
    buffer: Vec<u8>,
}

impl<W: Write> WorkWriter<W> {
    pub fn new(out: W) -> Self {
        WorkWriter { out, buffer: Vec::with_capacity(MAX_PAYLOAD) }
    }

    /// Sends what is buffered and [`Message::WorkEnd`].
    pub fn finish(mut self) -> io::Result<W> {
        self.send()?;
        write_message(&mut self.out, &Message::WorkEnd)?;
        Ok(self.out)
    }

    fn send(&mut self) -> io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let part = std::mem::take(&mut self.buffer);
        write_message(&mut self.out, &Message::Work(part))
    }
}

impl<W: Write> Write for WorkWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let room = MAX_PAYLOAD - self.buffer.len();
        let taken = bytes.len().min(room);
        self.buffer.extend_from_slice(&bytes[..taken]);
        if self.buffer.len() == MAX_PAYLOAD {
            self.send()?;
        }
        Ok(taken)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_read_back_as_written_and_the_work_tar_is_split_into_frames() {
        let large = vec![7u8; MAX_PAYLOAD * 2 + 3];
        let mut stream = Vec::new();
        let request =
            RunRequest { root: "0000:00:06.0".to_owned(), run: "0000:00:07.0".to_owned(), volumes: Vec::new() };
        for message in [Message::Run(request.clone()), Message::Exit(-3), Message::Failed("why".to_owned())] {
            write_message(&mut stream, &message).unwrap();
        }
        let mut work = WorkWriter::new(&mut stream);
        work.write_all(&large).unwrap();
        work.finish().unwrap();

        let mut reader = stream.as_slice();
        let mut messages = Vec::new();
        while let Some(message) = read_message(&mut reader).unwrap() {
            messages.push(message);
        }
        let work: Vec<u8> = messages
            .iter()
            .filter_map(|message| match message {
                Message::Work(part) => Some(part.clone()),
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(messages[..3], [Message::Run(request), Message::Exit(-3), Message::Failed("why".to_owned())]);
        assert_eq!(work, large);
        assert_eq!(messages.iter().filter(|m| matches!(m, Message::Work(_))).count(), 3);
        assert_eq!(messages.last(), Some(&Message::WorkEnd));
    }
}
