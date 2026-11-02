#![allow(clippy::unwrap_used)]

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use azad::ir::text;
use azad::ir::{Digest, Graph, Image};
use azad::local::LocalBackend;
use azad::remote::{DirectoryProvider, PublicKey, RemoteBackend, RemoteCache, RemoteConfig, RemoteSummary, SigningKey};
use azad::sandbox::{Bubblewrap, Exit, Limits, Sandbox, SandboxError, SandboxSpec};
use azad::scheduler::{Runner, Status};
use azad::store::{Selection, Store};
use azad::timings::{NOTEWORTHY, Timings};
use support::{ROOTFS_PATH, TEST_PROGRAMS, host_rootfs};
use tempfile::TempDir;

struct Counting {
    inner: Bubblewrap,
    runs: Arc<AtomicUsize>,
}

impl Sandbox for Counting {
    fn run(&self, spec: &SandboxSpec, logs: &dyn azad::log::LogSink) -> Result<Exit, SandboxError> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        self.inner.run(spec, logs)
    }
}

/// A machine with its own store, and the shared remote cache in `remote`.
struct Machine {
    _dir: TempDir,
    store: Store,
    backend: RemoteBackend,
    cache: Arc<RemoteCache>,
    runs: Arc<AtomicUsize>,
    timings: Timings,
}

fn machine(remote: &Path, signing_key: Option<SigningKey>, trusted: Vec<PublicKey>) -> (Machine, Digest) {
    machine_with(Box::new(DirectoryProvider::new(remote.to_path_buf())), signing_key, trusted)
}

/// A machine whose provider refuses empty objects, as the GitHub Actions cache does.
fn refusing_empty(remote: &Path, signing_key: Option<SigningKey>, trusted: Vec<PublicKey>) -> (Machine, Digest) {
    machine_with(Box::new(RefusesEmpty(DirectoryProvider::new(remote.to_path_buf()))), signing_key, trusted)
}

fn machine_with(
    provider: Box<dyn azad::remote::Provider>,
    signing_key: Option<SigningKey>,
    trusted: Vec<PublicKey>,
) -> (Machine, Digest) {
    let dir = TempDir::new().unwrap();
    let store = Store::open(dir.path().join("store")).unwrap();
    let rootfs = dir.path().join("rootfs");
    host_rootfs(&rootfs, TEST_PROGRAMS);
    let root = store.ingest(&rootfs, &Selection::All).unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let sandbox = Counting { inner: Bubblewrap::default(), runs: runs.clone() };
    let limits = Limits { timeout: Duration::from_secs(30), ..Limits::default() };
    let timings = Timings::default();
    let local = LocalBackend::new(store.clone(), Box::new(sandbox), limits).unwrap().with_timings(timings.clone());
    let config = RemoteConfig { provider, read: true, write: true, signing_key, trusted };
    let cache = Arc::new(RemoteCache::new(config, store.clone()).with_timings(timings.clone()));
    let backend = RemoteBackend::new(Box::new(local), cache.clone());
    (Machine { _dir: dir, store, backend, cache, runs, timings }, root)
}

fn key(name: &str) -> SigningKey {
    SigningKey::generate(name).unwrap()
}

/// `make` writes a value; `use` reads it from `make`'s output.
fn graph(root: &Digest) -> Graph {
    let document = format!(
        "action make\n  platform linux/amd64\n  root {root}\n  env PATH \"{ROOTFS_PATH}\"\n  command \"sh\" \"-c\" \"mkdir out && echo 42 > out/value\"\n  output out\n\n\
         action use\n  platform linux/amd64\n  root {root}\n  env PATH \"{ROOTFS_PATH}\"\n  command \"sh\" \"-c\" \"mkdir out && cat /in/value /in/value > out/twice\"\n  input /in output(make, \"out\")\n  output out\n\n\
         goal make make\ngoal use use\n"
    );
    text::parse(&document).unwrap().resolve(|_| Err::<Digest, ()>(())).unwrap()
}

impl Machine {
    fn run(&self, root: &Digest, goal: &str) -> (azad::scheduler::Outcome, RemoteSummary) {
        let graph = graph(root);
        let outcome = Runner::new(&graph, &self.backend, 1).run(goal, &mut |_| {}).unwrap();
        assert!(outcome.goal_holds(), "{outcome:?}");
        (outcome, self.cache.take_summary())
    }

    fn runs(&self) -> usize {
        self.runs.load(Ordering::SeqCst)
    }
}

fn objects(remote: &Path, kind: &str) -> Vec<std::path::PathBuf> {
    fs::read_dir(remote).unwrap().map(|e| e.unwrap().path()).filter(|p| p.to_string_lossy().contains(kind)).collect()
}

/// A writer with a new key, and the key's public half.
fn writer(remote: &Path) -> (Machine, Digest, PublicKey) {
    let signer = key("ci");
    let public = signer.public();
    let (machine, root) = machine(remote, Some(signer), vec![]);
    (machine, root, public)
}

#[test]
fn a_second_machine_gets_results_without_their_content() {
    let remote = TempDir::new().unwrap();
    let (a, root, public) = writer(remote.path());
    a.run(&root, "use");
    assert_eq!(objects(remote.path(), "-result-").len(), 2);
    assert_eq!(objects(remote.path(), "-tree-").len(), 2);

    let (b, _) = machine(remote.path(), None, vec![public]);
    let (outcome, summary) = b.run(&root, "use");
    assert_eq!(b.runs(), 0);
    assert_eq!(summary.hits, 2);
    assert!(summary.errors.is_empty(), "{summary:?}");
    let Some(Status::Cached { result, .. }) = outcome.statuses.get("use") else { panic!("{outcome:?}") };
    assert!(!b.store.contains(&result.outputs["out"]), "a hit fetches no content");
}

#[test]
fn content_is_fetched_when_an_action_that_runs_needs_it() {
    let remote = TempDir::new().unwrap();
    let (a, root, public) = writer(remote.path());
    a.run(&root, "make");

    let (b, _) = machine(remote.path(), None, vec![public]);
    let (outcome, summary) = b.run(&root, "use");
    assert_eq!(b.runs(), 1, "only `use` runs; `make` is a hit");
    assert!(summary.downloaded_bytes > 0);
    let Some(Status::Ran { result, .. }) = outcome.statuses.get("use") else { panic!("{outcome:?}") };
    let out = b.store.materialize(&result.outputs["out"]).unwrap();
    assert_eq!(fs::read_to_string(out.join("twice")).unwrap(), "42\n42\n");
}

#[test]
fn an_entry_whose_content_is_gone_is_a_miss_and_the_content_comes_back() {
    let remote = TempDir::new().unwrap();
    let (a, root, public) = writer(remote.path());
    a.run(&root, "make");
    for tree in objects(remote.path(), "-tree-") {
        fs::remove_file(tree).unwrap();
    }

    let signer = key("ci2");
    let mut trusted = vec![public];
    trusted.push(signer.public());
    let (b, _) = machine(remote.path(), Some(signer), trusted);
    let (_, summary) = b.run(&root, "make");
    assert_eq!(b.runs(), 1);
    assert_eq!(summary.incomplete, 1);
    assert_eq!(objects(remote.path(), "-tree-").len(), 1, "the rerun uploads the content again");
}

#[test]
fn entries_need_a_trusted_and_matching_signature() {
    let remote = TempDir::new().unwrap();
    let (a, root, _) = writer(remote.path());
    a.run(&root, "make");

    let (stranger, _) = machine(remote.path(), None, vec![key("ci").public()]);
    let (_, summary) = stranger.run(&root, "make");
    assert_eq!(stranger.runs(), 1, "a key with the same name but other bytes is not trusted");
    assert!(summary.errors[0].contains("does not match"), "{summary:?}");

    let (other, _) = machine(remote.path(), None, vec![key("other").public()]);
    let (_, summary) = other.run(&root, "make");
    assert_eq!(other.runs(), 1);
    assert!(summary.errors[0].contains("not a trusted key"), "{summary:?}");
}

#[test]
fn a_changed_entry_is_refused() {
    let remote = TempDir::new().unwrap();
    let (a, root, public) = writer(remote.path());
    a.run(&root, "make");
    let entry = objects(remote.path(), "-result-").pop().unwrap();
    let mut wire: serde_json::Value = serde_json::from_slice(&fs::read(&entry).unwrap()).unwrap();
    wire["key"] = serde_json::Value::String("ci".to_owned());
    wire["signature"] = serde_json::Value::String("AAAA".to_owned());
    fs::write(&entry, serde_json::to_vec(&wire).unwrap()).unwrap();

    let (b, _) = machine(remote.path(), None, vec![public]);
    let (_, summary) = b.run(&root, "make");
    assert_eq!(b.runs(), 1);
    assert_eq!(summary.hits, 0);
    assert_eq!(summary.errors.len(), 1, "{summary:?}");
}

#[test]
fn without_a_signing_key_nothing_is_written() {
    let remote = TempDir::new().unwrap();
    let (a, root) = machine(remote.path(), None, vec![]);
    let (_, summary) = a.run(&root, "make");
    assert_eq!(summary.uploads, 0);
    assert!(summary.not_writing.unwrap().contains("AZAD_SIGNING_KEY"));
    assert!(fs::read_dir(remote.path()).map(|d| d.count()).unwrap_or(0) == 0);
}

#[test]
fn keys_read_back_from_their_text() {
    let signer = key("laptop-1");
    let again: SigningKey = signer.to_secret_string().parse().unwrap();
    assert_eq!(again.public(), signer.public());
    let public: PublicKey = signer.public().to_string().parse().unwrap();
    assert_eq!(public, signer.public());
    assert!("no-colon".parse::<PublicKey>().is_err());
    assert!("bad name:AAAA".parse::<PublicKey>().is_err());
    let list = format!("# trusted\n\n{}\n", signer.public());
    assert_eq!(azad::remote::parse_public_keys(&list).unwrap(), [signer.public()]);
}

fn output_of(outcome: &azad::scheduler::Outcome, action: &str) -> Digest {
    match &outcome.statuses[action] {
        Status::Ran { result, .. } | Status::Cached { result, .. } => result.outputs["out"],
        other => panic!("{action}: {other:?}"),
    }
}

#[test]
fn a_pack_with_other_content_than_its_name_is_refused() {
    let remote = TempDir::new().unwrap();
    let (a, root, public) = writer(remote.path());
    let (outcome, _) = a.run(&root, "use");
    let tree_object = |action: &str| remote.path().join(format!("azad1-tree-{}", output_of(&outcome, action).hex()));
    // `make`'s name now holds a valid pack of `use`'s tree, and `use` must run again.
    fs::copy(tree_object("use"), tree_object("make")).unwrap();
    for entry in objects(remote.path(), "-result-") {
        let wire: serde_json::Value = serde_json::from_slice(&fs::read(&entry).unwrap()).unwrap();
        let is_make = outcome.statuses.iter().any(|(name, status)| {
            name == "make" && matches!(status, Status::Ran { action, .. } if action.to_string() == wire["action"])
        });
        if !is_make {
            fs::remove_file(entry).unwrap();
        }
    }

    let (b, _) = machine(remote.path(), None, vec![public]);
    let graph = graph(&root);
    let outcome = Runner::new(&graph, &b.backend, 1).run("use", &mut |_| {}).unwrap();
    assert!(matches!(outcome.statuses["make"], Status::Cached { .. }));
    let Status::Failed { reason, .. } = &outcome.statuses["use"] else { panic!("{outcome:?}") };
    assert!(reason.contains("does not hold all of the tree"), "{reason}");
    assert_eq!(b.runs(), 0, "nothing runs on wrong content");
}

#[test]
fn image_entries_are_shared_signed_and_their_trees_made_only_when_needed() {
    let remote = TempDir::new().unwrap();
    let (a, root, public) = writer(remote.path());
    let name = "image-record docker.io/library/rust@sha256:abc linux/amd64";
    let image = Image { tree: root, env: BTreeMap::from([("PATH".to_owned(), ROOTFS_PATH.to_owned())]) };
    a.cache.upload_image(name, &image);

    let (stranger, _) = machine(remote.path(), None, vec![key("other").public()]);
    assert_eq!(stranger.cache.image(name), None);
    assert!(stranger.cache.take_summary().errors[0].contains("not a trusted key"));

    let (b, _) = machine(remote.path(), None, vec![public]);
    assert_eq!(b.cache.image(name), Some(image));
    assert_eq!(b.cache.image("another image"), None);
    assert_eq!(b.cache.take_summary().images, 1);

    let empty = TempDir::new().unwrap();
    let c = Store::open(empty.path().join("store")).unwrap();
    let config = RemoteConfig {
        provider: Box::new(DirectoryProvider::new(remote.path().to_path_buf())),
        read: true,
        write: false,
        signing_key: None,
        trusted: vec![],
    };
    let cache = RemoteCache::new(config, c.clone());
    let made = Arc::new(AtomicUsize::new(0));
    let (source_store, counter) = (b.store.clone(), made.clone());
    let target = c.clone();
    cache.defer(
        root,
        "an image",
        Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            let dir = source_store.materialize(&root).map_err(|e| e.to_string())?;
            target.ingest(&dir, &Selection::All).map(|_| ()).map_err(|e| e.to_string())
        }),
    );
    assert_eq!(made.load(Ordering::SeqCst), 0, "nothing is made before it is needed");
    cache.ensure_tree(&root).unwrap();
    assert_eq!(made.load(Ordering::SeqCst), 1);
    assert!(c.contains(&root));
}

/// A provider that refuses empty objects, as the GitHub Actions cache does.
struct RefusesEmpty(DirectoryProvider);

impl azad::remote::Provider for RefusesEmpty {
    fn name(&self) -> String {
        self.0.name()
    }
    fn contains(&self, key: &str) -> Result<bool, azad::remote::RemoteError> {
        self.0.contains(key)
    }
    fn get(&self, key: &str, dest: &Path) -> Result<bool, azad::remote::RemoteError> {
        self.0.get(key, dest)
    }
    fn put(&self, key: &str, source: &Path) -> Result<(), azad::remote::RemoteError> {
        if fs::metadata(source).unwrap().len() == 0 {
            return Err(azad::remote::RemoteError::Http(format!("{key}: size must be between 1 and ...")));
        }
        self.0.put(key, source)
    }
}

#[test]
fn empty_logs_are_not_stored_and_still_read_back() {
    let remote = TempDir::new().unwrap();
    let signer = key("ci");
    let public = signer.public();
    let (a, root) = refusing_empty(remote.path(), Some(signer), vec![]);
    let (outcome, summary) = a.run(&root, "make");
    assert!(summary.errors.is_empty(), "{summary:?}");
    assert_eq!(objects(remote.path(), "-result-").len(), 1);

    let (b, _) = refusing_empty(remote.path(), None, vec![public]);
    let (_, summary) = b.run(&root, "make");
    assert_eq!(summary.hits, 1);
    let Some(Status::Ran { result, .. }) = outcome.statuses.get("make") else { panic!("{outcome:?}") };
    b.cache.ensure_blob(&result.stdout).unwrap();
    assert_eq!(b.store.read(&result.stdout).unwrap(), b"");
}

/// Actions that run at the same time can need the same image, which the remote cache knows but which is not pulled
/// yet: one pulls it, and the others wait for that pull instead of failing. The timings show the one pull, and the
/// wait of each action.
#[test]
fn actions_that_need_a_deferred_tree_at_the_same_time_wait_for_one_pull() {
    const PULL_TIME: Duration = NOTEWORTHY.saturating_add(Duration::from_millis(100));
    const ACTIONS: usize = 3;

    let remote = TempDir::new().unwrap();
    let (m, _) = machine(remote.path(), Some(key("ci")), Vec::new());
    let elsewhere = TempDir::new().unwrap();
    let image = elsewhere.path().join("image");
    fs::create_dir_all(&image).unwrap();
    fs::write(image.join("tool"), "a tool").unwrap();
    let tree = Store::open(elsewhere.path().join("store")).unwrap().ingest(&image, &Selection::All).unwrap();

    let pulls = Arc::new(AtomicUsize::new(0));
    let (store, counted) = (m.store.clone(), pulls.clone());
    m.cache.defer(
        tree,
        "an image",
        Box::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(PULL_TIME);
            store.ingest(&image, &Selection::All).map(|_| ()).map_err(|e| e.to_string())
        }),
    );
    let results: Vec<_> = std::thread::scope(|scope| {
        let waiting: Vec<_> = (0..ACTIONS).map(|_| scope.spawn(|| m.cache.ensure_tree(&tree))).collect();
        waiting.into_iter().map(|handle| handle.join().unwrap()).collect()
    });
    for result in results {
        result.unwrap();
    }
    assert_eq!(pulls.load(Ordering::SeqCst), 1);
    let steps: Vec<String> = m.timings.take_noteworthy().into_iter().map(|step| step.what).collect();
    let waits = vec!["wait for an image".to_owned(); ACTIONS];
    assert_eq!(steps.iter().filter(|what| *what == "pull an image").count(), 1, "{steps:?}");
    assert_eq!(steps.into_iter().filter(|what| what.starts_with("wait")).collect::<Vec<_>>(), waits);
}

/// An impure action that counts its runs in a cache volume, and adds 2 MB to it, as a build adds compiled code. Its
/// first run dates a file of the volume in 2000; later runs print `kept` when that file still has its date, which
/// tools such as Cargo compare.
const COUNTING: &str = "n=$(cat /state/count 2>/dev/null || echo 0); n=$((n+1)); echo $n > /state/count; \
                  head -c 2000000 /dev/zero > /state/build-$n; \
                  touch -d @1000000000 /tmp/2001; \
                  if [ -f /state/first ] && [[ /state/first -ot /tmp/2001 ]]; then echo kept; fi; \
                  [ -f /state/first ] || touch -d @946684800 /state/first; echo count=$n";

/// An action that changes only a small file of its volume, as a tool changes its own records; the copy in the cache is
/// as good, so none is stored.
const RECORDING: &str = "echo again >> /state/last-run";

fn volume_graph(root: &Digest, script: &str) -> Graph {
    let document = format!(
        "action count\n  kind impure\n  platform linux/amd64\n  root {root}\n  env PATH \"{ROOTFS_PATH}\"\n  command \"bash\" \"-c\" {script:?}\n  cache /state \"counter\"\n\ngoal count count\n"
    );
    text::parse(&document).unwrap().resolve(|_| Err::<Digest, ()>(())).unwrap()
}

impl Machine {
    fn count(&self, root: &Digest) -> (String, RemoteSummary) {
        self.with_volume(root, COUNTING)
    }

    fn with_volume(&self, root: &Digest, script: &str) -> (String, RemoteSummary) {
        let graph = volume_graph(root, script);
        let outcome = Runner::new(&graph, &self.backend, 1).run("count", &mut |_| {}).unwrap();
        self.cache.store_kept();
        let Some(Status::Ran { result, .. }) = outcome.statuses.get("count") else { panic!("{outcome:?}") };
        let stdout = String::from_utf8(self.store.read(&result.stdout).unwrap()).unwrap();
        (stdout, self.cache.take_summary())
    }
}

#[test]
fn a_new_machine_starts_with_the_newest_copy_of_a_volume_and_its_times() {
    let remote = TempDir::new().unwrap();
    let (a, root, public) = writer(remote.path());
    assert_eq!(a.count(&root).0, "count=1\n");
    let (output, summary) = a.count(&root);
    assert_eq!(output, "kept\ncount=2\n");
    assert_eq!((summary.volumes_restored, summary.volumes_stored), (0, 1));
    assert_eq!(objects(remote.path(), "-volume-").len(), 2, "each run stores a new copy");

    let (b, root_b, _) = writer(remote.path());
    let (output, summary) = b.count(&root_b);
    assert_eq!(output, "kept\ncount=3\n", "the copy of the second run, with the times of its files");
    assert_eq!(summary.volumes_restored, 1);

    let (_, summary) = b.with_volume(&root_b, RECORDING);
    assert_eq!(summary.volumes_stored, 0, "a run that changes only a small file stores no copy");

    let (reader, root_r) = machine(remote.path(), None, vec![public]);
    let (_, summary) = reader.count(&root_r);
    assert_eq!(summary.volumes_stored, 0, "a run without a signing key stores no volume");
}

/// A deferred tree, such as an image, comes from the cache when the cache has its pack, which is faster than a
/// registry; the first machine that makes it from its source stores the pack.
#[test]
fn a_deferred_tree_comes_from_the_cache_after_one_machine_made_it() {
    let remote = TempDir::new().unwrap();
    let elsewhere = TempDir::new().unwrap();
    let image = elsewhere.path().join("image");
    fs::create_dir_all(&image).unwrap();
    fs::write(image.join("tool"), "a tool").unwrap();
    let tree = Store::open(elsewhere.path().join("store")).unwrap().ingest(&image, &Selection::All).unwrap();
    let pulls = Arc::new(AtomicUsize::new(0));
    let defer = |m: &Machine| {
        let (store, counted, image) = (m.store.clone(), pulls.clone(), image.clone());
        m.cache.defer(
            tree,
            "an image",
            Box::new(move || {
                counted.fetch_add(1, Ordering::SeqCst);
                store.ingest(&image, &Selection::All).map(|_| ()).map_err(|e| e.to_string())
            }),
        );
    };

    let (first, _, public) = writer(remote.path());
    defer(&first);
    first.cache.ensure_tree(&tree).unwrap();
    assert_eq!(pulls.load(Ordering::SeqCst), 1);
    assert!(
        objects(remote.path(), &tree.hex()).is_empty(),
        "the actions that wait for the tree do not wait for the upload"
    );
    first.cache.store_kept();
    assert_eq!(objects(remote.path(), &tree.hex()).len(), 1, "the end of the run stores the pack");

    let (second, _) = machine(remote.path(), None, vec![public]);
    defer(&second);
    second.cache.ensure_tree(&tree).unwrap();
    assert_eq!(pulls.load(Ordering::SeqCst), 1, "the second machine gets the tree from the cache");
    assert!(second.store.contains(&tree));
}
