//! Remote caches (decision 0012): results and their content in storage that outlives a machine, such as a directory
//! on a shared disk or the GitHub Actions cache.
//!
//! A provider stores whole objects by name. The remote cache keeps four kinds: a signed result entry per action, a
//! pack per output tree, a blob per log, and a signed image entry per image and platform. A hit fetches only the entry; content is fetched when an action that runs
//! needs it. The cache wraps any backend (`RemoteBackend`), so the run loop and the local backend do not know it.

mod backend;
mod directory;
mod entry;
pub mod gha;
pub mod keys;
mod pack;
mod spec;
mod volumes;

pub use volumes::Kept;

use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

pub use backend::RemoteBackend;
pub use directory::DirectoryProvider;
pub use keys::{PublicKey, SigningKey, parse_public_keys};
pub use spec::{SIGNING_KEY_ENV, TRUSTED_KEYS_FILE};

use crate::ir::{ActionResult, Digest, Image};
use crate::store::{DirectoryState, Store, StoreError};
use crate::timings::{Timings, tree_name};
use crate::trust::Trust;

/// A version in every object name, so that a change of format starts new objects.
const FORMAT: &str = "azad1";
const PACK_FILE: &str = "pack";
const STATE_DIR: &str = "state";

#[derive(Debug, thiserror::Error)]
pub enum RemoteError {
    #[error("{path}: {error}")]
    Io { path: PathBuf, error: io::Error },
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("{0}")]
    Http(String),
    #[error("{0}")]
    Key(String),
    #[error("{0}")]
    Entry(String),
    #[error("signed with {0}, which is not a trusted key")]
    Untrusted(String),
    #[error("the provider refused the write: {0}")]
    Denied(String),
}

impl RemoteError {
    pub(crate) fn io(path: &Path, error: io::Error) -> Self {
        RemoteError::Io { path: path.to_owned(), error }
    }
}

/// Storage for whole objects by name. Objects do not change: a `put` of a name that exists keeps the object.
pub trait Provider: Send + Sync {
    /// A short name for output, such as `gha` or `dir`.
    fn name(&self) -> String;
    fn contains(&self, key: &str) -> Result<bool, RemoteError>;
    /// Writes the object to `dest`. Returns false when there is no such object.
    fn get(&self, key: &str, dest: &Path) -> Result<bool, RemoteError>;
    fn put(&self, key: &str, source: &Path) -> Result<(), RemoteError>;
    /// Writes the newest object whose name starts with `prefix` to `dest`. Returns false when there is none, or when
    /// the provider cannot look up names by prefix.
    fn get_newest(&self, _prefix: &str, _dest: &Path) -> Result<bool, RemoteError> {
        Ok(false)
    }
}

/// What a remote cache may do, and with which keys.
pub struct RemoteConfig {
    pub provider: Box<dyn Provider>,
    pub read: bool,
    /// Writes also need `signing_key`.
    pub write: bool,
    pub signing_key: Option<SigningKey>,
    /// The keys whose entries are believed. The public key of `signing_key` is always trusted.
    pub trusted: Vec<PublicKey>,
}

/// What a remote cache did in a run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RemoteSummary {
    pub provider: String,
    pub hits: usize,
    pub uploads: usize,
    pub uploaded_bytes: u64,
    pub downloaded_bytes: u64,
    /// Entries that were misses because the cache no longer had their content.
    pub incomplete: usize,
    /// Images that were known from the cache, so they were not pulled.
    pub images: usize,
    /// Cache volumes that were restored from the cache, and stored in it (decision 0017).
    pub volumes_restored: usize,
    pub volumes_stored: usize,
    /// For each stored copy, why: the first file whose content changed, or that the state is new here.
    pub stored_because: Vec<String>,
    /// Why the cache did not write, when it may not.
    pub not_writing: Option<String>,
    /// Errors that did not stop the run: a failed read is a miss, and a failed write is skipped.
    pub errors: Vec<String>,
}

impl fmt::Display for RemoteSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} cache: {} hits, {} uploads ({}), {} downloaded",
            self.provider,
            self.hits,
            self.uploads,
            megabytes(self.uploaded_bytes),
            megabytes(self.downloaded_bytes)
        )?;
        if self.images > 0 {
            write!(f, ", {} images known without a pull", self.images)?;
        }
        if self.incomplete > 0 {
            write!(f, ", {} entries without their content", self.incomplete)?;
        }
        if self.volumes_restored > 0 || self.volumes_stored > 0 {
            write!(f, ", volumes: {} restored, {} stored", self.volumes_restored, self.volumes_stored)?;
        }
        if !self.stored_because.is_empty() {
            write!(f, " ({})", self.stored_because.join("; "))?;
        }
        if let Some(reason) = &self.not_writing {
            write!(f, "; not writing: {reason}")?;
        }
        if let Some(first) = self.errors.first() {
            write!(f, "; {} errors, the first: {first}", self.errors.len())?;
        }
        Ok(())
    }
}

/// Whether a change of kept state is worth a new copy: at least a megabyte, and a hundredth of the state. Tools keep
/// records of their own, such as Cargo's `.rustc_info.json` and `.global-cache`, which change on every new machine;
/// the copy that the cache has is as good while only those changed.
fn worth_a_copy(changed: u64, size: u64) -> bool {
    const MIN_CHANGED_BYTES: u64 = 1_000_000;
    const MIN_CHANGED_PER_HUNDRED: u64 = 1;
    const HUNDRED: u64 = 100;
    changed >= MIN_CHANGED_BYTES && changed * HUNDRED >= size * MIN_CHANGED_PER_HUNDRED
}

fn file_size(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or_default()
}

fn megabytes(bytes: u64) -> String {
    const MEGABYTE: f64 = 1_000_000.0;
    format!("{:.1} MB", bytes as f64 / MEGABYTE)
}

fn result_key(action: &Digest) -> String {
    format!("{FORMAT}-result-{}", action.hex())
}

fn tree_key(tree: &Digest) -> String {
    format!("{FORMAT}-tree-{}", tree.hex())
}

fn blob_key(blob: &Digest) -> String {
    format!("{FORMAT}-blob-{}", blob.hex())
}

/// The empty blob is never stored remotely: its content is known from its digest, and a provider such as the GHA
/// cache refuses empty objects.
fn is_empty(blob: &Digest) -> bool {
    *blob == Digest::of_bytes(&[])
}

fn image_key(name: &str) -> String {
    format!("{FORMAT}-image-{}", Digest::of_bytes(name.as_bytes()).hex())
}

/// Makes a tree that the store does not have yet, such as by pulling an image.
pub type TreeSource = Box<dyn FnOnce() -> Result<(), String> + Send>;

/// A tree that is made only when an action needs it: by the first action, while others that need it wait.
struct Deferred {
    /// What the tree is, such as the reference of an image.
    name: String,
    source: Mutex<Option<TreeSource>>,
    made: OnceLock<Result<(), String>>,
}

impl Deferred {
    /// Makes the tree once, with `make`, which gets the source; other callers wait and get the same result.
    fn make(&self, make: impl FnOnce(TreeSource) -> Result<(), String>) -> Result<(), String> {
        let made = self.made.get_or_init(|| {
            let source = self.source.lock().expect("the source lock is not poisoned").take();
            source.map_or_else(|| Err("the tree was already made".to_owned()), make)
        });
        made.clone()
    }
}

pub struct RemoteCache {
    provider: Box<dyn Provider>,
    store: Store,
    read: bool,
    write: bool,
    signing_key: Option<SigningKey>,
    trusted: Vec<PublicKey>,
    summary: Mutex<RemoteSummary>,
    /// Trees that are known by digest but not made yet, with how to make them when an action needs them.
    deferred: Mutex<HashMap<Digest, Arc<Deferred>>>,
    /// The kept state that actions of this run needed, each restored once, with its state before the run changed it.
    restored: Mutex<HashMap<Kept, Arc<OnceLock<Option<DirectoryState>>>>>,
    /// The kept state that actions changed in this run, to store at its end.
    changed: Mutex<BTreeSet<Kept>>,
    /// The trees that deferred sources made in this run, such as pulled images, to store at its end.
    made_trees: Mutex<BTreeSet<Digest>>,
    timings: Timings,
}

impl RemoteCache {
    pub fn new(config: RemoteConfig, store: Store) -> Self {
        let mut trusted = config.trusted;
        if let Some(key) = &config.signing_key {
            trusted.push(key.public());
        }
        let not_writing = match (config.write, &config.signing_key) {
            (false, _) => Some("the cache is read-only here".to_owned()),
            (true, None) => Some("no signing key (AZAD_SIGNING_KEY)".to_owned()),
            (true, Some(_)) => None,
        };
        let summary = RemoteSummary { provider: config.provider.name(), not_writing, ..RemoteSummary::default() };
        RemoteCache {
            provider: config.provider,
            store,
            read: config.read,
            write: config.write,
            signing_key: config.signing_key,
            trusted,
            summary: Mutex::new(summary),
            deferred: Mutex::new(HashMap::new()),
            restored: Mutex::new(HashMap::new()),
            changed: Mutex::new(BTreeSet::new()),
            made_trees: Mutex::new(BTreeSet::new()),
            timings: Timings::default(),
        }
    }

    /// Records how long downloads, unpacking and uploads take in `timings`.
    pub fn with_timings(mut self, timings: Timings) -> Self {
        self.timings = timings;
        self
    }

    /// What the cache did since the last call.
    pub fn take_summary(&self) -> RemoteSummary {
        let mut summary = self.summary.lock().expect("the summary lock is not poisoned");
        let fresh = RemoteSummary {
            provider: summary.provider.clone(),
            not_writing: summary.not_writing.clone(),
            ..RemoteSummary::default()
        };
        std::mem::replace(&mut *summary, fresh)
    }

    fn note(&self, change: impl FnOnce(&mut RemoteSummary)) {
        change(&mut self.summary.lock().expect("the summary lock is not poisoned"));
    }

    fn error(&self, what: &str, error: RemoteError) {
        self.note(|s| s.errors.push(format!("{what}: {error}")));
    }

    /// A verified result for an action, or none. A failure to read is reported, and is a miss.
    pub fn result(&self, action: &Digest) -> Option<ActionResult> {
        if !self.read {
            return None;
        }
        match self.try_result(action) {
            Ok(Some(result)) => {
                self.note(|s| s.hits += 1);
                Some(result)
            }
            Ok(None) => None,
            Err(error) => {
                self.error(&format!("reading the result of {action}"), error);
                None
            }
        }
    }

    fn try_result(&self, action: &Digest) -> Result<Option<ActionResult>, RemoteError> {
        let scratch = self.store.scratch_dir()?;
        let path = scratch.path().join("entry");
        if !self.provider.get(&result_key(action), &path)? {
            return Ok(None);
        }
        let bytes = std::fs::read(&path).map_err(|e| RemoteError::io(&path, e))?;
        self.note(|s| s.downloaded_bytes += bytes.len() as u64);
        let result = entry::verify(&bytes, action, &self.trusted)?;
        // Providers drop objects one by one, and an entry is read more often than the content it names, so the
        // content can be gone first. Such an entry is a miss: the action runs again and uploads its content again.
        for tree in result.outputs.values() {
            if !pack::is_complete(&self.store, tree) && !self.provider.contains(&tree_key(tree))? {
                self.note(|s| s.incomplete += 1);
                return Ok(None);
            }
        }
        Ok(Some(result))
    }

    /// The record that a trusted key gave the name of an image, or none.
    pub fn image(&self, name: &str) -> Option<Image> {
        if !self.read {
            return None;
        }
        match self.try_image(name) {
            Ok(Some(image)) => {
                self.note(|s| s.images += 1);
                Some(image)
            }
            Ok(None) => None,
            Err(error) => {
                self.error(&format!("reading the image entry {name}"), error);
                None
            }
        }
    }

    /// Records the record of an image for other machines. A failure is reported, and the run goes on.
    pub fn upload_image(&self, name: &str, image: &Image) {
        let Some(key) = self.signing_key.as_ref().filter(|_| self.write) else { return };
        if let Err(error) = self.try_upload_image(name, image, key) {
            self.error(&format!("uploading the image entry {name}"), error);
        }
    }

    fn try_image(&self, name: &str) -> Result<Option<Image>, RemoteError> {
        let scratch = self.store.scratch_dir()?;
        let path = scratch.path().join("image");
        if !self.provider.get(&image_key(name), &path)? {
            return Ok(None);
        }
        let bytes = std::fs::read(&path).map_err(|e| RemoteError::io(&path, e))?;
        self.note(|s| s.downloaded_bytes += bytes.len() as u64);
        entry::verify_image(&bytes, name, &self.trusted).map(Some)
    }

    fn try_upload_image(&self, name: &str, image: &Image, key: &SigningKey) -> Result<(), RemoteError> {
        let object = image_key(name);
        if self.provider.contains(&object)? {
            return Ok(());
        }
        let scratch = self.store.scratch_dir()?;
        let path = scratch.path().join("image");
        std::fs::write(&path, entry::sign_image(name, image, key)).map_err(|e| RemoteError::io(&path, e))?;
        self.put(&object, &path)
    }

    /// Makes `tree`, which `name` names, with `source` if an action that runs needs it; until then, nothing is fetched.
    pub fn defer(&self, tree: Digest, name: &str, source: TreeSource) {
        let deferred =
            Arc::new(Deferred { name: name.to_owned(), source: Mutex::new(Some(source)), made: OnceLock::new() });
        self.deferred.lock().expect("the deferred lock is not poisoned").insert(tree, deferred);
    }

    /// Makes the store hold a whole tree: from the remote cache, or from its deferred source.
    pub fn ensure_tree(&self, tree: &Digest) -> Result<(), RemoteError> {
        if pack::is_complete(&self.store, tree) {
            return Ok(());
        }
        let deferred = self.deferred.lock().expect("the deferred lock is not poisoned").get(tree).cloned();
        if let Some(deferred) = deferred {
            let timer = self.timings.start(format!("wait for {}", deferred.name));
            deferred.make(|source| self.make_deferred(tree, &deferred.name, source)).map_err(RemoteError::Entry)?;
            timer.stop();
            return Ok(());
        }
        match self.fetch_tree(tree, &tree_name(tree))? {
            true => Ok(()),
            false => Err(RemoteError::Entry(format!("the tree {tree} is neither here nor in the remote cache"))),
        }
    }

    /// Fetches the pack of a tree, which `name` names, from the cache. Returns false when the cache does not have it.
    fn fetch_tree(&self, tree: &Digest, name: &str) -> Result<bool, RemoteError> {
        let scratch = self.store.scratch_dir()?;
        let path = scratch.path().join(PACK_FILE);
        let timer = self.timings.start(format!("download the pack of {name}"));
        if !self.read || !self.provider.get(&tree_key(tree), &path)? {
            return Ok(false);
        }
        let bytes = self.note_download(&path);
        timer.stop_with_bytes(bytes);
        let timer = self.timings.start(format!("read the pack of {name}"));
        pack::read(&self.store, tree, &path)?;
        timer.stop_with_bytes(bytes);
        Ok(true)
    }

    /// A deferred tree, such as an image: from the cache when it has the pack, which is faster than a registry; else
    /// from its source, after which the end of the run gives the cache the pack, for the next machine. The actions
    /// that wait for the tree do not wait for that upload.
    fn make_deferred(&self, tree: &Digest, name: &str, source: TreeSource) -> Result<(), String> {
        if self.fetch_tree(tree, name).map_err(|e| e.to_string())? {
            return Ok(());
        }
        let timer = self.timings.start(format!("pull {name}"));
        source()?;
        timer.stop();
        self.made_trees.lock().expect("the trees lock is not poisoned").insert(*tree);
        Ok(())
    }

    fn upload_tree(&self, tree: &Digest) -> Result<(), RemoteError> {
        let name = tree_key(tree);
        if self.provider.contains(&name)? {
            return Ok(());
        }
        let scratch = self.store.scratch_dir()?;
        let path = scratch.path().join(PACK_FILE);
        let timer = self.timings.start(format!("pack {}", tree_name(tree)));
        pack::write(&self.store, tree, &path)?;
        timer.stop();
        let timer = self.timings.start(format!("upload the pack of {}", tree_name(tree)));
        self.put(&name, &path)?;
        timer.stop_with_bytes(file_size(&path));
        Ok(())
    }

    /// Makes the store hold a blob, from the remote cache when it is missing here.
    pub fn ensure_blob(&self, blob: &Digest) -> Result<(), RemoteError> {
        if self.store.contains(blob) {
            return Ok(());
        }
        if is_empty(blob) {
            self.store.put_bytes(&[])?;
            return Ok(());
        }
        let scratch = self.store.scratch_dir()?;
        let path = scratch.path().join("blob");
        if !self.read || !self.provider.get(&blob_key(blob), &path)? {
            return Err(RemoteError::Entry(format!("the blob {blob} is neither here nor in the remote cache")));
        }
        self.note_download(&path);
        let stored = self.store.put_file(&path)?;
        match stored == *blob {
            true => Ok(()),
            false => Err(RemoteError::Entry(format!("the object for {blob} holds {stored}"))),
        }
    }

    /// Notes a downloaded object, and returns its size.
    fn note_download(&self, path: &Path) -> u64 {
        let bytes = file_size(path);
        self.note(|s| s.downloaded_bytes += bytes);
        bytes
    }

    /// Uploads a successful result that the cache does not have: its content first, then the signed entry, so that an entry never names content
    /// that the cache does not have. A failure is reported, and the run goes on.
    /// Restores kept state that this machine does not have from its newest copy in the cache, if there is one. Actions
    /// that need it at the same time wait for one restore. A failure is noted, not returned: the action still runs,
    /// with empty state.
    pub fn ensure_kept(&self, kept: &Kept) {
        let once =
            self.restored.lock().expect("the kept lock is not poisoned").entry(kept.clone()).or_default().clone();
        once.get_or_init(|| {
            if !kept.exists(&self.store)
                && self.read
                && let Err(error) = self.try_restore(kept)
            {
                self.error(&format!("restoring {}", kept.name()), error);
            }
            let timer = self.timings.start(format!("read the state of {}", kept.name()));
            let state = kept.state(&self.store, &DirectoryState::default()).ok().flatten();
            timer.stop();
            state
        });
    }

    fn try_restore(&self, kept: &Kept) -> Result<(), RemoteError> {
        let scratch = self.store.scratch_dir()?;
        let pack = scratch.path().join(PACK_FILE);
        let timer = self.timings.start(format!("download {}", kept.name()));
        if !self.provider.get_newest(&kept.prefix(), &pack)? {
            return Ok(());
        }
        let bytes = self.note_download(&pack);
        timer.stop_with_bytes(bytes);
        let timer = self.timings.start(format!("unpack {}", kept.name()));
        let staging = kept.staging(&self.store)?;
        let state = staging.path().join(STATE_DIR);
        volumes::unpack(&pack, &state)?;
        timer.stop_with_bytes(bytes);
        if kept.install(&self.store, &state)? {
            self.note(|s| s.volumes_restored += 1);
        }
        Ok(())
    }

    /// Notes that an action changed kept state, so that the end of the run stores it.
    pub fn kept_changed(&self, kept: &Kept) {
        self.changed.lock().expect("the kept lock is not poisoned").insert(kept.clone());
    }

    /// Stores a copy of the kept state that actions changed in this run, and the trees that deferred sources made, when
    /// the run may write, and ends the run's record of both. State whose files have the same content as when the run first used it is skipped, and so is a
    /// volume that another run holds now. Failures are noted, not returned.
    pub fn store_kept(&self) {
        let changed = std::mem::take(&mut *self.changed.lock().expect("the kept lock is not poisoned"));
        let before = std::mem::take(&mut *self.restored.lock().expect("the kept lock is not poisoned"));
        let made = std::mem::take(&mut *self.made_trees.lock().expect("the trees lock is not poisoned"));
        if self.signing_key.is_none() || !self.write {
            return;
        }
        for tree in made {
            if let Err(error) = self.upload_tree(&tree) {
                self.error(&format!("uploading the tree {tree}"), error);
            }
        }
        for kept in changed {
            let state = before.get(&kept).and_then(|once| once.get().cloned()).flatten();
            if let Err(error) = self.try_store(&kept, state) {
                self.error(&format!("storing {}", kept.name()), error);
            }
        }
    }

    /// Stores a copy of kept state, unless its files have the content that they had in `before`.
    fn try_store(&self, kept: &Kept, before: Option<DirectoryState>) -> Result<(), RemoteError> {
        let held = match kept {
            Kept::Volume { name, platform } => Some(self.store.cache_volume(name, platform, Trust::Main)?),
            Kept::Directory { .. } => None,
        };
        if held.as_ref().is_some_and(|volume| !volume.is_shared()) {
            return Ok(());
        }
        let since = before.clone().unwrap_or_default();
        let timer = self.timings.start(format!("read the state of {}", kept.name()));
        let now = kept.state(&self.store, &since)?.unwrap_or_default();
        timer.stop();
        let reason = match &before {
            Some(before) => {
                let (changed, largest) = before.difference(&now);
                if !worth_a_copy(changed, now.size()) {
                    return Ok(());
                }
                format!("{} changed, such as {}", megabytes(changed), largest.unwrap_or_default())
            }
            None => "new".to_owned(),
        };
        let scratch = self.store.scratch_dir()?;
        let pack = scratch.path().join(PACK_FILE);
        let timer = self.timings.start(format!("pack {}", kept.name()));
        volumes::pack(&kept.root(&self.store), &pack)?;
        timer.stop();
        drop(held);
        let bytes = std::fs::metadata(&pack).map_err(|e| RemoteError::io(&pack, e))?.len();
        let timer = self.timings.start(format!("upload {}", kept.name()));
        self.provider.put(&kept.new_key(), &pack)?;
        timer.stop_with_bytes(bytes);
        self.note(|s| {
            s.volumes_stored += 1;
            s.uploaded_bytes += bytes;
            s.stored_because.push(format!("{}: {reason}", kept.name()));
        });
        Ok(())
    }

    pub fn upload(&self, action: &Digest, result: &ActionResult) {
        let Some(key) = self.signing_key.as_ref().filter(|_| self.write) else { return };
        if let Err(error) = self.try_upload(action, result, key) {
            self.error(&format!("uploading the result of {action}"), error);
        }
    }

    /// Each part is checked on its own: an entry can be there while its content is gone.
    fn try_upload(&self, action: &Digest, result: &ActionResult, key: &SigningKey) -> Result<(), RemoteError> {
        let scratch = self.store.scratch_dir()?;
        for tree in result.outputs.values() {
            self.upload_tree(tree)?;
        }
        for blob in result.logs().filter(|blob| !is_empty(blob)) {
            let name = blob_key(blob);
            if !self.provider.contains(&name)? {
                self.put(&name, &self.store.blob_path(blob))?;
            }
        }
        let name = result_key(action);
        if self.provider.contains(&name)? {
            return Ok(());
        }
        let path = scratch.path().join("entry");
        std::fs::write(&path, entry::sign(action, result, key)).map_err(|e| RemoteError::io(&path, e))?;
        self.put(&name, &path)
    }

    fn put(&self, name: &str, path: &Path) -> Result<(), RemoteError> {
        self.provider.put(name, path)?;
        let bytes = file_size(path);
        self.note(|s| {
            s.uploads += 1;
            s.uploaded_bytes += bytes;
        });
        Ok(())
    }
}

impl fmt::Debug for RemoteConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RemoteConfig")
            .field("provider", &self.provider.name())
            .field("read", &self.read)
            .field("write", &self.write)
            .field("signing_key", &self.signing_key)
            .field("trusted", &self.trusted)
            .finish()
    }
}

impl fmt::Debug for RemoteCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RemoteCache").field("provider", &self.provider.name()).finish_non_exhaustive()
    }
}
