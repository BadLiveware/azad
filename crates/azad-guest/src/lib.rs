//! The guest side of Azad's microVM sandbox: the init of the VM, and the protocol between it and the host.
//!
//! One VM serves the runs of a sandbox group. For each run, the host attaches two disks: the root of the action as a
//! read-only EROFS image, and a run disk that holds the work directory, `/tmp`, the read-only mounts and the run's
//! [`RunSpec`]; and one disk for each cache volume. The init runs the command and sends [`Message`]s: each line of
//! output, the exit code, and the work directory as a tar.

pub mod protocol;
