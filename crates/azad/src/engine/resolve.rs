//! Turning the sources of a frontend's package into trees in the store: globs and directories are stored, images are
//! pulled, and outputs of other actions stay references. An image root also gives the base of the action's environment
//! (decision 0016).

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::frontend::{InlineFile, Note, NoteLevel, Package, SourceSpec};
use crate::ir::{Canonical, Digest, Directory, Graph, Image, Node, Source, Spec};
use crate::oci::Puller;
use crate::remote::{RemoteCache, TreeSource};
use crate::sandbox::{Dockerfile, ImageContent, build_image, export_image, image_content};
use crate::store::{Selection, Store};
use crate::timings::Timings;
use crate::trust::Trust;
use tempfile::TempDir;

use crate::engine::{EngineConfig, EngineError, Event, git};

const GIT_ALIAS_PREFIX: &str = "git";
const EXECUTABLE_MODE: u32 = 0o755;
const FILE_MODE: u32 = 0o644;

pub(crate) struct Resolver<'a> {
    pub(crate) config: &'a EngineConfig,
    pub(crate) root: &'a Path,
    pub(crate) store: &'a Store,
    pub(crate) puller: &'a Puller,
    pub(crate) remote: Option<&'a RemoteCache>,
    pub(crate) timings: &'a Timings,
    pub(crate) stored: HashMap<String, Digest>,
    /// The images of this load, by the name of their record.
    pub(crate) images: HashMap<String, Image>,
    /// Where the package declared its sources (see `Package::declared_at`).
    pub(crate) declared_at: BTreeMap<String, String>,
}

/// An image that a container runtime built from a Dockerfile, before Azad stores it.
#[derive(Clone, Copy)]
struct Built<'b> {
    runtime: &'b Path,
    id: &'b str,
    content: &'b ImageContent,
    reference: &'b str,
    file: &'b str,
}

/// The references by which the images that runtimes built from Dockerfiles are remembered, with a digest of their
/// content.
const BUILT_IMAGES: &str = "azad.invalid/built";
/// The start of the names, in a load, of the images of Dockerfiles.
const BUILT_PREFIX: &str = "dockerfile:";
const EXPORTED_ARCHIVE: &str = "image.tar";

impl Resolver<'_> {
    pub(crate) fn graph(&mut self, mut package: Package, observe: &mut dyn FnMut(Event)) -> Result<Graph, EngineError> {
        self.declared_at = std::mem::take(&mut package.declared_at);
        let mut actions = BTreeMap::new();
        for (name, spec) in package.actions {
            let inputs = spec
                .inputs
                .iter()
                .map(|(path, source)| self.source(source, &spec.platform, observe).map(|s| (path.clone(), s)))
                .collect::<Result<_, _>>()?;
            let work = spec
                .work
                .iter()
                .map(|(path, source)| self.source(source, &spec.platform, observe).map(|s| (path.clone(), s)))
                .collect::<Result<_, _>>()?;
            let (root, env) = self.root(&spec.root, &spec.platform, &spec.env, observe)?;
            let resolved = Spec {
                kind: spec.kind,
                root,
                platform: spec.platform,
                command: spec.command,
                env,
                inputs,
                outputs: spec.outputs,
                caches: spec.caches,
                work,
                dir: spec.dir,
                secrets: spec.secrets,
            };
            actions.insert(name, resolved);
        }
        Ok(Graph { actions, goals: package.goals })
    }

    /// The root of an action, and its environment: an image's `Env` with the action's variables in place of the
    /// image's.
    fn root(
        &mut self,
        source: &SourceSpec,
        platform: &str,
        env: &BTreeMap<String, String>,
        observe: &mut dyn FnMut(Event),
    ) -> Result<(Source, BTreeMap<String, String>), EngineError> {
        let (image, image_env) = match source {
            SourceSpec::Image { reference, env } => (self.image(reference, platform, observe)?, env),
            SourceSpec::Dockerfile { env, .. } => (self.built_image(source, platform, observe)?, env),
            _ => return Ok((self.source(source, platform, observe)?, env.clone())),
        };
        let mut on_top = image_env.clone();
        on_top.extend(env.iter().map(|(name, value)| (name.clone(), value.clone())));
        Ok((Source::Tree(image.tree), image.env_with(&on_top)))
    }

    fn source(
        &mut self,
        source: &SourceSpec,
        platform: &str,
        observe: &mut dyn FnMut(Event),
    ) -> Result<Source, EngineError> {
        let key = memo_key(source, platform);
        if let Some(digest) = self.stored.get(&key) {
            return Ok(Source::Tree(*digest));
        }
        let digest = match source {
            SourceSpec::Output { action, path } => {
                return Ok(Source::Output { action: action.clone(), path: path.clone() });
            }
            SourceSpec::Tree(digest) => *digest,
            SourceSpec::Glob { dir, include, exclude } => {
                let selection = Selection::globs(include, exclude).map_err(EngineError::store(source.to_string()))?;
                let tree = self
                    .store
                    .ingest(&self.root.join(dir), &selection)
                    .map_err(EngineError::store(source.to_string()))?;
                if tree == Directory::default().digest() {
                    observe(Event::Warning(self.empty_glob(source, dir, include, exclude)));
                }
                tree
            }
            SourceSpec::Local { path } => self
                .store
                .ingest(&self.root.join(path), &Selection::All)
                .map_err(EngineError::store(source.to_string()))?,
            SourceSpec::Image { reference, .. } => self.image(reference, platform, observe)?.tree,
            SourceSpec::Inline { files } => self.inline(files)?,
            SourceSpec::Tool { name } => self.tool(name)?,
            SourceSpec::GitFiles { dir } => {
                let directory = self.root.join(dir);
                let files = git::working_tree_files(&directory)?;
                self.store
                    .ingest(&directory, &Selection::Paths(files))
                    .map_err(EngineError::store(source.to_string()))?
            }
            SourceSpec::Git { url, rev } => self.git(url, rev, observe)?,
            SourceSpec::Tracked { dir, selections } => {
                let directory = self.root.join(dir);
                let globs = selections
                    .iter()
                    .map(|selection| Selection::globs(&selection.include, &selection.exclude))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(EngineError::store(source.to_string()))?;
                let files = git::working_tree_files(&directory)?
                    .into_iter()
                    .filter(|file| globs.iter().any(|selection| selection.keeps_leaf(file)))
                    .collect();
                self.store
                    .ingest(&directory, &Selection::Paths(files))
                    .map_err(EngineError::store(source.to_string()))?
            }
            SourceSpec::Dockerfile { .. } => self.built_image(source, platform, observe)?.tree,
        };
        self.stored.insert(key, digest);
        Ok(Source::Tree(digest))
    }

    /// The warning for a glob that matches no file: the action gets an empty directory, which is most often a typo in
    /// a pattern.
    fn empty_glob(&self, source: &SourceSpec, dir: &str, include: &[String], exclude: &[String]) -> String {
        let excluded = match exclude.is_empty() {
            true => String::new(),
            false => format!(", exclude = {exclude:?}"),
        };
        let declared = match self.declared_at.get(&source.to_string()) {
            Some(at) => format!(" (declared at {at})"),
            None => String::new(),
        };
        format!("glob({include:?}{excluded}) in //{dir} matches no file{declared}")
    }

    /// The image of a Dockerfile (decision 0024, amendment): this host's container runtime builds it, and Azad stores
    /// its file system as it stores a pulled image, once for each content: its layers and `Env`. An action then runs in
    /// it in Azad's own sandbox. A run of code that is not merged builds none, because a build runs the Dockerfile's
    /// steps with the host's runtime.
    fn built_image(
        &mut self,
        source: &SourceSpec,
        platform: &str,
        observe: &mut dyn FnMut(Event),
    ) -> Result<Image, EngineError> {
        let SourceSpec::Dockerfile { file, context, target, args, contexts, .. } = source else {
            unreachable!("built_image is called for a Dockerfile");
        };
        let built = format!("{BUILT_PREFIX}{file}:{target}:{context}:{args:?}:{contexts:?}");
        if let Some(image) = self.images.get(&built) {
            return Ok(image.clone());
        }
        if self.config.trust == Trust::PullRequest {
            return Err(EngineError::Config(format!(
                "//{file}: a run of a pull request does not build images from Dockerfiles, because a build runs their \
                 steps with this host's container runtime; use an image by reference in such a run"
            )));
        }
        let runtime = crate::sandbox::find_runtime().ok_or_else(|| {
            EngineError::Config(format!(
                "//{file}: an isolated run builds the image of a Dockerfile with Docker or Podman, and this host has \
                 neither; set ${} to one of them",
                crate::sandbox::RUNTIME_ENV
            ))
        })?;
        let dockerfile = Dockerfile { file, context, target, args, contexts };
        let timer = self.timings.start(format!("build the image of //{file}"));
        let id = build_image(&runtime, self.root, &dockerfile, &BTreeMap::new())
            .map_err(|e| EngineError::Config(e.to_string()))?;
        let content = image_content(&runtime, &id).map_err(|e| EngineError::Config(e.to_string()))?;
        timer.stop();
        let key = serde_json::to_vec(&(&content.layers, &content.env)).expect("lists of text are JSON");
        let reference = format!("{BUILT_IMAGES}@sha256:{}", Digest::of_bytes(&key).hex());
        let pull_error = |source| EngineError::Pull { reference: reference.clone(), error: source };
        let image = match self.puller.known(&reference, platform).map_err(pull_error)? {
            Some(image) => image,
            None => {
                let built = Built { runtime: &runtime, id: &id, content: &content, reference: &reference, file };
                self.import_built(&built, platform, observe)?
            }
        };
        self.images.insert(built, image.clone());
        Ok(image)
    }

    /// Stores the file system and the `Env` of an image that the runtime built, by the reference of its ID.
    fn import_built(
        &self,
        built: &Built,
        platform: &str,
        observe: &mut dyn FnMut(Event),
    ) -> Result<Image, EngineError> {
        let Built { runtime, id, content, reference, file } = *built;
        let timer = self.timings.start(format!("store the image of //{file}"));
        let tmp_root = self.store.tmp_dir();
        let staging =
            TempDir::new_in(&tmp_root).map_err(|e| EngineError::Config(format!("{}: {e}", tmp_root.display())))?;
        let archive = staging.path().join(EXPORTED_ARCHIVE);
        export_image(runtime, id, &archive).map_err(|e| EngineError::Config(e.to_string()))?;
        let pull_error = |source| EngineError::Pull { reference: reference.to_owned(), error: source };
        let image = self.puller.import(reference, platform, &archive, &content.env).map_err(pull_error)?;
        timer.stop();
        observe(Event::Note(Note {
            level: NoteLevel::Normal,
            text: format!("stored the image of //{file} that {} built, {id}", runtime.display()),
        }));
        Ok(image)
    }

    /// The record of an image: known in this load, known here, known from the remote cache (pulled only when an
    /// action that runs needs its tree), or pulled now.
    fn image(&mut self, reference: &str, platform: &str, observe: &mut dyn FnMut(Event)) -> Result<Image, EngineError> {
        let pull_error = |source| EngineError::Pull { reference: reference.to_owned(), error: source };
        let name = Puller::alias_name(reference, platform).map_err(pull_error)?;
        if let Some(image) = self.images.get(&name) {
            return Ok(image.clone());
        }
        let image = self.find_image(reference, platform, &name, observe)?;
        self.images.insert(name, image.clone());
        Ok(image)
    }

    fn find_image(
        &self,
        reference: &str,
        platform: &str,
        name: &str,
        observe: &mut dyn FnMut(Event),
    ) -> Result<Image, EngineError> {
        let pull_error = |source| EngineError::Pull { reference: reference.to_owned(), error: source };
        if let Some(image) = self.puller.known(reference, platform).map_err(pull_error)? {
            if let Some(remote) = self.remote {
                remote.upload_image(name, &image);
            }
            return Ok(image);
        }
        if let Some(remote) = self.remote
            && let Some(image) = remote.image(name)
        {
            remote.defer(image.tree, reference, deferred_pull(self.store, reference, platform, &image));
            return Ok(image);
        }
        observe(Event::Pull { reference: reference.to_owned() });
        let timer = self.timings.start(format!("pull {reference}"));
        let image = self.puller.pull(reference, platform).map_err(pull_error)?;
        timer.stop();
        if let Some(remote) = self.remote {
            remote.upload_image(name, &image);
        }
        Ok(image)
    }
}

/// Pulls an image that the remote cache named, when an action that runs needs its tree. The pull must give the record
/// that the cache gave.
fn deferred_pull(store: &Store, reference: &str, platform: &str, expected: &Image) -> TreeSource {
    let (store, reference, platform, expected) =
        (store.clone(), reference.to_owned(), platform.to_owned(), expected.clone());
    Box::new(move || {
        let puller = Puller::new(store).map_err(|e| e.to_string())?;
        let pulled = puller.pull(&reference, &platform).map_err(|e| format!("pulling {reference}: {e}"))?;
        match pulled == expected {
            true => Ok(()),
            false => Err(format!(
                "{reference} gives the tree {} and the environment {:?}, not the tree {} and the environment {:?} as the cache says",
                pulled.tree, pulled.env, expected.tree, expected.env
            )),
        }
    })
}

impl Resolver<'_> {
    fn inline(&self, files: &BTreeMap<String, InlineFile>) -> Result<Digest, EngineError> {
        let tmp_root = self.store.tmp_dir();
        let staging =
            TempDir::new_in(&tmp_root).map_err(|e| EngineError::Config(format!("{}: {e}", tmp_root.display())))?;
        for (relative, file) in files {
            let path = staging.path().join(relative);
            let io_error = |e: std::io::Error| EngineError::Config(format!("{}: {e}", path.display()));
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(io_error)?;
            }
            fs::write(&path, &file.content).map_err(io_error)?;
            let mode = match file.executable {
                true => EXECUTABLE_MODE,
                false => FILE_MODE,
            };
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).map_err(io_error)?;
        }
        self.store.ingest(staging.path(), &Selection::All).map_err(EngineError::store("inline files"))
    }

    fn tool(&self, name: &str) -> Result<Digest, EngineError> {
        let path = self.config.tools.get(name).ok_or_else(|| EngineError::MissingTool {
            name: name.to_owned(),
            hint: crate::engine::config::missing_tool_hint(name),
        })?;
        let digest = self.store.put_file(path).map_err(EngineError::store(path.display().to_string()))?;
        let entries = BTreeMap::from([(name.to_owned(), Node::File { digest, executable: true })]);
        let directory = Directory::new(entries).map_err(|e| EngineError::Config(e.to_string()))?;
        self.store.put(&directory).map_err(EngineError::store(format!("tool {name}")))
    }

    fn git(&self, url: &str, rev: &str, observe: &mut dyn FnMut(Event)) -> Result<Digest, EngineError> {
        let commit = git::resolve_rev(url, rev)?;
        let alias = format!("{GIT_ALIAS_PREFIX} {url} {commit}");
        let state = self.store.state();
        if let Some(tree) = state.alias(&alias).map_err(EngineError::store("the aliases"))? {
            return Ok(tree);
        }
        observe(Event::Fetch { url: url.to_owned(), rev: rev.to_owned() });
        let tmp_root = self.store.tmp_dir();
        let checkout =
            TempDir::new_in(&tmp_root).map_err(|e| EngineError::Config(format!("{}: {e}", tmp_root.display())))?;
        git::fetch(url, &commit, checkout.path())?;
        let selection = Selection::globs(&["**"], &[".git", ".git/**"]).map_err(EngineError::store(url))?;
        let tree =
            self.store.ingest(checkout.path(), &selection).map_err(EngineError::store(format!("{url}@{commit}")))?;
        state.set_alias(&alias, &tree).map_err(EngineError::store("the aliases"))?;
        Ok(tree)
    }
}

/// The key under which a resolved source is remembered for one load. It must name the content, so inline files are
/// keyed by a digest of their content, not by their names.
fn memo_key(source: &SourceSpec, platform: &str) -> String {
    match source {
        SourceSpec::Inline { files } => {
            let mut hasher = crate::ir::Hasher::new();
            for (path, file) in files {
                hasher.update(path.as_bytes());
                hasher.update(&[0, u8::from(file.executable)]);
                hasher.update(&(file.content.len() as u64).to_be_bytes());
                hasher.update(&file.content);
            }
            format!("inline {}", hasher.finish())
        }
        other => format!("{other} {platform}"),
    }
}
