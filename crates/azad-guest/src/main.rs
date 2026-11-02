//! The init of Azad's microVM sandbox, which runs only as PID 1 of a Linux guest (`init.rs`).

#[cfg(target_os = "linux")]
mod init;
#[cfg(target_os = "linux")]
mod sys;

#[cfg(target_os = "linux")]
fn main() {
    init::main();
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("azad-guest is the init of Azad's microVM sandbox, and runs only inside a Linux guest");
    std::process::exit(1);
}
