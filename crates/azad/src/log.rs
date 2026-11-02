//! Lines of an action's output while it runs, for whoever shows them. The sandbox produces them; the run loop turns
//! them into events.

/// Which output a line came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// Receives the lines of a running action, from more than one thread.
pub trait LogSink: Sync {
    fn line(&self, stream: Stream, line: &str);
}

/// A sink that drops every line.
#[derive(Debug, Default, Clone, Copy)]
pub struct Discard;

impl LogSink for Discard {
    fn line(&self, _stream: Stream, _line: &str) {}
}
