//! Finding files with GitHub's glob patterns, copying trees, and `hashFiles`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use globset::{GlobBuilder, GlobMatcher};
use sha2::{Digest, Sha256};

use crate::error::{Error, WithContext};

pub(crate) const GLOB_CHARACTERS: [char; 4] = ['*', '?', '[', '{'];
pub(crate) const EXCLUDE_PREFIX: char = '!';
const COMMENT_PREFIX: char = '#';
const OWNER_WRITE: u32 = 0o200;
const EXECUTABLE_BITS: u32 = 0o111;

/// One line of a pattern list: the directory to search, and the glob below it.
/// A pattern without glob characters names a path; it matches that path and everything below it.
#[derive(Debug)]
struct Pattern {
    root: PathBuf,
    glob: Option<GlobMatcher>,
}

impl Pattern {
    fn new(base: &Path, text: &str) -> Result<Self, Error> {
        let full = base.join(text);
        let mut root = PathBuf::new();
        let mut rest = PathBuf::new();
        for component in full.components() {
            let is_glob = component.as_os_str().to_string_lossy().contains(GLOB_CHARACTERS);
            match rest.as_os_str().is_empty() && !is_glob {
                true => root.push(component),
                false => rest.push(component),
            }
        }
        let glob = match rest.as_os_str().is_empty() {
            true => None,
            false => Some(compile_glob(&rest.to_string_lossy())?),
        };
        Ok(Self { root: normalize(&root), glob })
    }

    /// True when the path, or a directory above it, matches.
    fn matches(&self, path: &Path) -> bool {
        let Ok(relative) = path.strip_prefix(&self.root) else {
            return false;
        };
        let Some(glob) = &self.glob else {
            return true;
        };
        relative.ancestors().any(|candidate| !candidate.as_os_str().is_empty() && glob.is_match(candidate))
    }
}

fn compile_glob(text: &str) -> Result<GlobMatcher, Error> {
    let glob =
        GlobBuilder::new(text).literal_separator(true).build().with_context(|| format!("invalid glob `{text}`"))?;
    Ok(glob.compile_matcher())
}

/// Removes `.` and resolves `..` without reading the file system.
fn normalize(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            other => result.push(other),
        }
    }
    result
}

/// The lines of a pattern list, without blank lines and comments.
pub(crate) fn pattern_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with(COMMENT_PREFIX))
        .map(str::to_string)
        .collect()
}

/// The files that match the patterns (relative to `base`, or absolute), less the files of `!` patterns;
/// sorted, each one once.
pub(crate) fn find_files(base: &Path, patterns: &[String]) -> Result<Vec<PathBuf>, Error> {
    let mut includes = Vec::new();
    let mut excludes = Vec::new();
    for line in patterns {
        match line.strip_prefix(EXCLUDE_PREFIX) {
            Some(excluded) => excludes.push(Pattern::new(base, excluded.trim())?),
            None => includes.push(Pattern::new(base, line)?),
        }
    }
    let mut found = Vec::new();
    for include in &includes {
        for file in files_below(&include.root)? {
            let is_excluded = excludes.iter().any(|exclude| exclude.matches(&file));
            if include.matches(&file) && !is_excluded {
                found.push(file);
            }
        }
    }
    found.sort();
    found.dedup();
    Ok(found)
}

/// The path itself when it is a file; every file below it when it is a directory; nothing when it is missing.
/// Symbolic links to directories are not followed, so a loop of links cannot make the search endless.
fn files_below(path: &Path) -> Result<Vec<PathBuf>, Error> {
    let Ok(metadata) = fs::metadata(path) else {
        return Ok(Vec::new());
    };
    if !metadata.is_dir() {
        return Ok(vec![path.to_path_buf()]);
    }
    let mut files = Vec::new();
    let mut directories = vec![path.to_path_buf()];
    while let Some(directory) = directories.pop() {
        let entries = fs::read_dir(&directory).with_context(|| format!("cannot read {}", directory.display()))?;
        for entry in entries {
            let entry = entry.with_context(|| format!("cannot read {}", directory.display()))?;
            let entry_path = entry.path();
            match entry.file_type()?.is_dir() {
                true => directories.push(entry_path),
                false if entry_path.is_file() => files.push(entry_path),
                false => {}
            }
        }
    }
    Ok(files)
}

/// GitHub's `hashFiles`: the SHA-256 of the SHA-256 digests of the files below the workspace that match, in sorted
/// order, as hex; empty when no file matches.
pub(crate) fn hash_files(workspace: &Path, patterns: &[String]) -> Result<String, String> {
    let lines: Vec<String> = patterns.iter().flat_map(|pattern| pattern_lines(pattern)).collect();
    let files = find_files(workspace, &lines).map_err(|error| error.to_string())?;
    let below_workspace: Vec<&PathBuf> = files.iter().filter(|file| file.starts_with(workspace)).collect();
    if below_workspace.is_empty() {
        return Ok(String::new());
    }
    let mut total = Sha256::new();
    for file in below_workspace {
        let content = fs::read(file).map_err(|error| format!("cannot read {}: {error}", file.display()))?;
        total.update(Sha256::digest(&content));
    }
    Ok(to_hex(&total.finalize()))
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Copies a directory tree, or one file, to `target`.
/// Copies can be changed by the job, so each one is writable by its owner, also when the source is read-only.
pub(crate) fn copy_tree(source: &Path, target: &Path) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(source).with_context(|| format!("cannot read {}", source.display()))?;
    if metadata.is_symlink() {
        let link = fs::read_link(source)?;
        return std::os::unix::fs::symlink(&link, target)
            .with_context(|| format!("cannot create the link {}", target.display()));
    }
    if metadata.is_file() {
        return copy_file(source, target);
    }
    fs::create_dir_all(target).with_context(|| format!("cannot create {}", target.display()))?;
    make_owner_writable(target)?;
    let entries = fs::read_dir(source).with_context(|| format!("cannot read {}", source.display()))?;
    for entry in entries {
        let entry = entry?;
        copy_tree(&entry.path(), &target.join(entry.file_name()))?;
    }
    Ok(())
}

pub(crate) fn copy_file(source: &Path, target: &Path) -> Result<(), Error> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
    }
    fs::copy(source, target).with_context(|| format!("cannot copy {} to {}", source.display(), target.display()))?;
    make_owner_writable(target)
}

fn make_owner_writable(path: &Path) -> Result<(), Error> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(permissions.mode() | OWNER_WRITE);
    fs::set_permissions(path, permissions).with_context(|| format!("cannot make {} writable", path.display()))
}

/// The first executable file with this name in the directories of a `PATH` value.
pub(crate) fn find_program(name: &str, path_value: &str) -> Option<PathBuf> {
    if name.contains('/') {
        return Some(PathBuf::from(name));
    }
    std::env::split_paths(path_value).map(|directory| directory.join(name)).find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & EXECUTABLE_BITS != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(files: &[&str]) -> tempfile::TempDir {
        let root = tempfile::tempdir().expect("temp dir");
        for file in files {
            let path = root.path().join(file);
            fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
            fs::write(&path, file.as_bytes()).expect("write");
        }
        root
    }

    fn found(root: &Path, patterns: &[&str]) -> Vec<String> {
        let patterns: Vec<String> = patterns.iter().map(|pattern| pattern.to_string()).collect();
        find_files(root, &patterns)
            .expect("valid patterns")
            .iter()
            .map(|file| file.strip_prefix(root).expect("below root").to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn a_star_does_not_cross_directories_and_two_stars_do() {
        let root = tree(&["a.txt", "dir/b.txt", "dir/sub/c.txt"]);
        assert_eq!(found(root.path(), &["*.txt"]), vec!["a.txt"]);
        assert_eq!(found(root.path(), &["**/*.txt"]), vec!["a.txt", "dir/b.txt", "dir/sub/c.txt"]);
    }

    #[test]
    fn a_directory_matches_everything_below_it() {
        let root = tree(&["a.txt", "dir/b.txt", "dir/sub/c.txt"]);
        assert_eq!(found(root.path(), &["dir"]), vec!["dir/b.txt", "dir/sub/c.txt"]);
        assert_eq!(found(root.path(), &["d*"]), vec!["dir/b.txt", "dir/sub/c.txt"]);
    }

    #[test]
    fn exclusions_remove_files() {
        let root = tree(&["dir/b.txt", "dir/sub/c.txt", "dir/d.log"]);
        assert_eq!(found(root.path(), &["dir", "!dir/sub", "!**/*.log"]), vec!["dir/b.txt"]);
    }

    #[test]
    fn missing_paths_find_nothing() {
        let root = tree(&["a.txt"]);
        assert!(found(root.path(), &["missing", "missing/*.txt"]).is_empty());
    }

    #[test]
    fn hash_files_hashes_the_digests_of_the_files_in_sorted_order() {
        let root = tree(&["b.lock", "a.lock", "other.txt"]);
        let a_digest = Sha256::digest(b"a.lock");
        let b_digest = Sha256::digest(b"b.lock");
        let mut total = Sha256::new();
        total.update(a_digest);
        total.update(b_digest);
        let expected = to_hex(&total.finalize());
        assert_eq!(hash_files(root.path(), &["**/*.lock".to_string()]), Ok(expected));
    }

    #[test]
    fn hash_files_is_empty_when_nothing_matches() {
        let root = tree(&["a.txt"]);
        assert_eq!(hash_files(root.path(), &["*.lock".to_string()]), Ok(String::new()));
    }

    #[test]
    fn hash_files_ignores_files_outside_the_workspace() {
        let outside = tree(&["x.lock"]);
        let workspace = tree(&[]);
        let pattern = outside.path().join("*.lock").to_string_lossy().into_owned();
        assert_eq!(hash_files(workspace.path(), &[pattern]), Ok(String::new()));
    }

    #[test]
    fn copies_read_only_files_as_writable() {
        let source = tree(&["dir/a.txt"]);
        let file = source.path().join("dir/a.txt");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o444)).expect("chmod");
        let target = tempfile::tempdir().expect("temp dir");
        copy_tree(source.path(), &target.path().join("copy")).expect("copy");
        let copied = target.path().join("copy/dir/a.txt");
        assert_eq!(fs::read_to_string(&copied).expect("read"), "dir/a.txt");
        assert!(fs::metadata(&copied).expect("metadata").permissions().mode() & OWNER_WRITE != 0);
    }
}
