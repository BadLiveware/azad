use std::fs::{self, File, OpenOptions, Permissions};
use std::io;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::ir::{Digest, Directory, Node};
use tempfile::TempDir;

use crate::store::{Store, StoreError, TREES_DIR};

const FILE_MODE: u32 = 0o444;
const EXECUTABLE_MODE: u32 = 0o555;
const WRITABLE_FILE_MODE: u32 = 0o644;
const WRITABLE_EXECUTABLE_MODE: u32 = 0o755;
/// The lock file of a shared tree, next to its directory.
const LOCK_EXTENSION: &str = ".lock";

/// The shared directory of a tree, and whether this call made it; a call that found it, or waited while another call
/// made it, did not.
#[derive(Debug)]
pub struct SharedTree {
    pub path: PathBuf,
    pub made: bool,
}

impl Store {
    /// Returns a directory with the content of a tree, as `share` does.
    pub fn materialize(&self, tree: &Digest) -> Result<PathBuf, StoreError> {
        self.share(tree).map(|shared| shared.path)
    }

    /// Returns a directory with the content of a tree. The directory is made once per tree and shared; callers must
    /// not change it, and a sandbox mounts it read-only. One run makes it while the others that need it wait, so actions
    /// that start at the same time with a new image do not each copy the image.
    pub fn share(&self, tree: &Digest) -> Result<SharedTree, StoreError> {
        let target = self.root.join(TREES_DIR).join(tree.function().name()).join(tree.hex());
        if target.is_dir() {
            return Ok(SharedTree { path: target, made: false });
        }
        let parent = target.parent().expect("a tree path has a parent");
        fs::create_dir_all(parent).map_err(|e| StoreError::io(parent, e))?;
        let lock_path = parent.join(format!("{}{LOCK_EXTENSION}", tree.hex()));
        let lock = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|e| StoreError::io(&lock_path, e))?;
        lock.lock().map_err(|e| StoreError::io(&lock_path, e))?;
        let made = self.make_shared_tree(tree, target);
        // A process that Azad starts for another action has a copy of the open lock file until it runs its program,
        // so the lock is freed explicitly (see `CacheVolume`).
        lock.unlock().ok();
        made
    }

    fn make_shared_tree(&self, tree: &Digest, target: PathBuf) -> Result<SharedTree, StoreError> {
        if target.is_dir() {
            return Ok(SharedTree { path: target, made: false });
        }
        let tmp_root = self.tmp_dir();
        let staging = TempDir::new_in(&tmp_root).map_err(|e| StoreError::io(&tmp_root, e))?;
        let built = staging.path().join("tree");
        self.write_tree(tree, &built, Placement::LinkWhenShared)?;
        match fs::rename(&built, &target) {
            Ok(()) => Ok(SharedTree { path: target, made: true }),
            Err(_) if target.is_dir() => Ok(SharedTree { path: target, made: false }),
            Err(e) => Err(StoreError::io(&target, e)),
        }
    }

    /// Writes a copy of the content of a tree into `dest`, which must not exist.
    pub fn materialize_into(&self, tree: &Digest, dest: &Path) -> Result<(), StoreError> {
        self.write_tree(tree, dest, Placement::Copy)
    }

    /// Writes a tree into `dest` as files that the caller may change, next to what `dest` holds already; a file of the
    /// tree takes the place of a file at the same path.
    pub fn merge_into(&self, tree: &Digest, dest: &Path) -> Result<(), StoreError> {
        let directory: Directory = self.get(tree)?;
        fs::create_dir_all(dest).map_err(|e| StoreError::io(dest, e))?;
        for (name, node) in directory.entries() {
            let path = dest.join(name);
            match node {
                Node::Directory { digest } => self.merge_into(digest, &path)?,
                Node::File { digest, executable } => {
                    remove_file_if_present(&path)?;
                    self.place_blob(digest, *executable, &path, Placement::Copy)?;
                    let mode = match executable {
                        true => WRITABLE_EXECUTABLE_MODE,
                        false => WRITABLE_FILE_MODE,
                    };
                    fs::set_permissions(&path, Permissions::from_mode(mode)).map_err(|e| StoreError::io(&path, e))?;
                }
                Node::Symlink { target } => {
                    remove_file_if_present(&path)?;
                    symlink(target, &path).map_err(|e| StoreError::io(&path, e))?;
                }
            }
        }
        Ok(())
    }

    pub(super) fn write_tree(&self, tree: &Digest, dest: &Path, placement: Placement) -> Result<(), StoreError> {
        let directory: Directory = self.get(tree)?;
        fs::create_dir(dest).map_err(|e| StoreError::io(dest, e))?;
        for (name, node) in directory.entries() {
            self.write_node(node, &dest.join(name), placement)?;
        }
        Ok(())
    }

    pub(super) fn write_node(&self, node: &Node, path: &Path, placement: Placement) -> Result<(), StoreError> {
        match node {
            Node::Directory { digest } => self.write_tree(digest, path, placement),
            Node::File { digest, executable } => self.place_blob(digest, *executable, path, placement),
            Node::Symlink { target } => symlink(target, path).map_err(|e| StoreError::io(path, e)),
        }
    }

    /// A file of a tree. In the store's own trees, a file that is not executable is a hard link to its blob: both are
    /// read-only and the sandbox mounts trees read-only, so an image is not stored twice. A copy is the fallback, and
    /// the only way for executable files, whose mode differs from the blob's.
    fn place_blob(
        &self,
        digest: &Digest,
        executable: bool,
        dest: &Path,
        placement: Placement,
    ) -> Result<(), StoreError> {
        let source = self.blob_path(digest);
        let may_link = matches!(placement, Placement::LinkWhenShared) && !executable;
        if may_link && fs::hard_link(&source, dest).is_ok() {
            return Ok(());
        }
        fs::copy(&source, dest).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => StoreError::Missing(*digest),
            _ => StoreError::io(&source, e),
        })?;
        // On macOS, `fs::copy` clones the blob with its old modification time; a file that a checkout writes must be
        // newer than any build that saw other content at its path (decision 0011).
        File::open(dest).and_then(|file| file.set_modified(SystemTime::now())).map_err(|e| StoreError::io(dest, e))?;
        fs::set_permissions(dest, Permissions::from_mode(file_mode(executable))).map_err(|e| StoreError::io(dest, e))
    }
}

fn remove_file_if_present(path: &Path) -> Result<(), StoreError> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(StoreError::io(path, e)),
        _ => Ok(()),
    }
}

fn file_mode(executable: bool) -> u32 {
    match executable {
        true => EXECUTABLE_MODE,
        false => FILE_MODE,
    }
}

#[derive(Clone, Copy)]
pub(super) enum Placement {
    /// For the store's own trees, which nothing changes.
    LinkWhenShared,
    /// For directories that the caller owns.
    Copy,
}
