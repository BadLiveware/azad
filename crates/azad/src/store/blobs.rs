use std::fs::{self, File, Permissions};
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::ir::{Canonical, Digest, Hasher};
use tempfile::NamedTempFile;

use crate::store::{BLOBS_DIR, Store, StoreError};

const BLOB_MODE: u32 = 0o444;
const COPY_BUFFER: usize = 64 * 1024;

impl Store {
    pub fn blob_path(&self, digest: &Digest) -> PathBuf {
        let hex = digest.hex();
        self.root.join(BLOBS_DIR).join(digest.function().name()).join(&hex[..2]).join(hex)
    }

    pub fn contains(&self, digest: &Digest) -> bool {
        self.blob_path(digest).is_file()
    }

    pub fn put_bytes(&self, bytes: &[u8]) -> Result<Digest, StoreError> {
        let digest = Digest::of_bytes(bytes);
        if self.contains(&digest) {
            return Ok(digest);
        }
        let mut tmp = self.new_tmp_file()?;
        tmp.write_all(bytes).map_err(|e| StoreError::io(tmp.path(), e))?;
        self.publish(tmp, &digest)?;
        Ok(digest)
    }

    pub fn put_file(&self, path: &Path) -> Result<Digest, StoreError> {
        let source = File::open(path).map_err(|e| StoreError::io(path, e))?;
        self.put_reader(source, path)
    }

    /// Stores what `source` reads; `origin` names the source in errors.
    pub fn put_reader(&self, mut source: impl Read, origin: &Path) -> Result<Digest, StoreError> {
        let path = origin;
        let mut tmp = self.new_tmp_file()?;
        let mut hasher = Hasher::new();
        let mut buffer = vec![0u8; COPY_BUFFER];
        loop {
            let n = source.read(&mut buffer).map_err(|e| StoreError::io(path, e))?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
            tmp.write_all(&buffer[..n]).map_err(|e| StoreError::io(tmp.path(), e))?;
        }
        let digest = hasher.finish();
        if self.contains(&digest) {
            return Ok(digest);
        }
        self.publish(tmp, &digest)?;
        Ok(digest)
    }

    pub fn read(&self, digest: &Digest) -> Result<Vec<u8>, StoreError> {
        let path = self.blob_path(digest);
        let bytes = fs::read(&path).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => StoreError::Missing(*digest),
            _ => StoreError::io(&path, e),
        })?;
        let actual = Digest::of_bytes(&bytes);
        if actual != *digest {
            return Err(StoreError::Corrupt { expected: *digest, actual });
        }
        Ok(bytes)
    }

    pub fn put<T: Canonical>(&self, value: &T) -> Result<Digest, StoreError> {
        self.put_bytes(&value.encode())
    }

    pub fn get<T: Canonical>(&self, digest: &Digest) -> Result<T, StoreError> {
        let bytes = self.read(digest)?;
        T::decode(&bytes).map_err(|source| StoreError::Decode { digest: *digest, error: source })
    }

    fn new_tmp_file(&self) -> Result<NamedTempFile, StoreError> {
        let dir = self.tmp_dir();
        NamedTempFile::new_in(&dir).map_err(|e| StoreError::io(&dir, e))
    }

    fn publish(&self, tmp: NamedTempFile, digest: &Digest) -> Result<(), StoreError> {
        let target = self.blob_path(digest);
        let parent = target.parent().expect("a blob path has a parent");
        fs::create_dir_all(parent).map_err(|e| StoreError::io(parent, e))?;
        tmp.as_file().set_permissions(Permissions::from_mode(BLOB_MODE)).map_err(|e| StoreError::io(tmp.path(), e))?;
        match tmp.persist_noclobber(&target) {
            Ok(_) => Ok(()),
            Err(e) if e.error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
            Err(e) => Err(StoreError::io(&target, e.error)),
        }
    }
}
