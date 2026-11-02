//! Commands in a new user and network namespace, or in existing ones.
//!
//! The standard library cannot start a child in other namespaces, so this module uses `pre_exec`, which is `unsafe`.
//! The hooks call only `unshare` and `setns`, which are async-signal-safe.
#![allow(unsafe_code)]

use std::io;
use std::os::fd::RawFd;
use std::os::unix::process::CommandExt;
use std::process::Command;

/// Starts `command` in a new user namespace and a new network namespace that the user namespace owns.
pub(super) fn in_new_namespaces(command: &mut Command) -> &mut Command {
    // SAFETY: the closure runs in the child between fork and exec; it calls one system call and does not allocate.
    unsafe {
        command.pre_exec(|| check(libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNET)));
    }
    command
}

/// Starts `command` in the user namespace and the network namespace of the open namespace files `user` and `net`.
/// The user namespace comes first, because it gives the capabilities to enter the network namespace.
pub(super) fn in_namespaces(command: &mut Command, user: RawFd, net: RawFd) -> &mut Command {
    // SAFETY: the closure runs in the child between fork and exec; it calls two system calls on descriptors that the
    // parent keeps open, and does not allocate.
    unsafe {
        command.pre_exec(move || {
            check(libc::setns(user, libc::CLONE_NEWUSER))?;
            check(libc::setns(net, libc::CLONE_NEWNET))
        });
    }
    command
}

/// The effective user and group of this process.
pub(super) fn user_and_group() -> (u32, u32) {
    // SAFETY: plain system calls that cannot fail.
    unsafe { (libc::geteuid(), libc::getegid()) }
}

fn check(result: libc::c_int) -> io::Result<()> {
    match result {
        0 => Ok(()),
        _ => Err(io::Error::last_os_error()),
    }
}
