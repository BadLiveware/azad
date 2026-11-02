use std::io;
use std::path::{Path, PathBuf};

use crate::ir::{CanonicalError, Digest};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{path}: {error}")]
    Io { path: PathBuf, error: io::Error },
    #[error("{0}: the name is not UTF-8")]
    NotUtf8(PathBuf),
    #[error("{0}: only files, directories and symbolic links can be stored")]
    UnsupportedFileType(PathBuf),
    #[error("{0} is not in the store")]
    Missing(Digest),
    #[error("{expected} is corrupt: its content has the digest {actual}")]
    Corrupt { expected: Digest, actual: Digest },
    #[error("{digest}: {error}")]
    Decode { digest: Digest, error: CanonicalError },
    #[error("{0}")]
    Invalid(String),
}

impl StoreError {
    pub(crate) fn io(path: &Path, error: io::Error) -> Self {
        StoreError::Io { path: path.to_owned(), error }
    }
}
