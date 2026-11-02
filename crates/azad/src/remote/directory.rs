use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tempfile::NamedTempFile;

use crate::remote::{Provider, RemoteError};

/// Objects as files in a directory, such as a shared disk or a mounted bucket.
#[derive(Debug, Clone)]
pub struct DirectoryProvider {
    dir: PathBuf,
}

impl DirectoryProvider {
    pub fn new(dir: PathBuf) -> Self {
        DirectoryProvider { dir }
    }
}

impl Provider for DirectoryProvider {
    fn name(&self) -> String {
        format!("dir:{}", self.dir.display())
    }

    fn contains(&self, key: &str) -> Result<bool, RemoteError> {
        Ok(self.dir.join(key).is_file())
    }

    fn get(&self, key: &str, dest: &Path) -> Result<bool, RemoteError> {
        let source = self.dir.join(key);
        match fs::copy(&source, dest) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(RemoteError::io(&source, e)),
        }
    }

    fn get_newest(&self, prefix: &str, dest: &Path) -> Result<bool, RemoteError> {
        let listing = match fs::read_dir(&self.dir) {
            Ok(listing) => listing,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(RemoteError::io(&self.dir, e)),
        };
        let mut names = Vec::new();
        for entry in listing {
            let name = entry.map_err(|e| RemoteError::io(&self.dir, e))?.file_name().to_string_lossy().into_owned();
            if name.starts_with(prefix) {
                names.push(name);
            }
        }
        match names.into_iter().max() {
            Some(newest) => self.get(&newest, dest),
            None => Ok(false),
        }
    }

    fn put(&self, key: &str, source: &Path) -> Result<(), RemoteError> {
        fs::create_dir_all(&self.dir).map_err(|e| RemoteError::io(&self.dir, e))?;
        let tmp = NamedTempFile::new_in(&self.dir).map_err(|e| RemoteError::io(&self.dir, e))?;
        fs::copy(source, tmp.path()).map_err(|e| RemoteError::io(source, e))?;
        let dest = self.dir.join(key);
        tmp.persist(&dest).map_err(|e| RemoteError::io(&dest, e.error))?;
        Ok(())
    }
}
