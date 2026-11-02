//! The messages of the plan: JSON, one message per line, on a Unix socket. Each connection is one sandbox group, which
//! ends when the client closes the connection.

use std::io::{self, BufRead, Write};

use serde::{Deserialize, Serialize};

use crate::ir::Digest;
use crate::log::Stream;
use crate::trust::Trust;

/// What a sandbox group needs from its VM.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Shape {
    /// Whether the group's actions have network, behind the rules of the executor.
    pub network: bool,
}

/// From the client to the service.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub(super) enum Request {
    /// The first message of a connection.
    Open { shape: Shape, trust: Trust },
    /// Runs the stored action; `id` names the run in the events.
    Run {
        id: u64,
        #[serde(with = "digest_text")]
        action: Digest,
    },
}

/// From the service to the client.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub(super) enum Event {
    Opened,
    /// The group could not be opened; the service closes the connection.
    Refused {
        reason: String,
    },
    Log {
        id: u64,
        stream: LogStream,
        line: String,
    },
    /// The run ended, and its result is stored.
    Done {
        id: u64,
        #[serde(with = "digest_text")]
        result: Digest,
        discarded: Vec<String>,
    },
    /// The run could not end with a result.
    Failed {
        id: u64,
        reason: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum LogStream {
    Stdout,
    Stderr,
}

impl From<Stream> for LogStream {
    fn from(stream: Stream) -> Self {
        match stream {
            Stream::Stdout => LogStream::Stdout,
            Stream::Stderr => LogStream::Stderr,
        }
    }
}

impl From<LogStream> for Stream {
    fn from(stream: LogStream) -> Self {
        match stream {
            LogStream::Stdout => Stream::Stdout,
            LogStream::Stderr => Stream::Stderr,
        }
    }
}

pub(super) fn send<T: Serialize>(out: &mut impl Write, message: &T) -> io::Result<()> {
    let mut line = serde_json::to_vec(message).map_err(io::Error::other)?;
    line.push(b'\n');
    out.write_all(&line)?;
    out.flush()
}

/// The next message, or none at the end of the connection.
pub(super) fn receive<T: for<'de> Deserialize<'de>>(input: &mut impl BufRead) -> io::Result<Option<T>> {
    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    serde_json::from_str(&line).map(Some).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// A digest in its text form, such as `sha256:<hex>/<size>`.
mod digest_text {
    use serde::{Deserialize, Deserializer, Serializer};

    use crate::ir::Digest;

    pub(super) fn serialize<S: Serializer>(digest: &Digest, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(digest)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Digest, D::Error> {
        String::deserialize(deserializer)?.parse().map_err(serde::de::Error::custom)
    }
}
