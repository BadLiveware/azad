#![allow(clippy::unwrap_used)]

mod support;

use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use azad::ir::text;
use azad::ir::{Digest, Graph};
use azad::local::LocalBackend;
use azad::sandbox::{Bubblewrap, Exit, Limits, Sandbox, SandboxError, SandboxSpec};
use azad::scheduler::{Outcome, Progress, Runner, Status};
use azad::store::{Selection, Store};
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

impl Fixture {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let store = Store::open(dir.path().join("store")).unwrap();
        let rootfs = dir.path().join("rootfs");
        host_rootfs(&rootfs, TEST_PROGRAMS);
        let root = store.ingest(&rootfs, &Selection::All).unwrap();
        let runs = Arc::new(AtomicUsize::new(0));
        let sandbox = Counting { inner: Bubblewrap::default(), runs: runs.clone() };
        let limits = Limits { timeout: Duration::from_secs(30), ..Limits::default() };
        let executor = LocalBackend::new(store.clone(), Box::new(sandbox), limits).unwrap();
        Fixture { dir, store, executor, runs, root }
    }

    fn write_source(&self, name: &str, content: &str) {
        let src = self.dir.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join(name), content).unwrap();
    }

    /// A graph of four actions: `lib` and `docs` read the sources, `app` reads `lib`, and `test` reads `app`.
    fn graph(&self) -> Graph {
        let action = |name: &str, script: &str, inputs: &[(&str, &str)]| {
            let mut text = format!(
                "action {name}\n  platform linux/amd64\n  root {root}\n  env PATH \"{ROOTFS_PATH}\"\n  command \"sh\" \"-c\" {script:?}\n  output out\n",
                root = self.root,
            );
            for (path, source) in inputs {
                text.push_str(&format!("  input {path} {source}\n"));
            }
            text
        };
        let document = [
            action("lib", "mkdir out && cat /src/lib.txt > out/lib", &[("/src", "local(\"src\")")]),
            action("docs", "mkdir out && cat /src/docs.txt > out/docs", &[("/src", "local(\"src\")")]),
            action(
                "app",
                "mkdir out && cat /in/lib/lib > out/app && echo app >> out/app",
                &[("/in/lib", "output(lib, \"out\")")],
            ),
            action("test", "mkdir out && cat /in/app/app > out/report", &[("/in/app", "output(app, \"out\")")]),
            "goal check test\ngoal all test docs\n".to_owned(),
        ]
        .join("\n");
        let src = self.dir.path().join("src");
        let graph = text::parse(&document)
            .unwrap()
            .resolve(|local| self.store.ingest(&self.dir.path().join(local), &Selection::All))
            .unwrap();
        assert!(src.is_dir());
        graph.verify().unwrap();
        graph
    }

    fn run(&self, goal: &str) -> Outcome {
        let graph = self.graph();
        let mut progress = Vec::new();
        let outcome = Runner::new(&graph, &self.executor, 4).run(goal, &mut |p| progress.push(p)).unwrap();
        assert!(
            progress
                .iter()
                .all(|p| matches!(p, Progress::Started { .. } | Progress::Log { .. } | Progress::Done { .. }))
        );
        outcome
    }

    fn sandbox_runs(&self) -> usize {
        self.runs.load(Ordering::SeqCst)
    }
}

fn ran(outcome: &Outcome) -> Vec<&str> {
    outcome.statuses.iter().filter(|(_, s)| matches!(s, Status::Ran { .. })).map(|(n, _)| n.as_str()).collect()
}

fn cached(outcome: &Outcome) -> Vec<&str> {
    outcome.statuses.iter().filter(|(_, s)| matches!(s, Status::Cached { .. })).map(|(n, _)| n.as_str()).collect()
}

#[test]
fn a_graph_runs_and_a_rerun_is_all_cache_hits() {
    let f = Fixture::new();
    f.write_source("lib.txt", "lib v1\n");
    f.write_source("docs.txt", "docs v1\n");

    let first = f.run("check");
    assert!(first.goal_holds());
    assert_eq!(ran(&first), ["app", "lib", "test"]);
    assert_eq!(first.statuses["docs"], Status::NotNeeded);
    let Status::Ran { result, .. } = &first.statuses["test"] else { panic!("test ran") };
    let report = f.dir.path().join("report");
    f.store.materialize_into(&result.outputs["out"], &report).unwrap();
    assert_eq!(fs::read_to_string(report.join("report")).unwrap(), "lib v1\napp\n");

    let runs_before = f.sandbox_runs();
    let second = f.run("check");
    assert!(second.goal_holds());
    assert_eq!(cached(&second), ["app", "lib", "test"]);
    assert_eq!(f.sandbox_runs(), runs_before);
}

#[test]
fn a_changed_input_reruns_only_what_depends_on_it() {
    let f = Fixture::new();
    f.write_source("lib.txt", "lib v1\n");
    f.write_source("docs.txt", "docs v1\n");
    f.run("all");

    f.write_source("docs.txt", "docs v2\n");
    let outcome = f.run("all");
    assert_eq!(ran(&outcome), ["docs", "lib"], "lib reads the same source tree, so it runs again too");
    assert_eq!(cached(&outcome), ["app", "test"], "lib made the same output, so app and test are cache hits");
}

#[test]
fn a_failure_skips_what_depends_on_it_and_why_explains_both() {
    let f = Fixture::new();
    f.write_source("docs.txt", "docs\n");
    let outcome = f.run("all");
    assert!(!outcome.goal_holds());
    assert!(matches!(&outcome.statuses["lib"], Status::Failed { reason, .. } if reason == "exit code 1"));
    assert_eq!(outcome.statuses["app"], Status::Skipped);
    assert_eq!(outcome.statuses["test"], Status::Skipped);
    assert!(matches!(outcome.statuses["docs"], Status::Ran { .. }));

    let why_test = outcome.explain("test").unwrap().to_string();
    assert!(why_test.contains("goal all requires test"), "{why_test}");
    assert!(why_test.contains("It was skipped: it depends on test -> app -> lib, which failed."), "{why_test}");
    let why_docs = outcome.explain("docs").unwrap().to_string();
    assert!(why_docs.contains("It ran, because the cache had no result for action"), "{why_docs}");
}

#[test]
fn a_plan_explains_a_cache_hit_and_a_run_without_running_anything() {
    let f = Fixture::new();
    f.write_source("lib.txt", "lib v1\n");
    f.write_source("docs.txt", "docs v1\n");
    f.run("check");
    f.write_source("docs.txt", "docs v2\n");

    let runs_before = f.sandbox_runs();
    let graph = f.graph();
    let plan = Runner::new(&graph, &f.executor, 1).plan("all").unwrap();
    assert_eq!(f.sandbox_runs(), runs_before);
    assert!(matches!(plan.statuses["docs"], Status::WillRun { .. }));
    assert!(matches!(plan.statuses["lib"], Status::WillRun { .. }));
    assert_eq!(plan.statuses["app"], Status::Waiting);

    let why_app = plan.explain("app").unwrap().to_string();
    assert!(why_app.contains("goal all requires test, test reads an output of app."), "{why_app}");
    assert!(why_app.contains("It must run after lib"), "{why_app}");
}

#[test]
fn two_actions_with_the_same_digest_share_one_execution() {
    let f = Fixture::new();
    let action = |name: &str| {
        format!(
            "action {name}\n  platform linux/amd64\n  root {}\n  env PATH \"{ROOTFS_PATH}\"\n  command \"sh\" \"-c\" \"echo same\"\n",
            f.root
        )
    };
    let document = format!("{}\n{}\ngoal both one two\n", action("one"), action("two"));
    let graph = text::parse(&document).unwrap().resolve(|_| Err::<Digest, ()>(())).unwrap();
    let outcome = Runner::new(&graph, &f.executor, 4).run("both", &mut |_| {}).unwrap();
    assert!(outcome.goal_holds());
    assert_eq!(f.sandbox_runs(), 1);
}

/// What the progress events say about each action: `started` when it came before `done`, and how many `done`s.
fn rows(progress: &[Progress]) -> Vec<(String, bool, usize)> {
    let mut rows: Vec<(String, bool, usize)> = Vec::new();
    for event in progress {
        let (name, started) = match event {
            Progress::Started { name, .. } => (name, true),
            Progress::Done { name, .. } => (name, false),
            Progress::Log { .. } => continue,
        };
        let index = match rows.iter().position(|(n, _, _)| n == name) {
            Some(index) => index,
            None => {
                rows.push((name.clone(), false, 0));
                rows.len() - 1
            }
        };
        match started {
            true => rows[index].1 = rows[index].2 == 0,
            false => rows[index].2 += 1,
        }
    }
    rows.sort();
    rows
}

#[test]
fn each_action_that_shares_an_execution_gets_its_own_events() {
    let f = Fixture::new();
    let action = |name: &str, text: &str| {
        format!(
            "action {name}\n  platform linux/amd64\n  root {}\n  env PATH \"{ROOTFS_PATH}\"\n  command \"sh\" \"-c\" \"echo {text}\"\n",
            f.root
        )
    };
    let run = |text: &str, parallelism: usize| {
        let document = format!("{}\n{}\ngoal both one two\n", action("one", text), action("two", text));
        let graph = text::parse(&document).unwrap().resolve(|_| Err::<Digest, ()>(())).unwrap();
        let mut progress = Vec::new();
        let outcome = Runner::new(&graph, &f.executor, parallelism).run("both", &mut |p| progress.push(p)).unwrap();
        (rows(&progress), ran(&outcome).len(), cached(&outcome).len())
    };
    let row = |name: &str, started: bool| (name.to_owned(), started, 1);

    let joined_while_running = run("same", 4);
    assert_eq!(joined_while_running, (vec![row("one", true), row("two", true)], 2, 0));
    let joined_a_cache_hit = run("same", 4);
    assert_eq!(joined_a_cache_hit, (vec![row("one", false), row("two", false)], 0, 2));
    let joined_after_it_ran = run("other", 1);
    assert_eq!(joined_after_it_ran, (vec![row("one", true), row("two", false)], 2, 0));
}

#[test]
fn a_recorded_run_explains_a_skip_after_the_fact() {
    use azad::ir::Canonical;
    let f = Fixture::new();
    f.write_source("docs.txt", "docs\n");
    let outcome = f.run("all");
    let restored = Outcome::decode(&outcome.encode()).unwrap();
    assert_eq!(restored.statuses, outcome.statuses);
    let why = restored.explain("test").unwrap().to_string();
    assert!(why.contains("It was skipped: it depends on test -> app -> lib, which failed."), "{why}");
}

#[test]
fn the_output_of_a_running_action_arrives_as_events() {
    let f = Fixture::new();
    let document = format!(
        "action talk\n  platform linux/amd64\n  root {}\n  env PATH \"{ROOTFS_PATH}\"\n  command \"sh\" \"-c\" \"echo one; echo two >&2; echo three\"\n\ngoal speak talk\n",
        f.root
    );
    let graph = text::parse(&document).unwrap().resolve(|_| Err::<Digest, ()>(())).unwrap();
    let mut lines = Vec::new();
    Runner::new(&graph, &f.executor, 1)
        .run("speak", &mut |p| {
            if let Progress::Log { name, stream, line } = p {
                lines.push((name, stream, line));
            }
        })
        .unwrap();
    use azad::log::Stream;
    assert!(lines.contains(&("talk".to_owned(), Stream::Stdout, "one".to_owned())));
    assert!(lines.contains(&("talk".to_owned(), Stream::Stderr, "two".to_owned())));
    assert!(lines.contains(&("talk".to_owned(), Stream::Stdout, "three".to_owned())));
}
