//! The disks of a run: the root of an action as a read-only EROFS (Enhanced Read-Only File System) image, made once
//! per root; a writable ext4 run disk for each run, with the work directory, `/tmp`, the read-only mounts and the
//! run's spec; and an ext4 disk for each cache volume, which lives as long as the volume.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;

use azad_guest::protocol::{MOUNTS_DIR, RunSpec, SPEC_FILE, TMP_DIR, VOLUME_DATA_DIR, WORK_DIR};

use crate::ir::Digest;
use crate::sandbox::{SandboxError, SandboxSpec};

const MKFS_EROFS: &str = "mkfs.erofs";
const MKFS_EXT4: &str = "mkfs.ext4";
const ROOTS_DIR: &str = "roots";
const EROFS_EXTENSION: &str = "erofs";
const LOCK_EXTENSION: &str = "lock";
const PARTIAL_EXTENSION: &str = "partial";
/// Cloud Hypervisor maps a persistent memory device in units of 2 MiB.
const PMEM_ALIGNMENT: u64 = 2 << 20;
const MEBIBYTE: u64 = 1 << 20;
const RUN_DISK_FACTOR: u64 = 2;

/// The read-only image of a root directory, made once and kept in `images`. Roots are trees of the store, which never
/// change, so the path of a root names its content.
pub(super) fn root_image(images: &Path, root: &Path) -> Result<PathBuf, SandboxError> {
    let key = Digest::of_bytes(root.as_os_str().as_encoded_bytes()).hex();
    let dir = images.join(ROOTS_DIR);
    fs::create_dir_all(&dir).map_err(|e| io_error(&dir, e))?;
    let image = dir.join(format!("{key}.{EROFS_EXTENSION}"));
    if image.is_file() {
        return Ok(image);
    }
    let lock_path = dir.join(format!("{key}.{LOCK_EXTENSION}"));
    let lock = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| io_error(&lock_path, e))?;
    lock.lock().map_err(|e| io_error(&lock_path, e))?;
    let made = make_root_image(root, &image);
    lock.unlock().ok();
    made.map(|()| image)
}

fn make_root_image(root: &Path, image: &Path) -> Result<(), SandboxError> {
    if image.is_file() {
        return Ok(());
    }
    let partial = image.with_extension(PARTIAL_EXTENSION);
    run_tool(Command::new(MKFS_EROFS).arg("--quiet").arg(&partial).arg(root))?;
    let size = fs::metadata(&partial).map_err(|e| io_error(&partial, e))?.len();
    let file = OpenOptions::new().write(true).open(&partial).map_err(|e| io_error(&partial, e))?;
    file.set_len(size.div_ceil(PMEM_ALIGNMENT) * PMEM_ALIGNMENT).map_err(|e| io_error(&partial, e))?;
    fs::rename(&partial, image).map_err(|e| io_error(image, e))
}

/// The run disk of `spec` in `dir`: its files are hard links to the files of the run where the file system allows, so
/// the disk is the only copy. It holds what the run starts with, and `room` for what the run writes; the file is
/// sparse, so room that the run does not use takes no space on the host.
pub(super) fn run_disk(
    spec: &SandboxSpec,
    dir: &Path,
    room: u64,
    resolv_conf: Option<String>,
) -> Result<PathBuf, SandboxError> {
    let staging = dir.join("run");
    fs::create_dir(&staging).map_err(|e| io_error(&staging, e))?;
    link_tree(&spec.work, &staging.join(WORK_DIR))?;
    link_tree(&spec.tmp, &staging.join(TMP_DIR))?;
    let mounts = staging.join(MOUNTS_DIR);
    fs::create_dir_all(&mounts).map_err(|e| io_error(&mounts, e))?;
    for (index, mount) in spec.mounts.iter().enumerate() {
        link_tree(&mount.host, &mounts.join(index.to_string()))?;
    }
    let owner = fs::metadata(&spec.work).map_err(|e| io_error(&spec.work, e))?;
    let run_spec = RunSpec {
        command: spec.command.clone(),
        env: spec.env.clone(),
        dir: spec.dir.clone(),
        mounts: spec.mounts.iter().map(|mount| mount.target.clone()).collect(),
        volumes: spec.volumes.iter().map(|volume| volume.target.clone()).collect(),
        resolv_conf,
        uid: owner.uid(),
        gid: owner.gid(),
        memory_bytes: spec.limits.memory_bytes,
        processes: spec.limits.processes,
    };
    let spec_path = staging.join(SPEC_FILE);
    let encoded = serde_json::to_vec(&run_spec).map_err(|e| SandboxError::Setup(e.to_string()))?;
    fs::write(&spec_path, encoded).map_err(|e| io_error(&spec_path, e))?;

    let disk = dir.join("run.ext4");
    let size = (tree_size(&staging)? * RUN_DISK_FACTOR + room).div_ceil(MEBIBYTE) * MEBIBYTE;
    File::create(&disk).and_then(|file| file.set_len(size)).map_err(|e| io_error(&disk, e))?;
    let root_owner = format!("root_owner={}:{}", owner.uid(), owner.gid());
    run_tool(
        Command::new(MKFS_EXT4)
            .args(["-q", "-F", "-O", "^has_journal", "-E", &root_owner, "-d"])
            .arg(&staging)
            .arg(&disk),
    )?;
    Ok(disk)
}

/// Makes the disk of a cache volume at `disk`, unless it exists: an ext4 file system with a journal, because the disk
/// lives longer than one run. The volume's files are in its directory [`VOLUME_DATA_DIR`], owned by the user, which
/// the guest mounts; so the command does not see the disk's `lost+found`.
pub(super) fn volume_disk(disk: &Path, size: u64) -> Result<(), SandboxError> {
    if disk.is_file() {
        return Ok(());
    }
    let dir = disk.parent().unwrap_or(disk);
    let staging = tempfile::TempDir::new_in(dir).map_err(|e| io_error(dir, e))?;
    let data = staging.path().join(VOLUME_DATA_DIR);
    fs::create_dir(&data).map_err(|e| io_error(&data, e))?;
    let partial = disk.with_extension(PARTIAL_EXTENSION);
    File::create(&partial).and_then(|file| file.set_len(size)).map_err(|e| io_error(&partial, e))?;
    run_tool(Command::new(MKFS_EXT4).args(["-q", "-F", "-d"]).arg(staging.path()).arg(&partial))?;
    fs::rename(&partial, disk).map_err(|e| io_error(disk, e))
}

/// A copy of the tree at `source` at `dest`: directories with their modes, files as hard links (or copies across file
/// systems), and symbolic links as they are.
fn link_tree(source: &Path, dest: &Path) -> Result<(), SandboxError> {
    let metadata = fs::symlink_metadata(source).map_err(|e| io_error(source, e))?;
    if metadata.is_symlink() {
        let target = fs::read_link(source).map_err(|e| io_error(source, e))?;
        return symlink(target, dest).map_err(|e| io_error(dest, e));
    }
    if metadata.is_file() {
        return fs::hard_link(source, dest)
            .or_else(|_| fs::copy(source, dest).map(|_| ()))
            .map_err(|e| io_error(dest, e));
    }
    fs::create_dir(dest).map_err(|e| io_error(dest, e))?;
    for entry in fs::read_dir(source).map_err(|e| io_error(source, e))? {
        let entry = entry.map_err(|e| io_error(source, e))?;
        link_tree(&entry.path(), &dest.join(entry.file_name()))?;
    }
    fs::set_permissions(dest, fs::Permissions::from_mode(metadata.permissions().mode())).map_err(|e| io_error(dest, e))
}

fn tree_size(dir: &Path) -> Result<u64, SandboxError> {
    let mut size = 0;
    for entry in fs::read_dir(dir).map_err(|e| io_error(dir, e))? {
        let entry = entry.map_err(|e| io_error(dir, e))?;
        let metadata = entry.metadata().map_err(|e| io_error(&entry.path(), e))?;
        size += match metadata.is_dir() {
            true => tree_size(&entry.path())?,
            false => metadata.len(),
        };
    }
    Ok(size)
}

fn run_tool(command: &mut Command) -> Result<(), SandboxError> {
    let program = command.get_program().to_string_lossy().into_owned();
    let output = command.output().map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => SandboxError::Setup(format!("{program} is not installed; the VM sandbox needs it")),
        _ => SandboxError::Spawn(e),
    })?;
    match output.status.success() {
        true => Ok(()),
        false => {
            Err(SandboxError::Setup(format!("{program} failed: {}", String::from_utf8_lossy(&output.stderr).trim())))
        }
    }
}

fn io_error(path: &Path, error: io::Error) -> SandboxError {
    SandboxError::Io { path: path.to_owned(), error }
}
