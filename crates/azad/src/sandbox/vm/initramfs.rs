//! The initramfs of a VM: the guest init (`azad-guest`) as `/init`, the directories it mounts on, and `/dev/console`,
//! as a cpio archive in the "newc" format, which the kernel unpacks. It is made once per build of the guest init.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::ir::Digest;
use crate::sandbox::SandboxError;

const NEWC_MAGIC: &str = "070701";
const TRAILER: &str = "TRAILER!!!";
const ALIGNMENT: usize = 4;
const FILE_TYPE_BITS: u32 = 0o170_000;
const DIRECTORY_TYPE: u32 = 0o040_000;
const DIRECTORY: u32 = 0o040_755;
const EXECUTABLE: u32 = 0o100_755;
const CHARACTER_DEVICE: u32 = 0o020_600;
const CONSOLE_DEVICE: (u32, u32) = (5, 1);
const MOUNT_POINTS: [&str; 4] = ["dev", "proc", "sys", "mnt"];
const INIT: &str = "init";
const CONSOLE: &str = "dev/console";

/// The initramfs for the guest init at `guest`, kept in `images`.
pub(super) fn initramfs(images: &Path, guest: &Path) -> Result<PathBuf, SandboxError> {
    let init = fs::read(guest).map_err(|e| io_error(guest, e))?;
    fs::create_dir_all(images).map_err(|e| io_error(images, e))?;
    let path = images.join(format!("initramfs-{}.cpio", Digest::of_bytes(&init).hex()));
    if path.is_file() {
        return Ok(path);
    }
    let partial = path.with_extension("partial");
    fs::write(&partial, archive(&init)).map_err(|e| io_error(&partial, e))?;
    fs::rename(&partial, &path).map_err(|e| io_error(&path, e))?;
    Ok(path)
}

fn archive(init: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut inode = 1;
    let mut add = |name: &str, mode: u32, device: (u32, u32), data: &[u8]| {
        entry(&mut out, inode, name, mode, device, data);
        inode += 1;
    };
    for dir in MOUNT_POINTS {
        add(dir, DIRECTORY, (0, 0), &[]);
    }
    add(CONSOLE, CHARACTER_DEVICE, CONSOLE_DEVICE, &[]);
    add(INIT, EXECUTABLE, (0, 0), init);
    entry(&mut out, 0, TRAILER, 0, (0, 0), &[]);
    out
}

fn entry(out: &mut Vec<u8>, inode: u32, name: &str, mode: u32, device: (u32, u32), data: &[u8]) {
    let links = match mode & FILE_TYPE_BITS == DIRECTORY_TYPE {
        true => 2,
        false => 1,
    };
    let name_size = name.len() + 1;
    let fields = [inode, mode, 0, 0, links, 0, data.len() as u32, 0, 0, device.0, device.1, name_size as u32, 0];
    out.extend_from_slice(NEWC_MAGIC.as_bytes());
    for field in fields {
        out.extend_from_slice(format!("{field:08x}").as_bytes());
    }
    out.extend_from_slice(name.as_bytes());
    out.push(0);
    pad(out);
    out.extend_from_slice(data);
    pad(out);
}

fn pad(out: &mut Vec<u8>) {
    while !out.len().is_multiple_of(ALIGNMENT) {
        out.push(0);
    }
}

fn io_error(path: &Path, error: io::Error) -> SandboxError {
    SandboxError::Io { path: path.to_owned(), error }
}
