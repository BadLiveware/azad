//! `actions/upload-artifact` and `actions/download-artifact`: artifacts are directories in the work directory's
//! artifacts output, and in the artifacts of the jobs in `needs`.

use std::fs;
use std::path::{Path, PathBuf};

use crate::plan::paths;
use globset::GlobMatcher;

use crate::error::{Error, WithContext};
use crate::files::{EXCLUDE_PREFIX, GLOB_CHARACTERS, copy_file, copy_tree, find_files, pattern_lines};

pub(crate) const DEFAULT_ARTIFACT_NAME: &str = "artifact";
const FORBIDDEN_NAME_CHARACTERS: [char; 11] = ['"', ':', '<', '>', '|', '*', '?', '\r', '\n', '\\', '/'];
const PARENT_DIRECTORY: &str = "..";

/// What `if-no-files-found` asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IfNoFilesFound {
    Warn,
    Error,
    Ignore,
}

impl IfNoFilesFound {
    pub(crate) fn parse(text: Option<&str>) -> Result<Self, Error> {
        match text.map(str::trim) {
            None | Some("") | Some("warn") => Ok(Self::Warn),
            Some("error") => Ok(Self::Error),
            Some("ignore") => Ok(Self::Ignore),
            Some(other) => {
                Err(Error::new(format!("`if-no-files-found: {other}` is not known: use warn, error or ignore")))
            }
        }
    }
}

/// The result of an upload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Uploaded {
    Files(usize),
    NothingFound,
}

/// Refuses names that GitHub refuses, which also keeps each artifact in its own directory.
pub(crate) fn check_name(name: &str) -> Result<(), Error> {
    let is_valid = !name.is_empty() && name != PARENT_DIRECTORY && !name.contains(FORBIDDEN_NAME_CHARACTERS);
    match is_valid {
        true => Ok(()),
        false => Err(Error::new(format!(
            "`{name}` is not a valid artifact name: it must not be empty, `..`, or hold any of {FORBIDDEN_NAME_CHARACTERS:?}"
        ))),
    }
}

/// Copies the files that `path` names into `<artifacts>/<name>/`.
pub(crate) fn upload(workspace: &Path, artifacts: &Path, name: &str, path: &str) -> Result<Uploaded, Error> {
    check_name(name)?;
    let lines = pattern_lines(path);
    let files = find_files(workspace, &lines)?;
    if files.is_empty() {
        return Ok(Uploaded::NothingFound);
    }
    let target = artifacts.join(name);
    if target.exists() {
        return Err(Error::new(format!("an artifact named `{name}` is already uploaded by this job")));
    }
    let root = upload_root(single_directory(workspace, &lines).as_deref(), &files);
    for file in &files {
        let relative = file.strip_prefix(&root).unwrap_or(file);
        copy_file(file, &target.join(relative))?;
    }
    Ok(Uploaded::Files(files.len()))
}

/// The directory when `path` is one directory, which gives its content.
fn single_directory(workspace: &Path, lines: &[String]) -> Option<PathBuf> {
    let includes: Vec<&String> = lines.iter().filter(|line| !line.starts_with(EXCLUDE_PREFIX)).collect();
    let [only] = includes.as_slice() else {
        return None;
    };
    let directory = workspace.join(only);
    let is_literal_directory = !only.contains(GLOB_CHARACTERS) && directory.is_dir();
    match is_literal_directory {
        true => Some(directory),
        false => None,
    }
}

/// The directory that the artifact's paths are relative to: the one directory that `path` names, else the deepest
/// directory that holds all the files.
pub(crate) fn upload_root(single_directory: Option<&Path>, files: &[PathBuf]) -> PathBuf {
    if let Some(directory) = single_directory {
        return directory.to_path_buf();
    }
    let mut parents = files.iter().map(|file| file.parent().unwrap_or(file));
    let Some(first) = parents.next() else {
        return PathBuf::new();
    };
    parents.fold(first.to_path_buf(), |common, parent| common_ancestor(&common, parent))
}

fn common_ancestor(left: &Path, right: &Path) -> PathBuf {
    left.components().zip(right.components()).take_while(|(a, b)| a == b).map(|(a, _)| a).collect()
}

/// An artifact that a job in `needs` uploaded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Available {
    pub(crate) name: String,
    pub(crate) directory: PathBuf,
}

/// The artifacts in `<needs>/<job>/artifacts/<name>`, sorted by job and name.
pub(crate) fn available(needs_dir: &Path) -> Result<Vec<Available>, Error> {
    let mut found = Vec::new();
    for job in sorted_directories(needs_dir)? {
        for directory in sorted_directories(&job.join(paths::ARTIFACTS_OUTPUT))? {
            let name = directory.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
            found.push(Available { name, directory });
        }
    }
    Ok(found)
}

fn sorted_directories(parent: &Path) -> Result<Vec<PathBuf>, Error> {
    if !parent.is_dir() {
        return Ok(Vec::new());
    }
    let mut directories = Vec::new();
    for entry in fs::read_dir(parent).with_context(|| format!("cannot read {}", parent.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            directories.push(path);
        }
    }
    directories.sort();
    Ok(directories)
}

/// What `actions/download-artifact` asks for.
#[derive(Debug)]
pub(crate) struct Download {
    pub(crate) name: Option<String>,
    pub(crate) pattern: Option<GlobMatcher>,
    pub(crate) merge_multiple: bool,
    pub(crate) target: PathBuf,
}

/// Copies the artifacts and gives their names.
/// With a name, the artifact's content goes to the target; without one, each artifact goes to `<target>/<name>`,
/// or into the target itself when `merge-multiple` is true.
pub(crate) fn download(needs_dir: &Path, request: &Download) -> Result<Vec<String>, Error> {
    let all = available(needs_dir)?;
    if let Some(name) = &request.name {
        let Some(artifact) = all.iter().find(|artifact| &artifact.name == name) else {
            return Err(Error::new(format!(
                "no artifact named `{name}` in the jobs in `needs`; there are: {:?}",
                all.iter().map(|artifact| artifact.name.as_str()).collect::<Vec<_>>()
            )));
        };
        copy_tree(&artifact.directory, &request.target)?;
        return Ok(vec![name.clone()]);
    }
    let mut names = Vec::new();
    for artifact in all.iter().filter(|artifact| name_matches(request.pattern.as_ref(), &artifact.name)) {
        let target = match request.merge_multiple {
            true => request.target.clone(),
            false => request.target.join(&artifact.name),
        };
        copy_tree(&artifact.directory, &target)?;
        names.push(artifact.name.clone());
    }
    Ok(names)
}

fn name_matches(pattern: Option<&GlobMatcher>, name: &str) -> bool {
    pattern.is_none_or(|pattern| pattern.is_match(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(files: &[&str]) -> Vec<PathBuf> {
        files.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn one_directory_gives_its_content() {
        let files = paths(&["/w/dist/sub/a.js"]);
        assert_eq!(upload_root(Some(Path::new("/w/dist")), &files), PathBuf::from("/w/dist"));
    }

    #[test]
    fn one_file_keeps_only_its_name() {
        assert_eq!(upload_root(None, &paths(&["/w/out/report.txt"])), PathBuf::from("/w/out"));
    }

    #[test]
    fn files_keep_their_paths_below_the_deepest_common_directory() {
        let files = paths(&["/w/out/a/x.txt", "/w/out/b/c/y.txt"]);
        assert_eq!(upload_root(None, &files), PathBuf::from("/w/out"));
    }

    #[test]
    fn a_common_name_prefix_is_not_a_common_directory() {
        let files = paths(&["/w/build/x", "/w/builds/y"]);
        assert_eq!(upload_root(None, &files), PathBuf::from("/w"));
    }

    #[test]
    fn uploads_with_the_path_rules() {
        let workspace = tempfile::tempdir().expect("temp dir");
        let artifacts = tempfile::tempdir().expect("temp dir");
        for file in ["dist/a.js", "dist/sub/b.js", "docs/c.md"] {
            let path = workspace.path().join(file);
            fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
            fs::write(path, file).expect("write");
        }
        let uploaded = upload(workspace.path(), artifacts.path(), "web", "dist\n").expect("upload");
        assert_eq!(uploaded, Uploaded::Files(2));
        assert!(artifacts.path().join("web/a.js").is_file());
        assert!(artifacts.path().join("web/sub/b.js").is_file());

        upload(workspace.path(), artifacts.path(), "mixed", "dist/a.js\ndocs/*.md").expect("upload");
        assert!(artifacts.path().join("mixed/dist/a.js").is_file());
        assert!(artifacts.path().join("mixed/docs/c.md").is_file());

        let nothing = upload(workspace.path(), artifacts.path(), "none", "missing/*").expect("upload");
        assert_eq!(nothing, Uploaded::NothingFound);
        assert!(!artifacts.path().join("none").exists());
    }

    #[test]
    fn refuses_names_that_leave_the_artifacts_directory() {
        for name in ["", "..", "a/b", "a\\b", "a:b"] {
            assert!(check_name(name).is_err(), "{name}");
        }
        assert!(check_name("build-output_1.2").is_ok());
    }

    #[test]
    fn reads_if_no_files_found() {
        assert_eq!(IfNoFilesFound::parse(None).expect("default"), IfNoFilesFound::Warn);
        assert_eq!(IfNoFilesFound::parse(Some("error")).expect("known"), IfNoFilesFound::Error);
        assert_eq!(IfNoFilesFound::parse(Some("ignore")).expect("known"), IfNoFilesFound::Ignore);
        assert!(IfNoFilesFound::parse(Some("fail")).is_err());
    }
}
