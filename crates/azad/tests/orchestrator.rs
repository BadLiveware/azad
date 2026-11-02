//! The orchestrator (decision 0020) in a local git repository: events in GitHub's format become check runs for the goals
//! that the repository names for them, at the trust level of the event.
#![allow(clippy::unwrap_used)]

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use azad::engine::{Engine, EngineConfig, Status, host_platform};
use azad::executor::{ExecutorService, ServiceSandbox};
use azad::ir::Image;
use azad::oci::Puller;
use azad::orchestrator::{
    CheckRun, CheckStatus, Conclusion, Orchestrator, SIGNATURE_HEADER, Simulator, fake_github, sign,
};
use azad::sandbox::Limits;
use azad::store::{Selection, Store};
use azad::trust::Trust;
use support::{ROOTFS_PATH, TEST_PROGRAMS, host_rootfs};
use tempfile::TempDir;

const SECRET: &[u8] = b"a shared secret";
const IMAGE: &str = "tools@sha256:00000000000000000000000000000000000000000000000000000000000000cc";
const TASKS: &str = r#"
IMAGE = image("tools@sha256:00000000000000000000000000000000000000000000000000000000000000cc")
lint = task(name = "lint", cmd = "cat lint/rules", sources = ["lint/**"], image = IMAGE)
build = task(name = "build", cmd = "mkdir -p out && cat src/a.txt > out/a.txt", sources = ["src/**"], generates = ["out"], image = IMAGE)
goal("check", require = [lint, build], on = ["push", "pull_request"])
goal("by-hand", require = [lint])
"#;
const WAIT: Duration = Duration::from_secs(60);

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git").arg("-C").arg(repo).args(args).output().unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn commit(repo: &Path, files: &[(&str, &str)], message: &str) -> String {
    for (path, content) in files {
        let path = repo.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
    git(repo, &["add", "-A"]);
    git(repo, &["-c", "user.name=test", "-c", "user.email=test@example.com", "commit", "-q", "-m", message]);
    git(repo, &["rev-parse", "HEAD"])
}

struct Fixture {
    dir: TempDir,
    repo: std::path::PathBuf,
    url: String,
    executor: Option<std::path::PathBuf>,
}

impl Fixture {
    fn config(&self, root: &Path, trust: Trust) -> EngineConfig {
        EngineConfig {
            store: self.dir.path().join("store"),
            root: root.to_path_buf(),
            platform: host_platform(),
            limits: Limits { timeout: Duration::from_secs(30), ..Limits::default() },
            tools: BTreeMap::new(),
            isolated: true,
            color: false,
            force: false,
            sandbox: azad::SandboxKind::Bubblewrap,
            executor: self.executor.clone(),
            trust,
            history: None,
            withheld: Default::default(),
            filter: Default::default(),
            local: None,
        }
    }

    fn simulator(&self, secret: &[u8]) -> Simulator {
        Simulator::new(&self.repo, &self.url, secret, "main").unwrap()
    }

    /// The check runs of a commit, once all of them completed.
    fn completed(&self, sha: &str) -> Vec<CheckRun> {
        let deadline = Instant::now() + WAIT;
        loop {
            let runs: Vec<CheckRun> = fake_github(&self.dir.path().join("store"))
                .check_runs()
                .unwrap()
                .into_iter()
                .filter(|run| run.head_sha == sha)
                .collect();
            if !runs.is_empty() && runs.iter().all(|run| run.status == CheckStatus::Completed) {
                return runs;
            }
            assert!(Instant::now() < deadline, "the check runs of {sha} did not complete: {runs:?}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// A repository with `TASKS` on `main`, and a store that knows `IMAGE`.
fn fixture() -> Fixture {
    let dir = TempDir::new().unwrap();
    let rootfs = dir.path().join("rootfs");
    host_rootfs(&rootfs, TEST_PROGRAMS);
    let store = Store::open(dir.path().join("store")).unwrap();
    let tree = store.ingest(&rootfs, &Selection::All).unwrap();
    let env = BTreeMap::from([("PATH".to_owned(), ROOTFS_PATH.to_owned())]);
    Puller::new(store).unwrap().remember(IMAGE, &host_platform(), &Image { tree, env }).unwrap();

    let repo = dir.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    commit(&repo, &[("tasks.star", TASKS), ("lint/rules", "no tabs\n"), ("src/a.txt", "a\n")], "first");

    Fixture { dir, repo, url: String::new(), executor: None }
}

impl Fixture {
    /// Starts an executor service on the store, through which the orchestrator runs its actions.
    fn start_executor(&mut self) {
        let socket = self.dir.path().join("executor.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let store = Store::open(self.dir.path().join("store")).unwrap();
        let limits = Limits { timeout: Duration::from_secs(30), ..Limits::default() };
        let service = ExecutorService::new(store, ServiceSandbox::Bubblewrap, limits);
        std::thread::spawn(move || service.serve(&listener));
        self.executor = Some(socket);
    }

    /// Starts an orchestrator on the store, which serves until the test ends.
    fn serve(&mut self) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        self.url = format!("http://{}/webhook", listener.local_addr().unwrap());
        let orchestrator = Orchestrator::new(self.config(self.dir.path(), Trust::Main), SECRET);
        std::thread::spawn(move || orchestrator.serve(&listener));
    }
}

fn plan(f: &Fixture, trust: Trust) -> BTreeMap<String, &'static str> {
    let engine = Engine::new(f.config(&f.repo, trust)).unwrap();
    let goal = engine.load("//:check", &mut |_| {}).unwrap();
    let outcome = engine.plan(&goal).unwrap();
    outcome
        .statuses
        .into_iter()
        .filter_map(|(label, status)| match status {
            Status::Cached { .. } => Some((label, "cached")),
            Status::WillRun { .. } => Some((label, "will run")),
            _ => None,
        })
        .collect()
}

/// What the local orchestrator must do, with the actions running through an executor service.
#[test]
fn a_pull_request_gets_hits_from_main_and_never_writes_mains_cache() {
    let mut f = fixture();
    f.start_executor();
    f.serve();
    let main = git(&f.repo, &["rev-parse", "HEAD"]);
    f.simulator(SECRET).push("main").unwrap();
    let checks = f.completed(&main);
    let names: Vec<&str> = checks.iter().map(|run| run.name.as_str()).collect();
    assert_eq!(names, ["//:check"], "only the goals that the event names run");
    assert_eq!(checks[0].conclusion, Some(Conclusion::Success), "{}", checks[0].summary);
    assert_eq!(checks[0].summary, "2 ran, 0 cached, 0 failed");

    git(&f.repo, &["checkout", "-q", "-b", "change"]);
    let change = commit(&f.repo, &[("src/a.txt", "a, changed\n")], "change a");
    f.simulator(SECRET).pull_request(1, "change").unwrap();
    let checks = f.completed(&change);
    assert_eq!(checks[0].conclusion, Some(Conclusion::Success), "{}", checks[0].summary);
    assert_eq!(checks[0].summary, "1 ran, 1 cached, 0 failed", "lint is a hit from main's run");

    let from = |pairs: &[(&str, &'static str)]| pairs.iter().map(|(l, s)| ((*l).to_owned(), *s)).collect();
    assert_eq!(
        plan(&f, Trust::Main),
        from(&[("//:build", "will run"), ("//:lint", "cached")]),
        "main never reads the pull request's build"
    );
    assert_eq!(plan(&f, Trust::PullRequest), from(&[("//:build", "cached"), ("//:lint", "cached")]));
}

#[test]
fn a_delivery_with_a_wrong_signature_is_refused_and_runs_nothing() {
    let mut f = fixture();
    f.serve();
    let error = f.simulator(b"another secret").push("main").unwrap_err();
    assert!(error.to_string().contains("401"), "{error}");
    std::thread::sleep(Duration::from_millis(200));
    assert!(fake_github(&f.dir.path().join("store")).check_runs().unwrap().is_empty());
}

/// A run that was accepted before the orchestrator stopped runs when it starts again; while one orchestrator serves a
/// store, another cannot.
#[test]
fn a_run_that_was_accepted_runs_after_a_restart() {
    let f = fixture();
    let sha = git(&f.repo, &["rev-parse", "HEAD"]);
    let stopped = Orchestrator::new(f.config(f.dir.path(), Trust::Main), SECRET);
    let payload = serde_json::json!({
        "ref": "refs/heads/feature", "after": sha, "deleted": false,
        "repository": { "full_name": "local/repo", "clone_url": f.repo.display().to_string(), "default_branch": "main" },
    });
    git(&f.repo, &["branch", "feature"]);
    let payload = serde_json::to_vec(&payload).unwrap();
    let headers = BTreeMap::from([
        ("x-github-event".to_owned(), "push".to_owned()),
        (SIGNATURE_HEADER.to_owned(), sign(SECRET, &payload)),
    ]);
    assert_eq!(stopped.receive(&headers, &payload).status, 202);
    drop(stopped);

    let mut f = f;
    f.serve();
    let checks = f.completed(&sha);
    assert_eq!(checks.len(), 1);
    let second = Orchestrator::new(f.config(f.dir.path(), Trust::Main), SECRET);
    let error = second.serve(&TcpListener::bind("127.0.0.1:0").unwrap()).unwrap_err();
    assert!(error.to_string().contains("another orchestrator"), "{error}");
}
