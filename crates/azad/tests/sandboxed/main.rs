//! The integration tests that run actions in a sandbox, in one binary, so that the library is linked once for them.
//! Azad's own `//:sandbox_tests` runs them on the host, which has bubblewrap.

mod engine;
mod exec;
mod modules;
mod orchestrator;
mod remote;
mod run;
mod support;
mod tasks;
mod vm;
