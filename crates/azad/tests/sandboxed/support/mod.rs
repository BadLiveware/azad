//! Helpers for tests: a root file system made from the host's own programs, so tests run without network access.
//! The result is the same on one host every time, but differs between hosts.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Programs that tests use in their commands.
pub(crate) const TEST_PROGRAMS: &[&str] =
    &["sh", "bash", "cat", "cp", "mkdir", "ls", "sleep", "echo", "wc", "rm", "touch", "chmod", "head"];

const SEARCH_PATH: &[&str] = &["/usr/bin", "/bin", "/usr/sbin", "/sbin"];

/// The PATH that commands in a root file system from `host_rootfs` use.
pub(crate) const ROOTFS_PATH: &str = "/usr/bin";

/// Builds a root file system in `dest` with the given host programs in `/usr/bin` and the libraries that they load.
pub(crate) fn host_rootfs(dest: &Path, programs: &[&str]) {
    fs::create_dir_all(dest.join("usr/bin")).unwrap();
    symlink("usr/bin", dest.join("bin")).unwrap();
    let mut libraries = BTreeSet::new();
    for program in programs {
        let source = find_program(program);
        copy_file(&source, &dest.join("usr/bin").join(program));
        libraries.extend(shared_libraries(&source));
    }
    for library in libraries {
        let relative = library.strip_prefix("/").expect("ldd gives absolute paths");
        copy_file(&library, &dest.join(relative));
    }
}

fn find_program(name: &str) -> PathBuf {
    SEARCH_PATH
        .iter()
        .map(|dir| Path::new(dir).join(name))
        .find(|path| path.is_file())
        .unwrap_or_else(|| panic!("the host has no program {name}"))
}

fn copy_file(source: &Path, dest: &Path) {
    fs::create_dir_all(dest.parent().unwrap()).unwrap();
    let real = fs::canonicalize(source).unwrap();
    fs::copy(&real, dest).unwrap();
    fs::set_permissions(dest, fs::Permissions::from_mode(0o755)).unwrap();
}

fn shared_libraries(program: &Path) -> Vec<PathBuf> {
    let output = Command::new("ldd").arg(program).output().expect("ldd runs");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .flat_map(|line| {
            let (left, right) = line.split_once("=>").unwrap_or((line, ""));
            [left, right].map(|side| side.split_whitespace().next().unwrap_or_default().to_owned())
        })
        .filter(|path| path.starts_with('/'))
        .map(PathBuf::from)
        .collect()
}
