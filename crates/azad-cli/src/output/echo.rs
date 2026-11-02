//! The terminal's echo of typed keys, off while a live view draws.
//!
//! indicatif leaves the cursor at the last column of the screen after each frame. A character that the terminal
//! echoes there, such as `^C` or a key that someone types, starts a new line that indicatif does not count, so its
//! next frame leaves a line of the old one behind. Without echo, nothing writes between frames. Signals still work:
//! only `ECHO` changes, not `ISIG`. Keys typed during a run still reach the shell after it.
//!
//! A process in the background of a terminal does not change its modes: a change from a background process group
//! stops the process (`SIGTTOU`), and keys that someone types do not reach it anyway.

use std::io;

use nix::sys::termios::{LocalFlags, SetArg, Termios, tcgetattr, tcsetattr};
use nix::unistd::{getpgrp, tcgetpgrp};

/// The terminal on standard error with its echo off, until `restore` or drop.
pub(crate) struct EchoOff {
    saved: Option<Termios>,
}

impl EchoOff {
    /// Turns the echo off, when standard error is a terminal that echoes and this process is in its foreground.
    pub(crate) fn new() -> Self {
        if !is_foreground() {
            return EchoOff { saved: None };
        }
        let Ok(saved) = tcgetattr(io::stderr()) else { return EchoOff { saved: None } };
        if !saved.local_flags.contains(LocalFlags::ECHO) {
            return EchoOff { saved: None };
        }
        let mut quiet = saved.clone();
        quiet.local_flags.remove(LocalFlags::ECHO);
        match tcsetattr(io::stderr(), SetArg::TCSANOW, &quiet) {
            Ok(()) => EchoOff { saved: Some(saved) },
            Err(_) => EchoOff { saved: None },
        }
    }

    /// Puts the echo back as it was.
    pub(crate) fn restore(&mut self) {
        if let Some(saved) = self.saved.take() {
            tcsetattr(io::stderr(), SetArg::TCSANOW, &saved).ok();
        }
    }
}

/// Whether this process's group is the foreground group of the terminal on standard error.
fn is_foreground() -> bool {
    tcgetpgrp(io::stderr()).is_ok_and(|group| group == getpgrp())
}

impl Drop for EchoOff {
    fn drop(&mut self) {
        self.restore();
    }
}
