#![allow(clippy::unwrap_used)]

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use azad::ir::{Action, Digest, Kind, Spec};
use azad::local::{ExecError, LocalBackend};
use azad::sandbox::{Bubblewrap, Exit, Limits, Sandbox, SandboxError, SandboxSpec};
use azad::store::{Selection, Store};
use azad::trust::Trust;
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

struct Fixture {
    dir: TempDir,
    store: Store,
    executor: LocalBackend,
    runs: Arc<AtomicUsize>,
    root: Digest,
}

fn fixture_with(limits: Limits) -> Fixture {
    let dir = TempDir::new().unwrap();
    let store = Store::open(dir.path().join("store")).unwrap();
    let rootfs = dir.path().join("rootfs");
    host_rootfs(&rootfs, TEST_PROGRAMS);
    let root = store.ingest(&rootfs, &Selection::All).unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let sandbox = Counting { inner: Bubblewrap::default(), runs: runs.clone() };
    let executor = LocalBackend::new(store.clone(), Box::new(sandbox), limits).unwrap();
    Fixture { dir, store, executor, runs, root }
}

fn fixture() -> Fixture {
    fixture_with(Limits { timeout: Duration::from_secs(30), ..Limits::default() })
}

fn action(f: &Fixture, kind: Kind, script: &str, outputs: &[&str]) -> Action {
    Spec {
        kind,
        platform: "linux/amd64".to_owned(),
        root: f.root,
        command: vec!["bash".to_owned(), "-c".to_owned(), script.to_owned()],
        env: BTreeMap::from([("PATH".to_owned(), ROOTFS_PATH.to_owned())]),
        inputs: BTreeMap::new(),
        outputs: outputs.iter().map(|s| (*s).to_owned()).collect::<BTreeSet<_>>(),
        caches: BTreeMap::new(),
        work: BTreeMap::new(),
        dir: String::new(),
        secrets: Default::default(),
    }
}

fn stdout(f: &Fixture, digest: &Digest) -> String {
    String::from_utf8(f.store.read(digest).unwrap()).unwrap()
}

#[test]
fn an_action_runs_and_a_second_run_is_a_cache_hit() {
    let f = fixture();
    let a = action(&f, Kind::Hermetic, "mkdir out && echo hello > out/greeting && echo done", &["out"]);
    let first = f.executor.execute(&a).unwrap();
    assert!(first.result.succeeded() && !first.cached);
    assert_eq!(stdout(&f, &first.result.stdout), "done\n");

    let copy = f.dir.path().join("copy");
    f.store.materialize_into(&first.result.outputs["out"], &copy).unwrap();
    assert_eq!(fs::read_to_string(copy.join("greeting")).unwrap(), "hello\n");

    let second = f.executor.execute(&a).unwrap();
    assert!(second.cached);
    assert_eq!(second.result, first.result);
    assert_eq!(f.runs.load(Ordering::SeqCst), 1, "a cache hit does not start a sandbox");
}

#[test]
fn a_declared_input_can_be_read_but_not_written() {
    let f = fixture();
    let src = f.dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("input.txt"), "from the host").unwrap();
    let mut a = action(&f, Kind::Hermetic, "cat /src/input.txt; touch /src/new 2>/dev/null && echo WROTE", &[]);
    a.inputs.insert("/src".to_owned(), f.store.ingest(&src, &Selection::All).unwrap());
    let run = f.executor.execute(&a).unwrap();
    assert_eq!(stdout(&f, &run.result.stdout), "from the host");
}

#[test]
fn an_undeclared_file_cannot_be_read() {
    let f = fixture();
    let secret = f.dir.path().join("secret.txt");
    fs::write(&secret, "secret").unwrap();
    let a = action(&f, Kind::Hermetic, &format!("cat {}", secret.display()), &[]);
    let run = f.executor.execute(&a).unwrap();
    assert!(!run.result.succeeded());
    assert_eq!(stdout(&f, &run.result.stdout), "");
}

#[test]
fn a_hermetic_action_has_no_network_and_an_impure_action_has() {
    let f = fixture();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let script = format!("exec 3<>/dev/tcp/127.0.0.1/{port} && echo connected");

    let hermetic = f.executor.execute(&action(&f, Kind::Hermetic, &script, &[])).unwrap();
    assert!(!hermetic.result.succeeded());

    let impure = f.executor.execute(&action(&f, Kind::Impure, &script, &[])).unwrap();
    assert_eq!(stdout(&f, &impure.result.stdout), "connected\n");
}

#[test]
fn a_failed_action_is_not_cached() {
    let f = fixture();
    let a = action(&f, Kind::Hermetic, "exit 3", &[]);
    assert_eq!(f.executor.execute(&a).unwrap().result.exit_code, 3);
    assert!(!f.executor.execute(&a).unwrap().cached);
    assert_eq!(f.runs.load(Ordering::SeqCst), 2);
}

#[test]
fn an_impure_action_is_never_cached() {
    let f = fixture();
    let a = action(&f, Kind::Impure, "true", &[]);
    f.executor.execute(&a).unwrap();
    assert!(!f.executor.execute(&a).unwrap().cached);
}

#[test]
fn outputs_must_be_declared_and_made() {
    let f = fixture();
    let undeclared = action(&f, Kind::Hermetic, "mkdir -p out stray/deep && touch note", &["out"]);
    let run = f.executor.execute(&undeclared).unwrap();
    assert!(run.result.succeeded(), "undeclared paths do not fail the action");
    assert_eq!(run.discarded, ["note", "stray"]);
    assert!(f.executor.execute(&undeclared).unwrap().discarded.is_empty(), "a cache hit discards nothing");
    let missing = action(&f, Kind::Hermetic, "true", &["out"]);
    assert!(matches!(f.executor.execute(&missing), Err(ExecError::MissingOutput(p)) if p == "out"));
    let nested = action(&f, Kind::Hermetic, "mkdir -p a/b && touch a/b/x", &["a/b"]);
    assert!(f.executor.execute(&nested).unwrap().result.succeeded());
}

#[test]
fn a_fixed_output_must_match_its_expected_digest() {
    let f = fixture();
    let expected = BTreeMap::from([("out".to_owned(), Digest::of_bytes(b"something else"))]);
    let a = action(&f, Kind::FixedOutput { expected }, "mkdir out && echo x > out/x", &["out"]);
    assert!(matches!(f.executor.execute(&a), Err(ExecError::OutputMismatch { .. })));
}

#[test]
fn an_action_that_runs_too_long_is_stopped() {
    let f = fixture_with(Limits { timeout: Duration::from_millis(300), ..Limits::default() });
    let a = action(&f, Kind::Hermetic, "sleep 30", &[]);
    assert!(matches!(f.executor.execute(&a), Err(ExecError::TimedOut(_))));
}

#[test]
fn a_command_that_does_not_exist_is_reported_as_not_found() {
    let f = fixture();
    let mut a = action(&f, Kind::Hermetic, "", &[]);
    a.command = vec!["/no/such/program".to_owned()];
    let error = f.executor.execute(&a).unwrap_err().to_string();
    assert_eq!(error, "the command /no/such/program is not found in the root file system or the inputs of the action");

    a.command = vec!["python".to_owned()];
    let error = f.executor.execute(&a).unwrap_err().to_string();
    assert_eq!(error, format!("the command python is not found in the PATH of the action: {ROOTFS_PATH}"));

    a.env.clear();
    let error = f.executor.execute(&a).unwrap_err().to_string();
    assert_eq!(error, "the command python is not found, and the action has no PATH");
}

#[test]
fn an_input_cannot_hide_a_path_of_the_root_image() {
    let f = fixture();
    let mut a = action(&f, Kind::Hermetic, "true", &[]);
    a.inputs.insert("/usr".to_owned(), f.root);
    assert!(matches!(f.executor.execute(&a), Err(ExecError::InputHidesRoot(p)) if p == "/usr"));
}

#[test]
fn tmp_is_writable_scratch_space_that_is_not_kept() {
    let f = fixture();
    let a = action(&f, Kind::Hermetic, "echo scratch > /tmp/x && cat /tmp/x", &[]);
    let run = f.executor.execute(&a).unwrap();
    assert_eq!(stdout(&f, &run.result.stdout), "scratch\n");
    assert!(run.discarded.is_empty());
}

#[test]
fn a_cache_volume_keeps_its_content_between_actions() {
    let f = fixture();
    let mut write = action(&f, Kind::Hermetic, "echo warm > /cache/state", &[]);
    write.caches.insert("/cache".to_owned(), "shared".to_owned());
    assert!(f.executor.execute(&write).unwrap().result.succeeded());

    let mut read = action(&f, Kind::Hermetic, "cat /cache/state", &[]);
    read.caches.insert("/cache".to_owned(), "shared".to_owned());
    let run = f.executor.execute(&read).unwrap();
    assert_eq!(stdout(&f, &run.result.stdout), "warm\n");
}

#[test]
fn a_held_volume_is_not_shared() {
    let f = fixture();
    let mut write = action(&f, Kind::Hermetic, "echo warm > /cache/state", &[]);
    write.caches.insert("/cache".to_owned(), "busy".to_owned());
    f.executor.execute(&write).unwrap();

    let held = f.store.cache_volume("busy", "linux/amd64", Trust::Main).unwrap();
    assert!(held.is_shared());
    let mut read = action(&f, Kind::Hermetic, "cat /cache/state 2>/dev/null || echo cold", &[]);
    read.caches.insert("/cache".to_owned(), "busy".to_owned());
    let run = f.executor.execute(&read).unwrap();
    assert_eq!(stdout(&f, &run.result.stdout), "cold\n", "a held volume gives a private, empty one");
    drop(held);
}

/// A pull request never writes a volume that `main` reads, and a run of the level `none` gets an empty volume each
/// time (decision 0019).
#[test]
fn the_volumes_of_each_trust_level_are_kept_apart() {
    let f = fixture();
    let at = |trust| {
        let sandbox = Bubblewrap::default();
        let limits = Limits { timeout: Duration::from_secs(30), ..Limits::default() };
        LocalBackend::new(f.store.clone(), Box::new(sandbox), limits).unwrap().with_trust(trust)
    };
    let script = |value: &str| format!("cat /cache/state 2>/dev/null || echo cold; echo {value} > /cache/state");
    let run = |trust, value: &str| {
        let mut a = action(&f, Kind::Hermetic, &script(value), &[]);
        a.caches.insert("/cache".to_owned(), "state".to_owned());
        let execution = at(trust).execute_uncached(&a, &azad::log::Discard).unwrap();
        stdout(&f, &execution.result.stdout)
    };
    assert_eq!(run(Trust::Main, "main"), "cold\n");
    assert_eq!(run(Trust::PullRequest, "pull request"), "cold\n", "a pull request does not read main's volume");
    assert_eq!(run(Trust::Main, "main again"), "main\n", "a pull request did not write main's volume");
    assert_eq!(run(Trust::PullRequest, "pull request again"), "pull request\n");
    assert_eq!(run(Trust::None, "none"), "cold\n");
    assert_eq!(run(Trust::None, "none again"), "cold\n", "a volume of the level none is not kept");
    let levels: BTreeSet<Trust> = f.store.caches().unwrap().into_iter().map(|info| info.trust).collect();
    assert_eq!(levels, BTreeSet::from([Trust::Main, Trust::PullRequest]));
}

/// A pull request gets hits from `main`'s results, but its own results never reach `main`; a run of the level `none`
/// reads `main`'s results and keeps none of its own (decision 0020).
#[test]
fn the_action_cache_of_each_trust_level_reads_main_and_writes_only_its_own() {
    let f = fixture();
    let at = |trust| {
        let limits = Limits { timeout: Duration::from_secs(30), ..Limits::default() };
        LocalBackend::new(f.store.clone(), Box::new(Bubblewrap::default()), limits).unwrap().with_trust(trust)
    };
    let (main, pull_request, none) = (at(Trust::Main), at(Trust::PullRequest), at(Trust::None));
    let on_main = action(&f, Kind::Hermetic, "echo main", &[]);
    let on_pull_request = action(&f, Kind::Hermetic, "echo pull request", &[]);
    let on_none = action(&f, Kind::Hermetic, "echo none", &[]);

    assert!(!main.execute(&on_main).unwrap().cached);
    assert!(pull_request.execute(&on_main).unwrap().cached, "a pull request reads main's results");
    assert!(none.execute(&on_main).unwrap().cached, "the level none reads main's results");

    assert!(!pull_request.execute(&on_pull_request).unwrap().cached);
    assert!(pull_request.cached(&on_pull_request).unwrap().is_some(), "a pull request reads its own results");
    assert!(main.cached(&on_pull_request).unwrap().is_none(), "main never reads a pull request's result");
    assert!(none.cached(&on_pull_request).unwrap().is_none());

    none.execute(&on_none).unwrap();
    assert!(none.cached(&on_none).unwrap().is_none(), "the level none keeps no results");
    assert!(main.cached(&on_none).unwrap().is_none());
}

#[test]
fn read_only_scratch_is_removed_after_a_run() {
    let f = fixture();
    let a = action(
        &f,
        Kind::Hermetic,
        "mkdir -p /tmp/locked/inner && touch /tmp/locked/inner/f && chmod -R a-w /tmp/locked",
        &[],
    );
    assert!(f.executor.execute(&a).unwrap().result.succeeded());
    let leftovers: Vec<_> = fs::read_dir(f.store.tmp_dir()).unwrap().collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

fn source(f: &Fixture, name: &str, content: &str) -> Digest {
    let dir = f.dir.path().join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("f"), content).unwrap();
    f.store.ingest(&dir, &azad::store::Selection::All).unwrap()
}

/// Whether `/src/f` is newer than the last build recorded in the volume at `/b`, after a build there.
fn newer_than_build(f: &Fixture, src: Digest, caches: &[(&str, &str)]) -> String {
    let script = "if [[ /src/f -nt /b/built ]]; then echo newer; else echo older; fi; sleep 0.01; touch /b/built";
    let mut a = action(f, Kind::Impure, script, &[]);
    a.inputs.insert("/src".to_owned(), src);
    a.caches = caches.iter().map(|(path, name)| ((*path).to_owned(), (*name).to_owned())).collect();
    stdout(f, &f.executor.execute(&a).unwrap().result.stdout).trim().to_owned()
}

#[test]
fn inputs_are_newer_than_a_volume_state_built_from_other_content() {
    let f = fixture();
    let one = source(&f, "one", "1");
    let two = source(&f, "two", "2");
    let both = [("/a", "first"), ("/b", "second")];
    newer_than_build(&f, one, &both);
    assert_eq!(newer_than_build(&f, one, &both), "older", "unchanged input keeps its time");
    assert_eq!(newer_than_build(&f, two, &[("/b", "second")]), "newer", "a new checkout");
    assert_eq!(
        newer_than_build(&f, one, &both),
        "newer",
        "the volume at /b was last built from other content, so the input must look changed"
    );
}
