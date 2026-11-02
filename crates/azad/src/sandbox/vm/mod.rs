//! A sandbox in microVMs (decisions 0018 and 0019). Cloud Hypervisor runs a VM for each sandbox group, and the runs of
//! the group share it: the disks of each run are attached while it lasts, the root of the action as a read-only image
//! and a run disk, and a disk for each cache volume. The guest init (`azad-guest`) runs the command and sends its
//! output, exit code and work directory over vsock.
//!
//! The VM gives the command the same view as the bubblewrap sandbox, so an action gives the same outputs in both.

mod api;
mod disks;
mod group;
mod initramfs;
mod namespaces;
mod network;

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::log::LogSink;
use crate::sandbox::{Exit, Sandbox, SandboxError, SandboxSpec};
use crate::timings::Timings;
use group::VmGroup;

/// What a VM needs from the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VmConfig {
    /// The Cloud Hypervisor program.
    pub vmm: PathBuf,
    /// The guest kernel, built from `guest/kernel`.
    pub kernel: PathBuf,
    /// A static build of `azad-guest`.
    pub guest: PathBuf,
    /// Where the images of roots and the initramfs are kept.
    pub images: PathBuf,
    /// Directories in which the VMM may open the disks of runs: those of the run disks and of the cache volumes.
    pub landlock: Vec<PathBuf>,
    pub cpus: u32,
    pub memory_mib: u64,
    /// The room on the run disk for what a run writes, in bytes.
    pub disk_room: u64,
    /// The size of the disk of a new cache volume, in bytes; the file is sparse.
    pub volume_size: u64,
}

/// A sandbox that runs commands in microVMs: one VM for the runs without network and one for the runs with network,
/// each booted at its first run. A VM that ended, for example because its guest panicked, or that kept a run's disks,
/// is replaced at the next run.
#[derive(Debug)]
pub struct MicroVm {
    config: VmConfig,
    timings: Timings,
    groups: Mutex<BTreeMap<bool, Arc<VmGroup>>>,
}

impl MicroVm {
    pub fn new(config: VmConfig) -> Self {
        MicroVm { config, timings: Timings::default(), groups: Mutex::default() }
    }

    /// Records how long the images, the boots and the copies of runs take in `timings`.
    pub fn with_timings(mut self, timings: Timings) -> Self {
        self.timings = timings;
        self
    }

    /// The VM for runs with or without network.
    fn group(&self, network: bool) -> Result<Arc<VmGroup>, SandboxError> {
        let mut groups = self.groups.lock().map_err(|_| SandboxError::Setup("the VM lock is poisoned".to_owned()))?;
        if let Some(group) = groups.get(&network).filter(|group| group.takes_runs()) {
            return Ok(group.clone());
        }
        let group = Arc::new(VmGroup::start(&self.config, network, self.timings.clone())?);
        groups.insert(network, group.clone());
        Ok(group)
    }
}

impl Sandbox for MicroVm {
    fn run(&self, spec: &SandboxSpec, logs: &dyn LogSink) -> Result<Exit, SandboxError> {
        self.group(spec.network)?.run(spec, logs)
    }
}

fn io_error(path: &Path, error: io::Error) -> SandboxError {
    SandboxError::Io { path: path.to_owned(), error }
}
