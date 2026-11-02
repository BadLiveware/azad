//! A tree as one object: a gzipped tar with every blob of the tree, its directories included, named by digest. A
//! remote cache stores a tree as one object, because a provider such as the GHA cache has a high cost per object.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;

use crate::ir::{Digest, Directory, Node};
use crate::remote::RemoteError;
use crate::store::{Store, StoreError};

const BLOB_MODE: u32 = 0o444;

/// Every blob of a tree: the directories and the files.
fn blobs_of(store: &Store, tree: &Digest, found: &mut BTreeSet<Digest>) -> Result<(), StoreError> {
    if !found.insert(*tree) {
        return Ok(());
    }
    let directory: Directory = store.get(tree)?;
    for node in directory.entries().values() {
        match node {
            Node::Directory { digest } => blobs_of(store, digest, found)?,
            Node::File { digest, .. } => {
                found.insert(*digest);
            }
            Node::Symlink { .. } => {}
        }
    }
    Ok(())
}

/// Whether the store has every blob of a tree.
pub(crate) fn is_complete(store: &Store, tree: &Digest) -> bool {
    let mut found = BTreeSet::new();
    blobs_of(store, tree, &mut found).is_ok() && found.iter().all(|digest| store.contains(digest))
}

/// Writes the pack of a tree to `dest`.
pub(crate) fn write(store: &Store, tree: &Digest, dest: &Path) -> Result<(), RemoteError> {
    let mut blobs = BTreeSet::new();
    blobs_of(store, tree, &mut blobs)?;
    let file = File::create(dest).map_err(|e| RemoteError::io(dest, e))?;
    let mut archive = tar::Builder::new(GzEncoder::new(BufWriter::new(file), Compression::fast()));
    for digest in blobs {
        let path = store.blob_path(&digest);
        let mut header = tar::Header::new_gnu();
        header.set_size(digest.size());
        header.set_mode(BLOB_MODE);
        header.set_cksum();
        let content = File::open(&path).map_err(|e| RemoteError::io(&path, e))?;
        archive.append_data(&mut header, digest.hex(), content).map_err(|e| RemoteError::io(dest, e))?;
    }
    let mut out =
        archive.into_inner().map_err(|e| RemoteError::io(dest, e))?.finish().map_err(|e| RemoteError::io(dest, e))?;
    out.flush().map_err(|e| RemoteError::io(dest, e))
}

/// Puts the blobs of a pack into the store, and checks that it holds all of `tree`. Each blob must have the digest
/// that names it, so a pack from untrusted storage cannot put wrong content under a name.
pub(crate) fn read(store: &Store, tree: &Digest, pack: &Path) -> Result<(), RemoteError> {
    let file = File::open(pack).map_err(|e| RemoteError::io(pack, e))?;
    let mut archive = tar::Archive::new(GzDecoder::new(file));
    for entry in archive.entries().map_err(|e| RemoteError::io(pack, e))? {
        let entry = entry.map_err(|e| RemoteError::io(pack, e))?;
        let name = entry.path().map_err(|e| RemoteError::io(pack, e))?.to_string_lossy().into_owned();
        let digest = store.put_reader(entry, pack)?;
        if digest.hex() != name {
            return Err(RemoteError::Entry(format!("the pack of {tree} holds {digest} under the name {name}")));
        }
    }
    match is_complete(store, tree) {
        true => Ok(()),
        false => Err(RemoteError::Entry(format!("the pack of {tree} does not hold all of the tree"))),
    }
}
