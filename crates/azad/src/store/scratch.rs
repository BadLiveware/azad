//! Scratch directories in the store's `tmp/`, which are removed when they are dropped, even when a tool made parts of
//! them read-only, as Go does with its module cache.

use std::fs::{self, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use tempfile::TempDir;

use crate::store::{Store, StoreError};

const WRITABLE_DIRECTORY: u32 = 0o755;

#[derive(Debug)]
pub struct ScratchDir {
    dir: Option<TempDir>,
}

impl ScratchDir {
    pub fn path(&self) -> &Path {
        self.dir.as_ref().expect("the directory exists until drop").path()
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let Some(dir) = self.dir.take() else { return };
        // A failure leaves garbage in tmp/, which a later clean-up can remove; it must not hide the result of a run.
        let _ = make_writable(dir.path());
        let _ = dir.close();
    }
}

impl Store {
    pub fn scratch_dir(&self) -> Result<ScratchDir, StoreError> {
        let tmp = self.tmp_dir();
        let dir = TempDir::new_in(&tmp).map_err(|e| StoreError::io(&tmp, e))?;
        Ok(ScratchDir { dir: Some(dir) })
    }
}

/// Makes every directory below `dir` writable, so that it can be removed.
pub(crate) fn make_writable(dir: &Path) -> Result<(), StoreError> {
    fs::set_permissions(dir, Permissions::from_mode(WRITABLE_DIRECTORY)).map_err(|e| StoreError::io(dir, e))?;
    for entry in fs::read_dir(dir).map_err(|e| StoreError::io(dir, e))? {
        let entry = entry.map_err(|e| StoreError::io(dir, e))?;
        let file_type = entry.file_type().map_err(|e| StoreError::io(dir, e))?;
        if file_type.is_dir() {
            make_writable(&entry.path())?;
        }
    }
    Ok(())
}
