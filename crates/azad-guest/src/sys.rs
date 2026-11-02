//! The system calls of the guest that only `unsafe` code can make: taking ownership of the descriptor that nix's
//! `accept4` returns, and the `ioctl` that brings a network interface up. Also the rest of the vsock sockets between
//! the guest and the host.
#![allow(unsafe_code)]

use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use nix::libc;
use nix::sys::socket::{
    AddressFamily, Backlog, SockFlag, SockType, VsockAddr, accept4, bind, connect, listen as listen_on, socket,
};

/// The context ID of the host, to which the guest connects.
const HOST_CID: u32 = 2;
/// Any context ID: the guest listens on its own.
const ANY_CID: u32 = u32::MAX;
const BACKLOG: i32 = 64;
const LOOPBACK: &[u8] = b"lo\0";

fn stream_socket() -> Result<OwnedFd, String> {
    socket(AddressFamily::Vsock, SockType::Stream, SockFlag::SOCK_CLOEXEC, None)
        .map_err(|e| format!("a vsock socket: {e}"))
}

pub(crate) fn connect_to_host(port: u32) -> Result<File, String> {
    let fd = stream_socket()?;
    connect(fd.as_raw_fd(), &VsockAddr::new(HOST_CID, port))
        .map_err(|e| format!("connecting to the host on vsock port {port}: {e}"))?;
    Ok(File::from(fd))
}

pub(crate) fn listen(port: u32) -> Result<OwnedFd, String> {
    let fd = stream_socket()?;
    bind(fd.as_raw_fd(), &VsockAddr::new(ANY_CID, port)).map_err(|e| format!("binding vsock port {port}: {e}"))?;
    let backlog = Backlog::new(BACKLOG).map_err(|e| format!("a listen backlog: {e}"))?;
    listen_on(&fd, backlog).map_err(|e| format!("listening on vsock port {port}: {e}"))?;
    Ok(fd)
}

pub(crate) fn accept(listener: &OwnedFd) -> Result<OwnedFd, String> {
    let fd = accept4(listener.as_raw_fd(), SockFlag::SOCK_CLOEXEC).map_err(|e| format!("accepting a run: {e}"))?;
    // SAFETY: accept4 returned a new descriptor, which nothing else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Brings the loopback interface up, as a new network namespace of bubblewrap has it.
pub(crate) fn bring_up_loopback() -> Result<(), String> {
    let fd = socket(AddressFamily::Inet, SockType::Datagram, SockFlag::SOCK_CLOEXEC, None)
        .map_err(|e| format!("a socket for the loopback interface: {e}"))?;
    // SAFETY: ifreq is a plain C structure, for which all zero bytes are a valid value.
    let mut request: libc::ifreq = unsafe { std::mem::zeroed() };
    for (to, from) in request.ifr_name.iter_mut().zip(LOOPBACK) {
        *to = *from as libc::c_char;
    }
    request.ifr_ifru.ifru_flags = (libc::IFF_UP | libc::IFF_RUNNING) as libc::c_short;
    // SAFETY: SIOCSIFFLAGS reads one ifreq, which lives until the call returns.
    match unsafe { libc::ioctl(fd.as_raw_fd(), libc::SIOCSIFFLAGS as _, &request) } {
        0 => Ok(()),
        _ => Err(format!("bringing the loopback interface up: {}", std::io::Error::last_os_error())),
    }
}
