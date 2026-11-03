//! The orchestrator (decision 0020) in a local git repository: events in GitHub's format become check runs for the goals
//! that the repository names for them, at the trust level of the event.
#![allow(clippy::unwrap_used)]

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use azad::engine::{Engine, EngineConfig, Status, host_platform};
use azad::executor::{ExecutorService, ServiceSandbox};
use azad::ir::Image;
use azad::oci::Puller;
use azad::orchestrator::{
    CheckRun, CheckStatus, Conclusion, GitHubApp, Orchestrator, SIGNATURE_HEADER, Simulator, fake_github, sign,
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
        let orchestrator = Orchestrator::new(self.config(self.dir.path(), Trust::Main), SECRET);
        self.serve_with(orchestrator);
    }

    fn serve_with(&mut self, orchestrator: Orchestrator) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        self.url = format!("http://{}/webhook", listener.local_addr().unwrap());
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

/// A tag runs at the trust level `main` only when the default branch holds its commit (decision 0032): a tag of
/// another commit writes nothing that `main` reads.
#[test]
fn a_tag_on_the_default_branch_runs_as_main_and_another_tag_writes_nothing() {
    let mut f = fixture();
    f.serve();
    let main = git(&f.repo, &["rev-parse", "HEAD"]);
    git(&f.repo, &["checkout", "-q", "-b", "side"]);
    git(
        &f.repo,
        &["-c", "user.name=test", "-c", "user.email=test@example.com", "commit", "-q", "--allow-empty", "-m", "side"],
    );
    let side = git(&f.repo, &["rev-parse", "HEAD"]);
    git(&f.repo, &["tag", "v0-side"]);
    f.simulator(SECRET).tag("v0-side").unwrap();
    assert_eq!(f.completed(&side)[0].conclusion, Some(Conclusion::Success));
    let from = |pairs: &[(&str, &'static str)]| pairs.iter().map(|(l, s)| ((*l).to_owned(), *s)).collect();
    let nothing_known = from(&[("//:build", "will run"), ("//:lint", "will run")]);
    assert_eq!(plan(&f, Trust::Main), nothing_known, "a tag outside main runs at none");
    assert_eq!(plan(&f, Trust::PullRequest), nothing_known);

    git(&f.repo, &["tag", "v1", &main]);
    f.simulator(SECRET).tag("v1").unwrap();
    assert_eq!(f.completed(&main)[0].conclusion, Some(Conclusion::Success));
    assert_eq!(plan(&f, Trust::Main), from(&[("//:build", "cached"), ("//:lint", "cached")]));
}

const APP_ID: &str = "12345";
const APP_KEY: &str = include_str!("data/github-app-test-key.pem");
const INSTALLATION_TOKEN: &str = "ghs_installation";

/// A call to the fake GitHub API.
#[derive(Clone, Debug)]
struct ApiCall {
    method: String,
    path: String,
    authorization: String,
    body: serde_json::Value,
}

/// A fake of GitHub's REST API for one repository, `local/repo`, with one installation of the App: it checks that the
/// App signs in with a JWT that its key signed, and that the other calls carry the installation's token.
fn fake_api() -> (String, Arc<Mutex<Vec<ApiCall>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let calls = Arc::new(Mutex::new(Vec::new()));
    let recorded = calls.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let call = read_call(&stream);
            let (status, answer) = answer(&call);
            recorded.lock().unwrap().push(call);
            let body = answer.to_string();
            let response = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        }
    });
    (url, calls)
}

fn read_call(stream: &TcpStream) -> ApiCall {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let mut parts = line.split_whitespace();
    let (method, path) = (parts.next().unwrap().to_owned(), parts.next().unwrap().to_owned());
    let mut headers = BTreeMap::new();
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).unwrap();
        let Some((name, value)) = header.trim_end().split_once(": ") else { break };
        headers.insert(name.to_ascii_lowercase(), value.to_owned());
    }
    let length: usize = headers.get("content-length").map_or(0, |length| length.parse().unwrap());
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    let body = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
    ApiCall { method, path, authorization: headers.get("authorization").cloned().unwrap_or_default(), body }
}

fn answer(call: &ApiCall) -> (u16, serde_json::Value) {
    let app_signed_in = call.authorization.strip_prefix("Bearer ").is_some_and(is_app_token);
    let installation = call.authorization == format!("Bearer {INSTALLATION_TOKEN}");
    match (call.method.as_str(), call.path.as_str()) {
        ("GET", "/repos/local/repo/installation") if app_signed_in => (200, serde_json::json!({ "id": 7 })),
        ("POST", "/app/installations/7/access_tokens") if app_signed_in => {
            (201, serde_json::json!({ "token": INSTALLATION_TOKEN, "expires_at": "2099-01-01T00:00:00Z" }))
        }
        ("POST", "/repos/local/repo/check-runs") if installation => (201, serde_json::json!({ "id": 42 })),
        ("PATCH", "/repos/local/repo/check-runs/42") if installation => (200, serde_json::json!({ "id": 42 })),
        _ => (401, serde_json::json!({ "message": "Bad credentials" })),
    }
}

/// Whether `token` is a JWT that the App's key signed with RS256, issued by the App, and valid now.
fn is_app_token(token: &str) -> bool {
    use base64::Engine as _;
    use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
    let Some((signed, signature)) = token.rsplit_once('.') else { return false };
    let Some((_, claims)) = signed.split_once('.') else { return false };
    let der: String = APP_KEY.lines().filter(|line| !line.starts_with("-----")).collect();
    let pair = ring::signature::RsaKeyPair::from_der(&STANDARD.decode(der).unwrap()).unwrap();
    let public = ring::signature::UnparsedPublicKey::new(
        &ring::signature::RSA_PKCS1_2048_8192_SHA256,
        ring::signature::KeyPair::public_key(&pair).as_ref().to_vec(),
    );
    let verified = public.verify(signed.as_bytes(), &URL_SAFE_NO_PAD.decode(signature).unwrap_or_default()).is_ok();
    let claims: serde_json::Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(claims).unwrap()).unwrap();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let lifetime = claims["exp"].as_u64().unwrap() - claims["iat"].as_u64().unwrap();
    verified
        && claims["iss"] == APP_ID
        && claims["iat"].as_u64().unwrap() <= now
        && now < claims["exp"].as_u64().unwrap()
        && lifetime <= 600
}

/// Check runs go to GitHub through the App: it signs in with its key, gets the installation's token once, and creates
/// and completes the goal's check run with that token.
#[test]
fn check_runs_go_to_github_through_the_installation_of_the_app() {
    let mut f = fixture();
    let (api, calls) = fake_api();
    let app = GitHubApp::new(APP_ID, APP_KEY, &api).unwrap();
    f.serve_with(Orchestrator::with_github(f.config(f.dir.path(), Trust::Main), SECRET, app));
    let main = git(&f.repo, &["rev-parse", "HEAD"]);
    f.simulator(SECRET).push("main").unwrap();

    let deadline = Instant::now() + WAIT;
    let calls = loop {
        let calls = calls.lock().unwrap().clone();
        if calls.iter().any(|call| call.body["status"] == "completed") {
            break calls;
        }
        assert!(Instant::now() < deadline, "no check run completed: {calls:#?}");
        std::thread::sleep(Duration::from_millis(50));
    };
    let made: Vec<String> = calls.iter().map(|call| format!("{} {}", call.method, call.path)).collect();
    assert_eq!(
        made,
        [
            "GET /repos/local/repo/installation",
            "POST /app/installations/7/access_tokens",
            "POST /repos/local/repo/check-runs",
            "PATCH /repos/local/repo/check-runs/42",
            "PATCH /repos/local/repo/check-runs/42",
        ],
        "{calls:#?}"
    );
    assert_eq!(calls[2].body["name"], "//:check");
    assert_eq!(calls[2].body["head_sha"], main);
    assert_eq!(calls[3].body["status"], "in_progress");
    assert_eq!(calls[4].body["conclusion"], "success", "{:#?}", calls[4]);
    assert_eq!(calls[4].body["output"]["summary"], "2 ran, 0 cached, 0 failed");
}
