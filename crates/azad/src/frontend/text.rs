use std::fs;
use std::path::{Path, PathBuf};

use crate::frontend::{Frontend, FrontendError, LoadRequest, Loaded, Package, SourceSpec};
use crate::ir::text::{self, SourceSpec as TextSource};
use crate::ir::{Source, Spec};

/// The extension of a graph in the text form of the IR.
pub const TEXT_EXTENSION: &str = ".azad";

/// Reads `<file>.azad:<goal>`: a graph in the text form of the IR. Paths are relative to the workspace root, unless
/// they are absolute; a `local("dir")` source is relative to the file.
#[derive(Debug, Default, Clone, Copy)]
pub struct TextFrontend;

impl TextFrontend {
    fn split(target: &str) -> Option<(&str, &str)> {
        let (file, goal) = target.rsplit_once(':')?;
        file.ends_with(TEXT_EXTENSION).then_some((file, goal))
    }

    /// The package of a text file, without a goal.
    pub fn read(root: &Path, file: &Path) -> Result<Package, FrontendError> {
        let path = root.join(file);
        let content = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let document = text::parse(&content).map_err(|e| format!("{}: {e}", path.display()))?;
        let base = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let source = |spec: TextSource| match spec {
            TextSource::Source(Source::Tree(digest)) => SourceSpec::Tree(digest),
            TextSource::Source(Source::Output { action, path }) => SourceSpec::Output { action, path },
            TextSource::Local(local) => SourceSpec::Local { path: base.join(local) },
        };
        let actions = document
            .actions
            .into_iter()
            .map(|(name, spec)| {
                let converted = Spec {
                    kind: spec.kind,
                    platform: spec.platform,
                    root: source(spec.root),
                    command: spec.command,
                    env: spec.env,
                    inputs: spec.inputs.into_iter().map(|(p, s)| (p, source(s))).collect(),
                    outputs: spec.outputs,
                    caches: spec.caches,
                    work: spec.work.into_iter().map(|(p, s)| (p, source(s))).collect(),
                    dir: spec.dir,
                    secrets: spec.secrets,
                };
                (name, converted)
            })
            .collect();
        Ok(Package { actions, goals: document.goals, ..Package::default() })
    }
}

impl Frontend for TextFrontend {
    fn name(&self) -> &'static str {
        "text"
    }

    fn accepts(&self, target: &str) -> bool {
        TextFrontend::split(target).is_some()
    }

    fn load(&self, request: &LoadRequest<'_>) -> Result<Loaded, FrontendError> {
        let (file, goal) = TextFrontend::split(request.target).ok_or("a text target is <file>.azad:<goal>")?;
        let package = TextFrontend::read(request.root, &PathBuf::from(file))?;
        Ok(Loaded { package, goal: goal.to_owned(), scope: None })
    }
}
