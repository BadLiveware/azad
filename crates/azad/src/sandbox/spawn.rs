//! Starting `bwrap` with its status pipe at a fixed file descriptor, and with resource limits.
//!
//! The standard library cannot give a child an extra file descriptor or set its resource limits, so this module uses
//! `pre_exec`, which is `unsafe`. The hook calls only `dup2`, `fcntl` and `setrlimit`, which are async-signal-safe.
#![allow(unsafe_code)]

use std::io::{self, PipeWriter};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};

use crate::sandbox::Limits;

pub(crate) const STATUS_FD: i32 = 3;

pub(crate) fn spawn(command: &mut Command, status: PipeWriter, limits: &Limits) -> io::Result<Child> {
    let fd = status.as_raw_fd();
    let memory = limits.memory_bytes;
    let processes = limits.processes;
    // SAFETY: the closure runs in the child between fork and exec; it calls only async-signal-safe functions and does
    // not allocate.
    unsafe {
        command.pre_exec(move || {
            pass_fd(fd)?;
            if let Some(bytes) = memory {
                set_limit(libc::RLIMIT_AS, bytes)?;
            }
            if let Some(count) = processes {
                set_limit(libc::RLIMIT_NPROC, count)?;
            }
            Ok(())
        });
    }
    let child = command.spawn();
    drop(status);
    child
}

fn pass_fd(fd: i32) -> io::Result<()> {
    // SAFETY: plain system calls on file descriptors that this process owns.
    let result = unsafe {
        match fd == STATUS_FD {
            true => libc::fcntl(fd, libc::F_SETFD, 0),
            false => libc::dup2(fd, STATUS_FD),
        }
    };
    match result < 0 {
        true => Err(io::Error::last_os_error()),
        false => Ok(()),
    }
}

/// The type of `setrlimit`'s resource, which glibc and musl declare differently.
#[cfg(target_env = "gnu")]
type Resource = libc::__rlimit_resource_t;
#[cfg(not(target_env = "gnu"))]
type Resource = libc::c_int;

fn set_limit(resource: Resource, value: u64) -> io::Result<()> {
    let limit = libc::rlimit { rlim_cur: value, rlim_max: value };
    // SAFETY: `limit` is a valid rlimit that lives for the call.
    let result = unsafe { libc::setrlimit(resource, &limit) };
    match result < 0 {
        true => Err(io::Error::last_os_error()),
        false => Ok(()),
    }
}
