//! Applying image layers to a directory: whiteouts, links, and paths that cannot leave the directory.

use std::fs::{self, File};
use std::io::{self, BufReader, Read};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Component, Path, PathBuf};

use flate2::read::GzDecoder;
use tar::{Archive, EntryType};

use crate::oci::PullError;

const GZIP_SUFFIX: &str = "+gzip";
/// Docker's own media type of a compressed layer, which registries such as Microsoft's still serve.
const DOCKER_GZIP_LAYER: &str = "application/vnd.docker.image.rootfs.diff.tar.gzip";
const TAR_SUFFIX: &str = ".tar";
const WHITEOUT_PREFIX: &str = ".wh.";
const OPAQUE_WHITEOUT: &str = ".wh..wh..opq";
const MAX_LINK_HOPS: usize = 40;
const EXECUTABLE_BITS: u32 = 0o111;
const FILE_MODE: u32 = 0o644;
const EXECUTABLE_MODE: u32 = 0o755;
const DIRECTORY_MODE: u32 = 0o755;

/// Applies one layer blob of the given media type to `root`.
pub(crate) fn apply(blob: &Path, media_type: &str, root: &Path) -> Result<(), PullError> {
    let io_error = |source| PullError::Io { path: blob.to_owned(), error: source };
    let file = BufReader::new(File::open(blob).map_err(io_error)?);
    let reader: Box<dyn Read> = match media_type {
        m if m.ends_with(GZIP_SUFFIX) || m == DOCKER_GZIP_LAYER => Box::new(GzDecoder::new(file)),
        m if m.ends_with(TAR_SUFFIX) => Box::new(file),
        other => {
            return Err(PullError::Format(format!("layer media type {other:?} is not supported")));
        }
    };
    let mut archive = Archive::new(reader);
    for entry in archive.entries().map_err(io_error)? {
        let mut entry = entry.map_err(io_error)?;
        let path = entry.path().map_err(io_error)?.into_owned();
        let Some(relative) = normalize(&path) else { continue };
        apply_entry(&mut entry, &relative, root)?;
    }
    Ok(())
}

fn apply_entry(entry: &mut tar::Entry<'_, impl Read>, relative: &Path, root: &Path) -> Result<(), PullError> {
    let name = relative.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_owned();
    let parent = resolve_parent(root, relative)?;
    let target = parent.join(&name);
    let io_error = |source| PullError::Io { path: target.clone(), error: source };
    if name == OPAQUE_WHITEOUT {
        return clear_directory(&parent);
    }
    if let Some(hidden) = name.strip_prefix(WHITEOUT_PREFIX) {
        return remove_any(&parent.join(hidden));
    }
    match entry.header().entry_type() {
        EntryType::Directory => {
            if !fs::symlink_metadata(&target).is_ok_and(|m| m.is_dir()) {
                remove_any(&target)?;
                fs::create_dir_all(&target).map_err(io_error)?;
            }
            fs::set_permissions(&target, fs::Permissions::from_mode(DIRECTORY_MODE)).map_err(io_error)
        }
        EntryType::Regular | EntryType::Continuous => {
            remove_any(&target)?;
            fs::create_dir_all(&parent).map_err(io_error)?;
            let mut file = File::create(&target).map_err(io_error)?;
            io::copy(entry, &mut file).map_err(io_error)?;
            let mode = entry.header().mode().map_err(io_error)?;
            fs::set_permissions(&target, fs::Permissions::from_mode(file_mode(mode))).map_err(io_error)
        }
        EntryType::Symlink => {
            let link = entry
                .link_name()
                .map_err(io_error)?
                .ok_or_else(|| PullError::Format("a symlink without a target".to_owned()))?;
            remove_any(&target)?;
            fs::create_dir_all(&parent).map_err(io_error)?;
            symlink(link.as_ref(), &target).map_err(io_error)
        }
        EntryType::Link => {
            let link = entry
                .link_name()
                .map_err(io_error)?
                .ok_or_else(|| PullError::Format("a hard link without a target".to_owned()))?;
            let Some(source_relative) = normalize(&link) else {
                return Ok(());
            };
            let source = resolve_parent(root, &source_relative)?.join(source_relative.file_name().unwrap_or_default());
            remove_any(&target)?;
            fs::create_dir_all(&parent).map_err(io_error)?;
            fs::copy(&source, &target)
                .map(|_| ())
                .map_err(|source_error| PullError::Io { path: source, error: source_error })
        }
        _ => Ok(()),
    }
}

fn file_mode(mode: u32) -> u32 {
    match mode & EXECUTABLE_BITS != 0 {
        true => EXECUTABLE_MODE,
        false => FILE_MODE,
    }
}

/// A relative path without `.` and `..`, or `None` for the root itself and for paths that try to climb out.
fn normalize(path: &Path) -> Option<PathBuf> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(part),
            Component::CurDir | Component::RootDir => {}
            Component::ParentDir | Component::Prefix(_) => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.iter().collect())
}

/// The host directory that holds `relative`, with symbolic links in its parents resolved as if `root` were `/`, so a
/// link in a layer can never lead outside `root`.
fn resolve_parent(root: &Path, relative: &Path) -> Result<PathBuf, PullError> {
    let parents: Vec<_> = relative.parent().map(|p| p.components().collect()).unwrap_or_default();
    let mut resolved: Vec<std::ffi::OsString> = Vec::new();
    let mut pending: Vec<std::ffi::OsString> = parents.iter().rev().map(|c| c.as_os_str().to_owned()).collect();
    let mut hops = 0;
    while let Some(part) = pending.pop() {
        match part.to_str() {
            Some("." | "") => continue,
            Some("..") => {
                resolved.pop();
                continue;
            }
            _ => {}
        }
        let candidate: PathBuf = root.join(resolved.iter().collect::<PathBuf>()).join(&part);
        let Ok(target) = fs::read_link(&candidate) else {
            resolved.push(part);
            continue;
        };
        hops += 1;
        if hops > MAX_LINK_HOPS {
            return Err(PullError::Format(format!("too many symbolic links under {}", relative.display())));
        }
        if target.is_absolute() {
            resolved.clear();
        }
        pending.extend(target.components().rev().filter_map(|c| match c {
            Component::RootDir => None,
            other => Some(other.as_os_str().to_owned()),
        }));
    }
    Ok(root.join(resolved.iter().collect::<PathBuf>()))
}

fn remove_any(path: &Path) -> Result<(), PullError> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    let result = match metadata.is_dir() {
        true => fs::remove_dir_all(path),
        false => fs::remove_file(path),
    };
    result.map_err(|source| PullError::Io { path: path.to_owned(), error: source })
}

fn clear_directory(dir: &Path) -> Result<(), PullError> {
    let Ok(listing) = fs::read_dir(dir) else { return Ok(()) };
    for entry in listing {
        let entry = entry.map_err(|source| PullError::Io { path: dir.to_owned(), error: source })?;
        remove_any(&entry.path())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn layer(dir: &Path, name: &str, build: impl FnOnce(&mut tar::Builder<File>)) -> PathBuf {
        let path = dir.join(name);
        let mut builder = tar::Builder::new(File::create(&path).unwrap());
        build(&mut builder);
        builder.finish().unwrap();
        path
    }

    fn add_file(builder: &mut tar::Builder<File>, path: &str, content: &[u8], mode: u32) {
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(mode);
        header.set_entry_type(EntryType::Regular);
        builder.append_data(&mut header, path, content).unwrap();
    }

    fn add_link(builder: &mut tar::Builder<File>, path: &str, target: &str, kind: EntryType) {
        let mut header = tar::Header::new_gnu();
        header.set_size(0);
        header.set_entry_type(kind);
        builder.append_link(&mut header, path, target).unwrap();
    }

    #[test]
    fn a_layer_in_dockers_own_compressed_media_type_applies() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("root");
        fs::create_dir(&root).unwrap();
        let tar = layer(dir.path(), "1.tar", |b| add_file(b, "etc/hostname", b"azad", 0o644));
        let compressed = dir.path().join("1.tar.gz");
        let mut encoder =
            flate2::write::GzEncoder::new(File::create(&compressed).unwrap(), flate2::Compression::fast());
        io::copy(&mut File::open(&tar).unwrap(), &mut encoder).unwrap();
        encoder.finish().unwrap();
        apply(&compressed, DOCKER_GZIP_LAYER, &root).unwrap();
        assert_eq!(fs::read_to_string(root.join("etc/hostname")).unwrap(), "azad");
    }

    #[test]
    fn layers_apply_in_order_with_whiteouts() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("root");
        fs::create_dir(&root).unwrap();
        let first = layer(dir.path(), "1.tar", |b| {
            add_file(b, "etc/keep", b"keep", 0o644);
            add_file(b, "etc/drop", b"drop", 0o644);
            add_file(b, "opt/old/a", b"a", 0o644);
            add_file(b, "bin/tool", b"#!", 0o755);
        });
        let second = layer(dir.path(), "2.tar", |b| {
            add_file(b, "etc/.wh.drop", b"", 0o644);
            add_file(b, "opt/old/.wh..wh..opq", b"", 0o644);
            add_file(b, "opt/old/b", b"b", 0o644);
            add_link(b, "bin/same", "bin/tool", EntryType::Link);
        });
        apply(&first, "application/vnd.oci.image.layer.v1.tar", &root).unwrap();
        apply(&second, "application/vnd.oci.image.layer.v1.tar", &root).unwrap();
        assert!(root.join("etc/keep").is_file());
        assert!(!root.join("etc/drop").exists());
        assert!(!root.join("opt/old/a").exists());
        assert!(root.join("opt/old/b").is_file());
        assert_eq!(fs::read(root.join("bin/same")).unwrap(), b"#!");
        assert_eq!(fs::metadata(root.join("bin/tool")).unwrap().permissions().mode() & 0o777, 0o755);
    }

    #[test]
    fn a_symlink_in_a_layer_cannot_lead_outside_the_root() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("root");
        let outside = dir.path().join("outside");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let evil = layer(dir.path(), "evil.tar", |b| {
            add_link(b, "escape", outside.to_str().unwrap(), EntryType::Symlink);
            add_link(b, "up", "../../..", EntryType::Symlink);
            add_file(b, "escape/planted", b"x", 0o644);
            add_file(b, "up/planted2", b"x", 0o644);
        });
        apply(&evil, "application/vnd.oci.image.layer.v1.tar", &root).unwrap();
        assert!(!outside.join("planted").exists());
        assert!(!dir.path().join("planted2").exists());
        assert!(root.join(outside.strip_prefix("/").unwrap()).join("planted").exists());
        assert!(root.join("planted2").exists());
    }
}
