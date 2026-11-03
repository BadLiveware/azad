//! Tasks (decision 0015): the same definition runs on the host for a local run, and in its image for an isolated run.
#![allow(clippy::unwrap_used)]

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::Duration;

use azad::engine::{Engine, EngineConfig, Event, Progress, Status, host_platform};
use azad::frontend::gha::annotation::Annotation;
use azad::ir::{Digest, Image};
use azad::oci::Puller;
use azad::sandbox::Limits;
use azad::store::{Selection, Store};
use support::{ROOTFS_PATH, TEST_PROGRAMS, host_rootfs};
use tempfile::TempDir;

const IMAGE: &str = "tools@sha256:00000000000000000000000000000000000000000000000000000000000000bb";

const TASKS: &str = r#"
IMAGE = image("tools@sha256:00000000000000000000000000000000000000000000000000000000000000bb")

build = task(
    name = "build",
    cmd = "mkdir -p out && cat src/a.txt src/b.txt > out/all.txt",
    sources = ["src/**"],
    generates = ["out"],
    image = IMAGE,
)

count = task(name = "count", cmd = ["sh", "-c", "wc -l < out/all.txt"], deps = [build], image = IMAGE)

local = task(name = "local", cmd = "true")

report = task(
    name = "report",
    cmd = 'echo "::error file=$(pwd)/src/a.txt,line=1::bad"; echo "::warning file=src/b.txt::careful"',
    sources = ["src/**"],
    image = IMAGE,
)

goal("check", require = [count])
"#;

/// A workspace with the tasks in `app/`, and a store that knows `IMAGE` as if it were pulled.
fn workspace(dir: &TempDir) -> std::path::PathBuf {
    let rootfs = dir.path().join("rootfs");
    host_rootfs(&rootfs, TEST_PROGRAMS);
    let store = Store::open(dir.path().join("store")).unwrap();
    let tree = store.ingest(&rootfs, &Selection::All).unwrap();
    let env = BTreeMap::from([("PATH".to_owned(), ROOTFS_PATH.to_owned())]);
    Puller::new(store).unwrap().remember(IMAGE, &host_platform(), &Image { tree, env }).unwrap();
    let workspace = dir.path().join("workspace");
    let app = workspace.join("app");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(app.join("tasks.star"), TASKS).unwrap();
    fs::write(app.join("src/a.txt"), "a\n").unwrap();
    fs::write(app.join("src/b.txt"), "b\n").unwrap();
    fs::write(app.join("README"), "not a source\n").unwrap();
    workspace
}

fn engine(dir: &TempDir, isolated: bool) -> Engine {
    engine_at(dir, isolated, azad::Trust::Main)
}

fn engine_at(dir: &TempDir, isolated: bool, trust: azad::Trust) -> Engine {
    Engine::new(EngineConfig {
        store: dir.path().join("store"),
        root: dir.path().join("workspace"),
        platform: host_platform(),
        limits: Limits { timeout: Duration::from_secs(30), ..Limits::default() },
        tools: Default::default(),
        isolated,
        color: false,
        force: false,
        sandbox: azad::SandboxKind::Bubblewrap,
        executor: None,
        trust,
        history: None,
        withheld: Default::default(),
        filter: Default::default(),
        local: None,
    })
    .unwrap()
}

/// What each task did in a run of `//app:check`: "ran" or "cached".
fn run(engine: &Engine) -> BTreeMap<String, &'static str> {
    let goal = engine.load("//app:check", &mut |_| {}).unwrap();
    let report = engine.run(&goal, 2, &mut |_| {}).unwrap();
    assert!(report.outcome.goal_holds(), "{:?}", report.outcome.statuses);
    report
        .outcome
        .statuses
        .iter()
        .filter(|(_, status)| !matches!(status, Status::NotNeeded))
        .map(|(label, status)| {
            let what = match status {
                Status::Ran { .. } => "ran",
                Status::Cached { .. } => "cached",
                other => panic!("{label}: {other:?}"),
            };
            (label.trim_start_matches("//app:").to_owned(), what)
        })
        .collect()
}

fn statuses(pairs: &[(&str, &'static str)]) -> BTreeMap<String, &'static str> {
    pairs.iter().map(|(label, what)| ((*label).to_owned(), *what)).collect()
}

fn output_of(engine: &Engine, action: &str) -> Digest {
    let goal = engine.load("//app:check", &mut |_| {}).unwrap();
    let (_, result) = engine.last_result(&goal, action).unwrap().unwrap();
    result.outputs["app/out"]
}

#[test]
fn a_local_run_works_in_the_workspace_and_runs_a_task_again_only_when_its_files_change() {
    let dir = TempDir::new().unwrap();
    let workspace = workspace(&dir);
    let engine = engine(&dir, false);
    assert_eq!(run(&engine), statuses(&[("build", "ran"), ("count", "ran")]));
    assert_eq!(fs::read_to_string(workspace.join("app/out/all.txt")).unwrap(), "a\nb\n");
    assert_eq!(run(&engine), statuses(&[("build", "cached"), ("count", "cached")]));

    fs::write(workspace.join("app/README"), "changed, but not a source\n").unwrap();
    assert_eq!(run(&engine), statuses(&[("build", "cached"), ("count", "cached")]));

    fs::remove_dir_all(workspace.join("app/out")).unwrap();
    assert_eq!(run(&engine), statuses(&[("build", "ran"), ("count", "cached")]));
    assert!(workspace.join("app/out/all.txt").is_file());

    fs::write(workspace.join("app/src/a.txt"), "A\n").unwrap();
    assert_eq!(run(&engine), statuses(&[("build", "ran"), ("count", "ran")]));
}

#[test]
fn an_isolated_run_leaves_the_workspace_alone_and_makes_the_same_outputs() {
    let dir = TempDir::new().unwrap();
    let workspace = workspace(&dir);
    let isolated = engine(&dir, true);
    assert_eq!(run(&isolated), statuses(&[("build", "ran"), ("count", "ran")]));
    assert!(!workspace.join("app/out").exists());
    assert_eq!(run(&isolated), statuses(&[("build", "cached"), ("count", "cached")]));

    let local = engine(&dir, false);
    run(&local);
    assert_eq!(output_of(&isolated, "build"), output_of(&local, "build"));
}

#[test]
fn a_task_without_an_image_runs_on_the_host_also_in_an_isolated_run() {
    let dir = TempDir::new().unwrap();
    workspace(&dir);
    for isolated in [false, true] {
        let engine = engine(&dir, isolated);
        let goal = engine.load("//app:local", &mut |_| {}).unwrap();
        let action = &goal.graph.actions["//app:local"];
        assert!(matches!(action.kind, azad::ir::Kind::Host { .. }), "isolated: {isolated}, {:?}", action.kind);
        assert!(engine.run(&goal, 1, &mut |_| {}).unwrap().outcome.goal_holds());
    }
}

#[test]
fn a_host_task_that_fails_names_the_command_it_cannot_find() {
    let dir = TempDir::new().unwrap();
    let workspace = workspace(&dir);
    let tasks =
        "missing = task(name = \"missing\", cmd = [\"no-such-tool-for-azad\"])\ngoal(\"check\", require = [missing])\n";
    fs::write(Path::new(&workspace).join("app/tasks.star"), tasks).unwrap();
    let engine = engine(&dir, false);
    let goal = engine.load("//app:check", &mut |_| {}).unwrap();
    let report = engine.run(&goal, 1, &mut |_| {}).unwrap();
    let Status::Failed { reason, .. } = &report.outcome.statuses["//app:missing"] else { panic!("the task ran") };
    assert!(reason.contains("the command no-such-tool-for-azad is not found"), "{reason}");
}

const CALC: &str = r#"
calc = task(
    name = "calc",
    cmd = "mkdir -p out && cat src/*.py > out/all.py",
    sources = ["src/**"],
    generates = ["out"],
    env = {"LOG": "LEVEL"},
)
goal("check", require = [calc])
"#;

/// Why `//app:calc` ran in the last run of `//app:check`, or must run when `next` is set.
fn why_calc(engine: &Engine, next: bool) -> String {
    let goal = engine.load("//app:check", &mut |_| {}).unwrap();
    engine.explain(&goal, "calc", next).unwrap().explanation.to_string()
}

fn run_check(engine: &Engine) {
    let goal = engine.load("//app:check", &mut |_| {}).unwrap();
    assert!(engine.run(&goal, 1, &mut |_| {}).unwrap().outcome.goal_holds());
}

#[test]
fn why_names_the_files_and_the_values_that_changed_since_the_last_run() {
    let dir = TempDir::new().unwrap();
    let workspace = workspace(&dir);
    let app = workspace.join("app");
    fs::remove_dir_all(app.join("src")).unwrap();
    fs::write(app.join("tasks.star"), CALC.replace("LEVEL", "info")).unwrap();
    fs::create_dir_all(app.join("src/old")).unwrap();
    fs::write(app.join("src/calc.py"), "1 + 1\n").unwrap();
    fs::write(app.join("src/old/gone.py"), "\n").unwrap();
    let engine = engine(&dir, false);
    run_check(&engine);
    let first = why_calc(&engine, false);
    assert!(first.contains("It ran, because the cache had no result for action"), "{first}");
    assert!(first.ends_with("; no earlier run of //app:check has this action.\nIt exited 0."), "{first}");

    fs::write(app.join("src/calc.py"), "1 + 2\n").unwrap();
    fs::write(app.join("src/new.py"), "\n").unwrap();
    fs::remove_dir_all(app.join("src/old")).unwrap();
    fs::write(app.join("tasks.star"), CALC.replace("LEVEL", "debug")).unwrap();
    let changes = "because it changed since run 1:\n  app/src/calc.py changed\n  app/src/new.py was added\n  app/src/old/ was removed\n  env LOG: \"info\" -> \"debug\"";
    let next = why_calc(&engine, true);
    assert!(next.ends_with(&format!("It must run, {changes}")), "{next}");

    run_check(&engine);
    let second = why_calc(&engine, false);
    assert!(second.ends_with(&format!("It ran, {changes}\nIt exited 0.")), "{second}");

    fs::remove_dir_all(app.join("out")).unwrap();
    run_check(&engine);
    let third = why_calc(&engine, false);
    let removed = "It ran, because its outputs in the workspace (app/out) changed or were removed after run 2; its inputs are the same.\nIt exited 0.";
    assert!(third.ends_with(removed), "{third}");
}

#[test]
fn why_says_that_a_task_that_failed_with_the_same_inputs_runs_again() {
    let dir = TempDir::new().unwrap();
    let workspace = workspace(&dir);
    let tasks =
        "calc = task(name = \"calc\", cmd = \"exit 3\", sources = [\"src/**\"])\ngoal(\"check\", require = [calc])\n";
    fs::write(workspace.join("app/tasks.star"), tasks).unwrap();
    let engine = engine(&dir, false);
    let goal = engine.load("//app:check", &mut |_| {}).unwrap();
    assert!(!engine.run(&goal, 1, &mut |_| {}).unwrap().outcome.goal_holds());
    let next = why_calc(&engine, true);
    assert!(
        next.ends_with("It must run, because it failed in run 1 with the same inputs, and the cache keeps no failure."),
        "{next}"
    );
}

#[test]
fn the_annotations_of_a_task_name_files_in_the_workspace_on_the_host_and_in_the_image() {
    let dir = TempDir::new().unwrap();
    workspace(&dir);
    for isolated in [false, true] {
        let engine = engine(&dir, isolated);
        let mut views = BTreeMap::new();
        let goal = engine
            .load("//app:report", &mut |event| {
                if let Event::Loaded { workspace_views, .. } = event {
                    views = workspace_views;
                }
            })
            .unwrap();
        let mut lines = Vec::new();
        let report = engine
            .run(&goal, 1, &mut |event| {
                if let Event::Progress(Progress::Log { line, .. }) = event {
                    lines.push(line);
                }
            })
            .unwrap();
        assert!(report.outcome.goal_holds(), "{:?}", report.outcome.statuses);
        let annotations: Vec<String> = lines
            .iter()
            .filter_map(|line| Annotation::of_line(line, views.get("//app:report")))
            .map(|annotation| annotation.command())
            .collect();
        assert_eq!(
            annotations,
            ["::error file=app/src/a.txt,line=1::bad", "::warning file=app/src/b.txt::careful"],
            "isolated: {isolated}, output: {lines:?}"
        );
    }
}

/// A task that counts its runs in its cache `state`, a directory of its package. It runs again when its sources
/// change.
const COUNTING: &str = r#"
count = task(
    name = "count",
    cmd = "mkdir -p state && n=$(cat state/count 2>/dev/null || echo 0) && n=$((n+1)) && echo $n > state/count && echo count=$n",
    sources = ["src/**"],
    caches = {"state": "counter"},
    image = image("tools@sha256:00000000000000000000000000000000000000000000000000000000000000bb"),
)
"#;

/// `COUNTING` without an image, so that the task runs on the host also in an isolated run.
const COUNTING_ON_THE_HOST: &str = r#"
count = task(
    name = "count",
    cmd = "mkdir -p state && n=$(cat state/count 2>/dev/null || echo 0) && n=$((n+1)) && echo $n > state/count && echo count=$n",
    sources = ["src/**"],
    caches = {"state": "counter"},
)
"#;

fn count(engine: &Engine) -> String {
    let goal = engine.load("//app:count", &mut |_| {}).unwrap();
    let report = engine.run(&goal, 1, &mut |_| {}).unwrap();
    assert!(report.outcome.goal_holds(), "{:?}", report.outcome.statuses);
    let (_, result) = engine.last_result(&goal, "count").unwrap().unwrap();
    String::from_utf8(engine.read_blob(&result.stdout).unwrap()).unwrap()
}

#[test]
fn a_relative_cache_of_an_isolated_task_is_a_volume_at_that_directory() {
    let dir = TempDir::new().unwrap();
    let workspace = workspace(&dir);
    fs::write(workspace.join("app/tasks.star"), COUNTING).unwrap();
    let engine = engine(&dir, true);
    assert_eq!(count(&engine), "count=1\n");
    fs::write(workspace.join("app/src/a.txt"), "changed\n").unwrap();
    assert_eq!(count(&engine), "count=2\n");
    assert!(!workspace.join("app/state").exists(), "an isolated run leaves the workspace alone");
}

#[test]
fn a_new_machine_starts_a_host_task_with_a_copy_of_its_cache_directory() {
    use azad::remote::{DirectoryProvider, RemoteConfig, SigningKey};

    let remote = TempDir::new().unwrap();
    let machine = |dir: &TempDir| {
        let workspace = workspace(dir);
        fs::write(workspace.join("app/tasks.star"), COUNTING_ON_THE_HOST).unwrap();
        let provider = Box::new(DirectoryProvider::new(remote.path().to_path_buf()));
        let key = SigningKey::generate("ci").unwrap();
        let cache = RemoteConfig { provider, read: true, write: true, signing_key: Some(key), trusted: Vec::new() };
        let config = EngineConfig {
            store: dir.path().join("store"),
            root: workspace.clone(),
            platform: host_platform(),
            limits: Limits { timeout: Duration::from_secs(30), ..Limits::default() },
            tools: Default::default(),
            isolated: true,
            color: false,
            force: false,
            sandbox: azad::SandboxKind::Bubblewrap,
            executor: None,
            trust: azad::Trust::Main,
            history: None,
            withheld: Default::default(),
            filter: Default::default(),
            local: None,
        };
        (Engine::with_remote(config, cache).unwrap(), workspace)
    };
    let (first_dir, second_dir) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (first, _) = machine(&first_dir);
    assert_eq!(count(&first), "count=1\n");
    let (second, workspace) = machine(&second_dir);
    assert_eq!(count(&second), "count=2\n", "the second machine starts from the first one's state/");
    assert_eq!(fs::read_to_string(workspace.join("app/state/count")).unwrap(), "2\n");
}

/// A task that reads a directory of the workspace outside its package, and what a task of another package generates
/// (decision 0026): a local run builds the other package's task first, runs the task again when either input
/// changes, and an isolated run has both at their workspace paths.
#[test]
fn a_task_reads_other_directories_and_the_outputs_of_another_package() {
    let dir = TempDir::new().unwrap();
    let workspace = workspace(&dir);
    fs::create_dir_all(workspace.join("lib")).unwrap();
    fs::create_dir_all(workspace.join("shared")).unwrap();
    fs::write(workspace.join("shared/notes.txt"), "shared\n").unwrap();
    fs::write(workspace.join("lib/data.txt"), "lib\n").unwrap();
    fs::write(
        workspace.join("lib/tasks.star"),
        format!(
            r#"task(name = "build", cmd = "mkdir -p out && cp data.txt out/data.txt", sources = ["data.txt"], generates = ["out"], image = image("{IMAGE}"))"#
        ),
    )
    .unwrap();
    fs::write(
        workspace.join("app/tasks.star"),
        format!(
            r#"
use = task(
    name = "use",
    cmd = "mkdir -p out && cat ../lib/out/data.txt ../shared/notes.txt > out/seen.txt",
    sources = ["//shared/**"],
    deps = ["//lib:build"],
    generates = ["out"],
    image = image("{IMAGE}"),
)
goal("check", require = [use])
"#
        ),
    )
    .unwrap();

    let local = engine(&dir, false);
    assert_eq!(run(&local), statuses(&[("//lib:build", "ran"), ("use", "ran")]));
    assert_eq!(fs::read_to_string(workspace.join("app/out/seen.txt")).unwrap(), "lib\nshared\n");
    assert_eq!(run(&local), statuses(&[("//lib:build", "cached"), ("use", "cached")]));
    fs::write(workspace.join("shared/notes.txt"), "shared, changed\n").unwrap();
    assert_eq!(run(&local), statuses(&[("//lib:build", "cached"), ("use", "ran")]));
    fs::write(workspace.join("lib/data.txt"), "lib, changed\n").unwrap();
    assert_eq!(run(&local), statuses(&[("//lib:build", "ran"), ("use", "ran")]));

    let isolated = engine(&dir, true);
    assert_eq!(run(&isolated), statuses(&[("//lib:build", "ran"), ("use", "ran")]));
}

/// Makes the workspace a git repository, as a dependency's files are those that git tracks or would track.
fn init_git(workspace: &Path) {
    let status = std::process::Command::new("git").args(["init", "-q"]).current_dir(workspace).status().unwrap();
    assert!(status.success());
    fs::write(workspace.join(".gitignore"), "out/\n").unwrap();
}

/// `app` names only `mid`; `mid` names `base` (decision 0028). A task with `dependencies = True` reads all three, in a
/// local and in an isolated run, and runs again when a file of `base` changes; a file that `mid` does not export does
/// not run it again.
#[test]
fn a_task_reads_the_dependencies_of_its_dependencies() {
    let dir = TempDir::new().unwrap();
    let workspace = workspace(&dir);
    for (package, file) in [("base", "base.txt"), ("mid", "mid.txt")] {
        fs::create_dir_all(workspace.join(package)).unwrap();
        fs::write(workspace.join(package).join(file), format!("{package}\n")).unwrap();
    }
    fs::write(workspace.join("mid/README"), "not exported\n").unwrap();
    init_git(&workspace);
    fs::write(workspace.join("base/tasks.star"), "package()\n").unwrap();
    fs::write(workspace.join("mid/tasks.star"), "package(deps = [\"//base\"], exports = [\"*.txt\"])\n").unwrap();
    fs::write(
        workspace.join("app/tasks.star"),
        format!(
            r#"
package(deps = ["//mid"])
use = task(
    name = "use",
    cmd = "mkdir -p out && cat ../mid/mid.txt ../base/base.txt > out/seen.txt",
    generates = ["out"],
    dependencies = True,
    image = image("{IMAGE}"),
)
goal("check", require = [use])
"#
        ),
    )
    .unwrap();

    let local = engine(&dir, false);
    assert_eq!(run(&local), statuses(&[("use", "ran")]));
    assert_eq!(fs::read_to_string(workspace.join("app/out/seen.txt")).unwrap(), "mid\nbase\n");
    fs::write(workspace.join("mid/README"), "changed, but not exported\n").unwrap();
    assert_eq!(run(&local), statuses(&[("use", "cached")]));
    fs::write(workspace.join("base/base.txt"), "base, changed\n").unwrap();
    assert_eq!(run(&local), statuses(&[("use", "ran")]));

    let isolated = engine(&dir, true);
    assert_eq!(run(&isolated), statuses(&[("use", "ran")]));
}

/// A package provides the build of a library, and a task that depends on the build's output alone stays cached when
/// a change in the library does not change the output: early cutoff (decision 0028).
#[test]
fn a_provided_output_runs_first_and_an_unchanged_output_keeps_its_dependents_cached() {
    let dir = TempDir::new().unwrap();
    let workspace = workspace(&dir);
    fs::create_dir_all(workspace.join("lib")).unwrap();
    fs::write(workspace.join("lib/data.txt"), "first\nsecond\n").unwrap();
    init_git(&workspace);
    fs::write(
        workspace.join("lib/tasks.star"),
        format!(
            r#"
package(provides = ["build"])
task(name = "build", cmd = "mkdir -p out && head -1 data.txt > out/first.txt", sources = ["data.txt"], generates = ["out"], image = image("{IMAGE}"))
"#
        ),
    )
    .unwrap();
    let consumer = |dependencies: &str| {
        format!(
            r#"
package(deps = ["//lib"])
use = task(name = "use", cmd = "mkdir -p out && cat ../lib/out/first.txt > out/seen.txt", generates = ["out"], dependencies = {dependencies}, image = image("{IMAGE}"))
goal("check", require = [use])
"#
        )
    };

    fs::write(workspace.join("app/tasks.star"), consumer("True")).unwrap();
    let local = engine(&dir, false);
    assert_eq!(run(&local), statuses(&[("//lib:build", "ran"), ("use", "ran")]));
    assert_eq!(fs::read_to_string(workspace.join("app/out/seen.txt")).unwrap(), "first\n");

    fs::write(workspace.join("app/tasks.star"), consumer(r#"["//lib:build"]"#)).unwrap();
    assert_eq!(run(&local), statuses(&[("//lib:build", "cached"), ("use", "ran")]));
    fs::write(workspace.join("lib/data.txt"), "first\nsecond, changed\n").unwrap();
    assert_eq!(run(&local), statuses(&[("//lib:build", "ran"), ("use", "cached")]));
}

#[test]
fn dependencies_that_form_a_cycle_are_an_error_that_names_it() {
    let dir = TempDir::new().unwrap();
    let workspace = workspace(&dir);
    for (package, deps) in [("a", "//b"), ("b", "//a")] {
        fs::create_dir_all(workspace.join(package)).unwrap();
        fs::write(workspace.join(package).join("tasks.star"), format!("package(deps = [\"{deps}\"])\n")).unwrap();
    }
    fs::write(
        workspace.join("app/tasks.star"),
        format!("package(deps = [\"//a\"])\nt = task(name = \"t\", cmd = \"true\", dependencies = True, image = image(\"{IMAGE}\"))\ngoal(\"check\", require = [t])\n"),
    )
    .unwrap();
    let error = engine(&dir, false).load("//app:check", &mut |_| {}).unwrap_err().to_string();
    assert!(error.contains("cycle: //a -> //b -> //a"), "{error}");
}

/// A run of code that is not merged builds no image from a Dockerfile, because a build runs the Dockerfile's steps
/// with the host's container runtime (decision 0024, amendment); it says so before it looks for a runtime.
#[test]
fn a_pull_request_does_not_build_an_image_from_a_dockerfile() {
    let dir = TempDir::new().unwrap();
    let workspace = workspace(&dir);
    fs::write(workspace.join("app/Dockerfile"), "FROM scratch\n").unwrap();
    fs::write(
        workspace.join("app/tasks.star"),
        "t = task(name = \"t\", cmd = \"true\", image = image(dockerfile = \"//app/Dockerfile\"))\ngoal(\"check\", require = [t])\n",
    )
    .unwrap();
    let engine = engine_at(&dir, true, azad::Trust::PullRequest);
    let error = engine.load("//app:check", &mut |_| {}).unwrap_err().to_string();
    assert!(error.contains("a run of a pull request does not build images from Dockerfiles"), "{error}");
}

/// What an isolated action leaves undeclared is reported, except what the workspace's git ignores, such as the `bin/`
/// of a build: that is the expected scratch of the project's tools.
#[test]
fn undeclared_paths_that_git_ignores_are_not_reported() {
    let dir = TempDir::new().unwrap();
    let workspace = workspace(&dir);
    init_git(&workspace);
    fs::write(workspace.join(".gitignore"), "bin/\n*.log\n").unwrap();
    let tool = workspace.join("tool");
    fs::create_dir_all(&tool).unwrap();
    let tasks = format!(
        "task(name = \"scratch\", cmd = \"mkdir -p bin obj && touch bin/app.dll build.log stray.txt\", image = image({IMAGE:?}))\n"
    );
    fs::write(tool.join("tasks.star"), tasks).unwrap();
    let engine = engine(&dir, true);
    let goal = engine.load("//tool:scratch", &mut |_| {}).unwrap();
    let mut discarded = Vec::new();
    let mut notes = Vec::new();
    engine
        .run(&goal, 1, &mut |event| match event {
            Event::Progress(Progress::Done { status: Status::Ran { discarded: paths, .. }, .. }) => {
                discarded.extend(paths);
            }
            Event::Note(note) => notes.push(note.text),
            _ => {}
        })
        .unwrap();
    assert_eq!(discarded, ["tool/obj/", "tool/stray.txt"]);
    let counted = "//tool:scratch: discarded 2 paths that git ignores, such as tool/bin/";
    assert!(notes.iter().any(|note| note.starts_with(counted)), "a diagnostic note counts them: {notes:?}");
}
