//! The executor as a service, behind the plan (contract 2 in "What Azad is", `AGENTS.md`): a client opens a sandbox
//! group with a shape and a trust level, runs actions in it, and gets their output and results back as events. One VM
//! serves a group, so its boot is paid once for several actions. See decision 0019.
//!
//! The client and the service share a store: an action goes to the service as the digest of its encoding, and its
//! result comes back as the digest of the stored result. The service runs actions and keeps cache volumes; the client
//! decides what runs and keeps the action cache.

mod client;
mod protocol;
mod service;

pub use client::{ExecutorClient, ExecutorError};
pub use protocol::Shape;
pub use service::{ExecutorService, ServiceSandbox};
