//! Files of the workspace during evaluation (decision 0031): paths relative to a package or, after `//`, to the root;
//! the text of a file, which the package records with the digest of what was read; the files that patterns of the
//! workspace select; and the package that holds a path.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use crate::frontend::starlark::values::Collector;
use crate::frontend::starlark::yaml::TASKS_YAML_FILE;
use crate::frontend::starlark::{TASKS_FILE, package_at};
use crate::frontend::{NoteLevel, WORKSPACE_PATTERN_PREFIX, split_workspace_pattern};
use crate::ir::Digest;
use crate::store::Selection;

const CURRENT_DIR: &str = ".";
const PARENT_DIR: &str = "..";
const BYTE_ORDER_MARK: char = '\u{feff}';
const PACKAGE_FILES: [&str; 2] = ["//**/tasks.star", "//**/tasks.yaml"];
/// A name within a third of its length of another is taken as a typo of it.
const MAX_TYPO_SHARE: usize = 3;

/// A path relative to the directory `dir`, or to the workspace root after `//`, as a normalized path relative to the
/// root, "" for the root itself. An absolute path, and a path that leaves the workspace, are errors.
pub(crate) fn resolve(dir: &str, path: &str) -> Result<String, String> {
    let (base, rest) = match path.strip_prefix(WORKSPACE_PATTERN_PREFIX) {
        Some(rest) => ("", rest),
        None if path.starts_with('/') => {
            return Err(format!("{path:?} is absolute; give a path relative to the package, or to the root after //"));
        }
        None => (dir, path),
    };
    let mut parts: Vec<&str> = base.split('/').filter(|part| !part.is_empty()).collect();
    for part in rest.split('/') {
        match part {
            "" | CURRENT_DIR => {}
            PARENT_DIR => {
                if parts.pop().is_none() {
                    return Err(format!("{path:?} leaves the workspace"));
                }
            }
            part => parts.push(part),
        }
    }
    Ok(parts.join("/"))
}

/// A path relative to the root as the built-ins show it: `//src/App`, and `//` for the root.
pub(crate) fn workspace_label(relative: &str) -> String {
    format!("{WORKSPACE_PATTERN_PREFIX}{relative}")
}

/// Why `dir` is not a package, with the nearest package by name when one is near enough to be a typo of it.
pub(crate) fn not_a_package(root: &Path, dir: &str) -> String {
    let problem = format!("{} is not a package: it has no {TASKS_FILE} or {TASKS_YAML_FILE}", workspace_label(dir));
    let patterns = PACKAGE_FILES.map(str::to_owned);
    let packages = list(root, &patterns, &[]).unwrap_or_default();
    let dirs = packages
        .iter()
        .filter_map(|file| file.rsplit_once('/'))
        .map(|(dir, _)| dir.trim_start_matches(WORKSPACE_PATTERN_PREFIX));
    match nearest(dir, dirs) {
        Some(near) => format!("{problem}; did you mean {}?", workspace_label(near)),
        None => problem,
    }
}

/// The candidate nearest to `name` by edit distance, when it is at most a third of the name's length away.
fn nearest<'a>(name: &str, candidates: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    let most = (name.chars().count() / MAX_TYPO_SHARE).max(1);
    candidates
        .map(|candidate| (edit_distance(name, candidate), candidate))
        .filter(|(distance, _)| *distance <= most)
        .min()
        .map(|(_, candidate)| candidate)
}

/// The edit distance with transpositions (optimal string alignment): how many characters to insert, delete or
/// replace, or neighbours to swap, to make `a` into `b`.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut rows = vec![(0..=b.len()).collect::<Vec<usize>>()];
    for i in 1..=a.len() {
        let mut row = vec![i];
        for j in 1..=b.len() {
            let mut distance =
                (rows[i - 1][j] + 1).min(row[j - 1] + 1).min(rows[i - 1][j - 1] + usize::from(a[i - 1] != b[j - 1]));
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                distance = distance.min(rows[i - 2][j - 2] + 1);
            }
            row.push(distance);
        }
        rows.push(row);
    }
    rows[a.len()][b.len()]
}

/// The text of a file of the workspace, which the package records with the digest of its content.
pub(crate) fn read(collector: &Collector, path: &str) -> anyhow::Result<String> {
    let relative = resolve(&collector.dir, path).map_err(|e| anyhow::anyhow!("read(): {e}"))?;
    let shown = workspace_label(&relative);
    let file = collector.root.join(&relative);
    let bytes = match fs::read(&file) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => anyhow::bail!("read(): {shown} does not exist"),
        Err(error) => anyhow::bail!("read(): {shown}: {error}"),
    };
    if !stays_inside(&collector.root, &file) {
        anyhow::bail!("read(): {shown} is a link to a file outside the workspace");
    }
    let digest = Digest::of_bytes(&bytes);
    let text = String::from_utf8(bytes).map_err(|_| anyhow::anyhow!("read(): {shown} is not UTF-8 text"))?;
    collector.note(NoteLevel::Diagnostic, format!("read: {shown} is {digest}"));
    collector.defined.borrow_mut().read_files.insert(shown, digest);
    Ok(text.strip_prefix(BYTE_ORDER_MARK).map(str::to_owned).unwrap_or(text))
}

/// Whether `file`, with its links followed, is inside `root`.
fn stays_inside(root: &Path, file: &Path) -> bool {
    match (fs::canonicalize(root), fs::canonicalize(file)) {
        (Ok(root), Ok(file)) => file.starts_with(root),
        _ => false,
    }
}

/// The label of the package that holds a path relative to the root, `//dir`, or `//` for the root; `None` when no
/// package holds it.
pub(crate) fn package_label(root: &Path, relative: &str) -> Option<String> {
    package_at(root, Path::new(relative)).map(|package| workspace_label(&package))
}

/// The files that patterns of the workspace (`//libs/*/package.json`) select, as `//` paths, sorted.
///
/// The selection reads only the directories that can hold a match, so `//Directory.Build.props` reads the root and
/// not the workspace below it.
pub(crate) fn list(root: &Path, include: &[String], exclude: &[String]) -> anyhow::Result<Vec<String>> {
    let selection = Selection::globs(&from_root(include)?, &from_root(exclude)?)?;
    let listed = selection.list(root)?;
    Ok(listed.iter().filter_map(|path| path.to_str()).map(workspace_label).collect())
}

/// Patterns of the workspace without their `//`, relative to the root.
fn from_root(patterns: &[String]) -> anyhow::Result<Vec<String>> {
    patterns
        .iter()
        .map(|pattern| match split_workspace_pattern(pattern) {
            Some(Ok(_)) => Ok(pattern[WORKSPACE_PATTERN_PREFIX.len()..].to_owned()),
            Some(Err(problem)) => anyhow::bail!("files(): {problem}"),
            None => anyhow::bail!("files(): {pattern:?} does not start with //"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_is_resolved_against_its_package_or_the_root() {
        assert_eq!(resolve("apps/web", "package.json").as_deref(), Ok("apps/web/package.json"));
        assert_eq!(resolve("apps/web", "../../libs/./core/").as_deref(), Ok("libs/core"));
        assert_eq!(resolve("apps/web", "//libs/core/a.txt").as_deref(), Ok("libs/core/a.txt"));
        assert_eq!(resolve("", "//").as_deref(), Ok(""));
        assert_eq!(resolve("apps", "..").as_deref(), Ok(""));
    }

    #[test]
    fn a_path_that_is_absolute_or_leaves_the_workspace_is_refused() {
        assert!(resolve("apps", "/etc/passwd").unwrap_err().contains("is absolute"));
        assert!(resolve("apps", "../../outside").unwrap_err().contains("leaves the workspace"));
        assert!(resolve("", "//../outside").unwrap_err().contains("leaves the workspace"));
    }
}
