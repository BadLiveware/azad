//! The `azad` command as a user runs it: its messages and exit codes.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

const USAGE_EXIT_CODE: i32 = 2;
/// The most changed paths that `azad why` lists.
const SHOWN_PATHS: usize = 10;
const TASKS: &str = r#"
root = image("busybox@sha256:0000000000000000000000000000000000000000000000000000000000000000")
a = action(name = "a", cmd = ["true"], root = root)
goal("check", require = [a], doc = "Everything")
goal("lint", require = [a])
hello = task(name = "hello", cmd = "mkdir -p out && echo hello > out/greeting", generates = ["out"], doc = "Say hello")
color = task(name = "color", cmd = "echo color=$CARGO_TERM_COLOR")
"#;
const WORKFLOW: &str = "on: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - run: true\n";

fn azad(workspace: &Path, args: &[&str]) -> Output {
    azad_in(workspace, workspace, args)
}

/// `azad` with the current directory `dir` in the workspace, and no personal defaults.
fn azad_in(workspace: &Path, dir: &Path, args: &[&str]) -> Output {
    azad_with(workspace, dir, args, "")
}

/// `azad` with the personal defaults in the file `config`, or none when it is empty.
fn azad_with(workspace: &Path, dir: &Path, args: &[&str], config: &str) -> Output {
    let store = workspace.join(".store");
    Command::new(env!("CARGO_BIN_EXE_azad"))
        .args(["--store", &store.display().to_string(), "--root", &workspace.display().to_string()])
        .args(args)
        .current_dir(dir)
        .env("AZAD_CONFIG", config)
        .output()
        .expect("azad starts")
}

/// Whether a listing of goals has a line for `target`.
fn lists(listing: &str, target: &str) -> bool {
    listing.lines().any(|line| line.split_whitespace().next() == Some(target))
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn write(root: &Path, path: &str, content: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().expect("a file has a directory")).expect("the directory is made");
    fs::write(&path, content).expect("the file is written");
}

/// An empty git repository that ignores the store.
fn repository() -> TempDir {
    let dir = TempDir::new().expect("a temporary directory");
    let git = Command::new("git").args(["init", "-q"]).current_dir(dir.path()).status().expect("git starts");
    assert!(git.success());
    write(dir.path(), ".gitignore", ".store/\n");
    dir
}

/// A git repository with goals in the root package, a package that cannot be evaluated, and a workflow.
fn workspace() -> TempDir {
    let dir = repository();
    let root = dir.path();
    write(root, "tasks.star", TASKS);
    write(root, "broken/tasks.star", "x = undefined_name\n");
    write(root, ".github/workflows/ci.yml", WORKFLOW);
    dir
}

#[test]
fn an_error_is_printed_once() {
    let dir = workspace();
    let output = azad(dir.path(), &["pull", "python@sha256:abc"]);
    let message = stderr(&output);
    assert!(!output.status.success());
    assert_eq!(message.matches("lower-case hex digits").count(), 1, "{message}");
}

#[test]
fn run_without_a_goal_lists_the_goals_of_the_workspace() {
    let dir = workspace();
    let output = azad(dir.path(), &["run"]);
    let message = stderr(&output);
    assert_eq!(output.status.code(), Some(USAGE_EXIT_CODE), "{message}");
    for goal in ["//:check", "//:lint", ".github/workflows/ci.yml", ".github/workflows/ci.yml:test"] {
        assert!(lists(&message, goal), "{goal} is not listed:\n{message}");
    }
    let broken = message.lines().find(|line| line.contains("broken/tasks.star")).expect("the broken package is listed");
    assert!(broken.contains("undefined_name"), "{broken}");
}

#[test]
fn a_target_that_no_frontend_reads_lists_the_goals() {
    let dir = workspace();
    let output = azad(dir.path(), &["run", "app/chek"]);
    let message = stderr(&output);
    assert!(!output.status.success());
    assert!(message.contains("no frontend reads \"app/chek\""), "{message}");
    assert!(lists(&message, "//:check"), "{message}");
}

/// An action whose root is a file of its package, so that loading its goal needs no image.
fn local_goal(action: &str) -> String {
    format!(
        "{action} = action(name = \"{action}\", cmd = [\"true\"], root = glob([\"tasks.star\"]))\ngoal(\"check\", require = [{action}])\n"
    )
}

/// `azad ir` in a directory of the workspace, and its standard output.
fn ir_in(workspace: &Path, dir: &str, target: &str) -> String {
    let output = azad_in(workspace, &workspace.join(dir), &["ir", target]);
    assert!(output.status.success(), "azad ir {target} in {dir:?}: {}", stderr(&output));
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn a_short_name_is_the_goal_of_the_package_of_the_directory() {
    let dir = repository();
    let root = dir.path();
    write(root, "tasks.star", &local_goal("top"));
    write(root, "app/tasks.star", &local_goal("inner"));
    write(root, "app/src/main.py", "");
    for (cwd, target, action) in [
        ("", "check", "//:top"),
        ("", ":check", "//:top"),
        ("", "app:check", "//app:inner"),
        ("app", "check", "//app:inner"),
        ("app", ":check", "//app:inner"),
        ("app", ".:check", "//app:inner"),
        ("app", "..:check", "//:top"),
        ("app/src", "check", "//app:inner"),
        ("app/src", "..:check", "//app:inner"),
    ] {
        let graph = ir_in(root, cwd, target);
        assert!(graph.contains(&format!("action {action}\n")), "{target} in {cwd:?}:\n{graph}");
    }
}

#[test]
fn a_short_name_that_is_not_a_goal_names_the_goals() {
    let dir = repository();
    let root = dir.path();
    write(root, "app/tasks.star", &local_goal("inner"));
    write(root, "docs/index.md", "");

    let unknown = azad_in(root, &root.join("app"), &["ir", "chek"]);
    let message = stderr(&unknown);
    assert!(!unknown.status.success());
    assert!(message.contains("there is no goal //app:chek; known goals: //app:check"), "{message}");

    let no_package = azad_in(root, &root.join("docs"), &["run", "check"]);
    let message = stderr(&no_package);
    assert!(!no_package.status.success());
    assert!(message.contains("the goal \"check\" has no package"), "{message}");
    assert!(lists(&message, "//app:check"), "{message}");
}

#[test]
fn a_glob_that_matches_no_file_is_a_warning() {
    let dir = repository();
    let root = dir.path();
    write(root, "app/src/main.py", "");
    write(root, "lib/test.star", "def test(patterns):\n    return glob(patterns)\n");
    let tasks = [
        "load(\"//lib/test.star\", \"test\")",
        "lint = action(",
        "    name = \"lint\",",
        "    cmd = [\"true\"],",
        "    root = glob([\"tasks.star\"]),",
        "    inputs = {\"/src\": glob([\"scr/*.py\"]), \"/ok\": glob([\"src/*.py\"]), \"/tests\": test([\"tests/*\"])},",
        ")",
        "goal(\"check\", require = [lint])",
    ];
    write(root, "app/tasks.star", &tasks.join("\n"));
    let output = azad(root, &["ir", "//app:check"]);
    let message = stderr(&output);
    assert!(output.status.success(), "{message}");
    let warnings: Vec<&str> = message.lines().filter(|line| line.starts_with("warning:")).collect();
    assert_eq!(
        warnings,
        [
            "warning: glob([\"scr/*.py\"]) in //app matches no file (declared at //app/tasks.star:6)",
            "warning: glob([\"tests/*\"]) in //app matches no file (declared at //lib/test.star:2, called from //app/tasks.star:6)",
        ],
        "{message}"
    );
}

#[test]
fn check_reports_a_runner_that_cannot_run_in_an_image() {
    let dir = workspace();
    let bin = TempDir::new().expect("a temporary directory");
    let azad = bin.path().join("azad");
    fs::copy(env!("CARGO_BIN_EXE_azad"), &azad).expect("azad is copied");
    // `azad` itself links the host's C library dynamically, as the glibc build of the runner does.
    fs::copy(env!("CARGO_BIN_EXE_azad"), bin.path().join("azad-gha-runner")).expect("the runner is copied");
    let output = Command::new(&azad)
        .args(["--store", &dir.path().join(".store").display().to_string(), "check", ".github/workflows/ci.yml"])
        .env_remove("AZAD_GHA_RUNNER")
        .current_dir(dir.path())
        .output()
        .expect("azad starts");
    let report = String::from_utf8_lossy(&output.stdout);
    assert!(!output.status.success(), "{report}");
    assert!(report.contains("azad-gha-runner is not found") && report.contains("AZAD_GHA_RUNNER"), "{report}");
}

/// A text-form graph whose `bad` action fails, with a root file system made from the host's programs.
fn failing_graph(root: &Path) {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../dev/host-rootfs.sh");
    let built = Command::new(script).arg(root.join("rootfs")).args(["sh", "echo"]).status().expect("the script starts");
    assert!(built.success());
    let action = |name: &str, script: &str| {
        format!(
            "action {name}\n  platform {}\n  root local(\"rootfs\")\n  env PATH \"/usr/bin\"\n  command \"sh\" \"-c\" {script:?}\n\n",
            azad::host_platform()
        )
    };
    let graph = [action("good", "echo fine"), action("bad", "echo broken; exit 3"), "goal check good bad\n".to_owned()];
    write(root, "graph.azad", &graph.concat());
}

#[test]
fn log_and_why_without_an_action_take_the_one_that_failed() {
    let dir = workspace();
    failing_graph(dir.path());
    let run = azad(dir.path(), &["run", "--output", "plain", "graph.azad:check"]);
    assert!(!run.status.success(), "{}", stderr(&run));
    let log = azad(dir.path(), &["log", "graph.azad:check"]);
    assert!(String::from_utf8_lossy(&log.stdout).contains("broken"), "{}", stderr(&log));
    let why = azad(dir.path(), &["why", "graph.azad:check"]);
    assert!(String::from_utf8_lossy(&why.stdout).contains("bad"), "{}", stderr(&why));
}

#[test]
fn list_shows_what_goals_and_tasks_are_for_and_a_task_runs_by_its_name() {
    let dir = workspace();
    let listed = azad(dir.path(), &["list"]);
    let listing = String::from_utf8_lossy(&listed.stdout);
    let line_of = |target: &str| listing.lines().find(|line| line.split_whitespace().next() == Some(target));
    assert!(line_of("//:check").is_some_and(|line| line.ends_with("Everything")), "{listing}");
    assert!(line_of("//:hello").is_some_and(|line| line.ends_with("Say hello")), "{listing}");

    let run = azad(dir.path(), &["run", "--output", "plain", "//:hello"]);
    assert!(run.status.success(), "{}", stderr(&run));
    assert_eq!(fs::read_to_string(dir.path().join("out/greeting")).expect("the task wrote its file"), "hello\n");
}

#[test]
fn tools_are_asked_for_color_only_when_the_terminal_view_shows_it() {
    let logged = |output: &str| {
        let dir = workspace();
        let run = Command::new(env!("CARGO_BIN_EXE_azad"))
            .args(["--store", &dir.path().join(".store").display().to_string(), "run", "--output", output, "//:color"])
            .env_remove("NO_COLOR")
            .env_remove("CARGO_TERM_COLOR")
            .env_remove("FORCE_COLOR")
            .env_remove("CLICOLOR_FORCE")
            .current_dir(dir.path())
            .output()
            .expect("azad starts");
        assert!(run.status.success(), "{}", stderr(&run));
        String::from_utf8_lossy(&azad(dir.path(), &["log", "//:color"]).stdout).into_owned()
    };
    assert!(logged("terminal").contains("color=always"));
    assert!(logged("plain").contains("color=\n"));
}

#[test]
fn force_runs_a_task_whose_result_is_known() {
    let dir = workspace();
    let last_line = |args: &[&str]| stderr(&azad(dir.path(), args)).lines().last().unwrap_or_default().to_owned();
    assert!(last_line(&["run", "--output", "plain", "//:hello"]).contains("1 ran"));
    assert!(last_line(&["run", "--output", "plain", "//:hello"]).contains("0 ran, 1 cached"));
    assert!(last_line(&["run", "--output", "plain", "--force", "//:hello"]).contains("1 ran"));
}

#[test]
fn why_names_what_changed_since_the_last_run() {
    let dir = repository();
    let root = dir.path();
    let tasks = |level: &str| {
        format!(
            "calc = task(name = \"calc\", cmd = \"cat src/*.py > /dev/null\", sources = [\"src/**\"], env = {{\"LOG\": \"{level}\"}})\n"
        )
    };
    let sources: Vec<String> = (0..SHOWN_PATHS + 2).map(|n| format!("src/m{n:02}.py")).collect();
    write(root, "tasks.star", &tasks("info"));
    for source in &sources {
        write(root, source, "1\n");
    }
    let run = azad(root, &["run", "--output", "plain", "calc"]);
    assert!(run.status.success(), "{}", stderr(&run));

    write(root, "tasks.star", &tasks("debug"));
    for source in &sources {
        write(root, source, "2\n");
    }
    let run = azad(root, &["run", "--output", "plain", "calc"]);
    assert!(run.status.success(), "{}", stderr(&run));

    let why = azad(root, &["why", "calc"]);
    let shown: Vec<String> = sources[..SHOWN_PATHS].iter().map(|source| format!("  {source} changed")).collect();
    let expected = [
        "In run 2 of //:calc:".to_owned(),
        "//:calc is needed: goal //:calc requires //:calc.".to_owned(),
        "It ran, because it changed since run 1:".to_owned(),
        shown.join("\n"),
        "  and 2 more paths".to_owned(),
        "  env LOG: \"info\" -> \"debug\"".to_owned(),
        "It exited 0.".to_owned(),
    ];
    assert_eq!(stdout(&why), format!("{}\n", expected.join("\n")), "{}", stderr(&why));
}

#[test]
fn in_github_actions_the_annotations_of_a_task_name_files_in_the_repository_and_other_commands_stay_stopped() {
    let dir = repository();
    let root = dir.path();
    let cmd = r#"echo "::error file=$(pwd)/main.py,line=3::bad"; echo "::warning file=lib/util.py,line=1,col=2::careful"; echo "::set-env name=PATH::/evil"; exit 1"#;
    write(root, "svc/tasks.star", &format!("lint = task(name = \"lint\", cmd = '{cmd}')\n"));
    let run = azad(root, &["run", "--output", "github", "//svc:lint"]);
    let lines: Vec<String> = stderr(&run).lines().map(str::to_owned).collect();
    let stop = lines.iter().find_map(|line| line.strip_prefix("::stop-commands::")).expect("commands are stopped");
    let resume = format!("::{stop}::");
    let stopped = |line: &str| {
        let at = |text: &str| lines.iter().position(|l| l == text).unwrap_or_else(|| panic!("no {text}: {lines:#?}"));
        let start = at(&format!("::stop-commands::{stop}"));
        (start..at(&resume)).contains(&at(line))
    };
    assert!(stopped("::set-env name=PATH::/evil"), "{lines:#?}");
    assert!(!stopped("::error file=svc/main.py,line=3::bad"), "{lines:#?}");
    assert!(!stopped("::warning file=svc/lib/util.py,line=1,col=2::careful"), "{lines:#?}");
}

/// An executor service that `azad executor` runs, stopped when the test ends.
struct Executor {
    process: std::process::Child,
    socket: std::path::PathBuf,
}

impl Executor {
    fn start(workspace: &Path) -> Executor {
        let socket = workspace.join(".store-executor.sock");
        let process = Command::new(env!("CARGO_BIN_EXE_azad"))
            .args(["--store", &workspace.join(".store").display().to_string(), "executor", "--socket"])
            .arg(&socket)
            .current_dir(workspace)
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("the executor starts");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::os::unix::net::UnixStream::connect(&socket).is_err() {
            assert!(std::time::Instant::now() < deadline, "the executor listens");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        Executor { process, socket }
    }

    fn stop(&mut self) {
        self.process.kill().ok();
        self.process.wait().ok();
    }
}

impl Drop for Executor {
    fn drop(&mut self) {
        self.stop();
    }
}

#[test]
fn a_goal_runs_through_the_executor_process_and_its_results_are_cached_by_the_client() {
    let dir = workspace();
    let mut executor = Executor::start(dir.path());
    let socket = executor.socket.display().to_string();
    let run = |args: &[&str]| {
        azad(dir.path(), &[&["--executor", socket.as_str(), "run", "--output", "plain"], args].concat())
    };
    let first = run(&["//:hello"]);
    assert!(first.status.success(), "{}", stderr(&first));
    assert_eq!(fs::read_to_string(dir.path().join("out/greeting")).expect("the task wrote its file"), "hello\n");
    assert!(stderr(&run(&["//:hello"])).contains("0 ran, 1 cached"));

    executor.stop();
    let without = run(&["--force", "//:hello"]);
    assert!(!without.status.success());
    assert!(stderr(&without).contains("the executor at"), "{}", stderr(&without));
}

const WEBHOOK_SECRET: &str = "a shared secret";

/// `azad serve`, which prints where it takes webhooks, stopped when the test ends.
struct Server {
    process: std::process::Child,
    url: String,
}

impl Server {
    fn start(workspace: &Path) -> Server {
        let mut process = Command::new(env!("CARGO_BIN_EXE_azad"))
            .args(["--store", &workspace.join(".store").display().to_string(), "serve", "--listen", "127.0.0.1:0"])
            .env("AZAD_WEBHOOK_SECRET", WEBHOOK_SECRET)
            .current_dir(workspace)
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("azad serve starts");
        let mut line = String::new();
        let stderr = process.stderr.take().expect("the standard error is piped");
        std::io::BufRead::read_line(&mut std::io::BufReader::new(stderr), &mut line).expect("azad serve says where");
        let url = line.split_whitespace().last().expect("a URL").to_owned();
        Server { process, url }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.process.kill().ok();
        self.process.wait().ok();
    }
}

/// An event runs code that someone else wrote, so the orchestrator never runs a task with the host's tools: a task
/// without an image fails its check run, and does not run.
#[test]
fn a_simulated_pull_request_gets_a_check_run_and_no_task_runs_on_the_host() {
    let dir = repository();
    let marker = dir.path().join("ran-on-the-host");
    let tasks = format!(
        "host = task(name = \"host\", cmd = \"touch {}\")\ngoal(\"check\", require = [host], on = [\"pull_request\"])\n",
        marker.display()
    );
    write(dir.path(), "tasks.star", &tasks);
    let git = |args: &[&str]| {
        let status = Command::new("git").args(args).current_dir(dir.path()).status().expect("git starts");
        assert!(status.success(), "git {args:?}");
    };
    git(&["add", "-A"]);
    git(&["-c", "user.name=test", "-c", "user.email=test@example.com", "commit", "-q", "-m", "first"]);
    let server = Server::start(dir.path());

    let simulate = Command::new(env!("CARGO_BIN_EXE_azad"))
        .args([
            "--store",
            &dir.path().join(".store").display().to_string(),
            "--root",
            &dir.path().display().to_string(),
        ])
        .args(["simulate", "--server", &server.url, "pull-request", "1"])
        .env("AZAD_WEBHOOK_SECRET", WEBHOOK_SECRET)
        .current_dir(dir.path())
        .output()
        .expect("azad simulate starts");
    assert!(simulate.status.success(), "{}", stderr(&simulate));

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let checks = loop {
        let listed = stdout(&azad(dir.path(), &["checks"]));
        if listed.starts_with("failure") || listed.starts_with("success") {
            break listed;
        }
        assert!(std::time::Instant::now() < deadline, "the check run did not complete: {listed}");
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    assert!(checks.starts_with("failure"), "{checks}");
    assert!(checks.contains("//:check") && checks.contains("would run with the host's tools"), "{checks}");
    assert!(!marker.exists(), "the task did not run on the host");
}

#[test]
fn host_tasks_do_not_get_the_secrets_of_azad_itself() {
    let workspace = repository();
    let check = r#"test -z "$AZAD_SIGNING_KEY" && test -z "$AZAD_GITHUB_TOKEN" && test -z "$ACTIONS_RUNTIME_TOKEN" && test "$VISIBLE" = yes"#;
    write(workspace.path(), "tasks.star", &format!("env = task(name = \"env\", cmd = {check:?})\n"));
    let store = workspace.path().join(".store");
    let output = Command::new(env!("CARGO_BIN_EXE_azad"))
        .args(["--store", &store.display().to_string(), "--root", &workspace.path().display().to_string()])
        .args(["run", "env", "--output", "plain"])
        .current_dir(workspace.path())
        .env("AZAD_SIGNING_KEY", "a secret")
        .env("AZAD_GITHUB_TOKEN", "a secret")
        .env("ACTIONS_RUNTIME_TOKEN", "a secret")
        .env("VISIBLE", "yes")
        .output()
        .expect("azad starts");
    assert!(output.status.success(), "{}", stderr(&output));
}

/// A repository whose apps get the tag `app` from their `config.star`, with a task `fmt` in four packages.
fn apps_and_services() -> TempDir {
    let dir = repository();
    let root = dir.path();
    write(root, "apps/config.star", "tags = [\"app\"]\n");
    for (package, tag) in [
        ("apps/ios", "swift"),
        ("apps/android", "gradle"),
        ("services/billing", "dotnet"),
        ("services/orders", "dotnet"),
    ] {
        write(
            root,
            &format!("{package}/tasks.star"),
            &format!("fmt = task(name = \"fmt\", cmd = \"true\", tags = [{tag:?}], doc = \"Format\")\n"),
        );
    }
    dir
}

/// The labels of the tasks that a plain run printed as ran or cached.
fn ran(output: &Output) -> Vec<String> {
    let mut labels: Vec<String> = stderr(output)
        .lines()
        .filter_map(|line| line.split_once("] ").map(|(_, text)| text))
        .filter_map(|text| text.strip_prefix("ok      ").or_else(|| text.strip_prefix("cached  ")))
        .map(|rest| rest.split_whitespace().next().unwrap_or_default().to_owned())
        .collect();
    labels.sort();
    labels
}

#[test]
fn a_short_name_runs_the_goal_of_every_package_at_or_below_the_directory() {
    let workspace = apps_and_services();
    let root = workspace.path();
    let plain = ["run", "fmt", "--output", "plain"];
    let everything = ["//apps/android:fmt", "//apps/ios:fmt", "//services/billing:fmt", "//services/orders:fmt"];
    assert_eq!(ran(&azad_in(root, root, &plain)), everything);
    assert_eq!(ran(&azad_in(root, &root.join("apps"), &plain)), ["//apps/android:fmt", "//apps/ios:fmt"]);
    assert_eq!(ran(&azad_in(root, &root.join("apps/ios"), &plain)), ["//apps/ios:fmt"]);

    write(root, "services/broken/tasks.star", "x = undefined_name\n");
    let broken = azad_in(root, root, &plain);
    assert!(!broken.status.success());
    assert!(stderr(&broken).contains("//services/broken/tasks.star"), "{}", stderr(&broken));
}

#[test]
fn personal_defaults_exclude_tags_and_say_so_and_the_command_line_overrides_them() {
    let workspace = apps_and_services();
    let root = workspace.path();
    let config = root.join(".store-config.toml");
    fs::write(&config, "exclude = [\"app\"]\n").expect("the config is written");
    let config = config.display().to_string();
    let plain = ["run", "fmt", "--output", "plain"];

    let run = azad_with(root, root, &plain, &config);
    assert_eq!(ran(&run), ["//services/billing:fmt", "//services/orders:fmt"]);
    let note = format!("note: //...:fmt: skipped 2 tasks tagged app (the personal defaults in {config})");
    assert!(stderr(&run).contains(&note), "{}", stderr(&run));
    let minimal = azad_with(root, root, &["run", "fmt", "--output", "plain", "--verbosity", "minimal"], &config);
    assert!(!stderr(&minimal).contains("skipped"), "{}", stderr(&minimal));

    let overridden = azad_with(root, root, &["run", "fmt", "--output", "plain", "--include", "swift"], &config);
    assert_eq!(ran(&overridden), ["//apps/ios:fmt"]);
}

#[test]
fn list_shows_the_tags_of_tasks() {
    let workspace = apps_and_services();
    let listing = stdout(&azad(workspace.path(), &["list"]));
    let ios = listing.lines().find(|line| line.contains("//apps/ios:fmt")).expect("the task is listed");
    assert!(ios.trim_end().ends_with("Format  [app, swift]"), "{listing}");
}

const BUSYBOX: &str = "busybox@sha256:a7dbc319b1438115a207e11930dc4bb3e643dcb3ec0bdd694bb36cd8fad289ec";
const CONTAINER_TESTS_ENV: &str = "AZAD_CONTAINER_TESTS";

/// Whether this host has a container runtime; without one, the container tests say so and pass, unless
/// $AZAD_CONTAINER_TESTS asks for them.
fn has_container_runtime() -> bool {
    let found = ["docker", "podman"]
        .iter()
        .any(|runtime| Command::new(runtime).arg("version").output().is_ok_and(|output| output.status.success()));
    if !found && std::env::var_os(CONTAINER_TESTS_ENV).is_some() {
        panic!("${CONTAINER_TESTS_ENV} is set, and neither docker nor podman runs");
    }
    if !found {
        eprintln!("no container runtime: the container tests are skipped");
    }
    found
}

/// Tasks that run in images on the files of the workspace: from a reference, from a Dockerfile whose entrypoint fails, as
/// a tool's entrypoint would with the task's command as its arguments, and that copies from a named build context, with a
/// cache volume and a secret.
fn container_workspace() -> TempDir {
    let dir = repository();
    let root = dir.path();
    write(root, ".gitignore", ".store/\nout/\n");
    write(
        root,
        "tools/Dockerfile",
        &format!(
            "FROM {BUSYBOX} AS base\nRUN echo built > /built\nFROM base AS tools\nCOPY --from=extra greeting.txt /greeting\nENV FROM_IMAGE=yes\nENTRYPOINT [\"/bin/false\"]\n"
        ),
    );
    write(
        root,
        "tasks.star",
        &format!(
            r#"
busybox = image("{BUSYBOX}")
tools = image(dockerfile = "//tools/Dockerfile", target = "tools", contexts = {{"extra": "//extra"}})
whoami = task(name = "whoami", cmd = "mkdir -p out && id -u > out/uid && if test -e /.dockerenv -o -e /run/.containerenv; then echo container; else echo host; fi > out/where", local = "image", image = busybox)
built = task(name = "built", cmd = "mkdir -p out && cat /built > out/built", local = "image", image = tools)
from_image = task(name = "from-image", cmd = "echo file=$(cat /built) env=$FROM_IMAGE context=$(cat /greeting)", image = tools)
counted = task(name = "counted", cmd = "mkdir -p out && echo x >> /cache/count && wc -l < /cache/count > out/count", local = "image", image = busybox, caches = {{"/cache": "counted"}}, sources = ["counter"])
secret = task(name = "secret", cmd = "mkdir -p out && echo $THE_SECRET > out/secret", local = "image", image = busybox, secrets = ["THE_SECRET"])
"#
        ),
    );
    write(root, "counter", "1\n");
    write(root, "extra/greeting.txt", "hello\n");
    dir
}

fn azad_env(workspace: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let store = workspace.join(".store");
    Command::new(env!("CARGO_BIN_EXE_azad"))
        .args(["--store", &store.display().to_string(), "--root", &workspace.display().to_string()])
        .args(args)
        .current_dir(workspace)
        .env("AZAD_CONFIG", "")
        .envs(env.iter().copied())
        .output()
        .expect("azad starts")
}

fn read(root: &Path, path: &str) -> String {
    fs::read_to_string(root.join(path)).unwrap_or_else(|error| panic!("{path}: {error}"))
}

#[test]
fn a_task_runs_in_its_image_on_the_files_of_the_workspace_as_their_owner() {
    use std::os::unix::fs::MetadataExt;
    if !has_container_runtime() {
        return;
    }
    let workspace = container_workspace();
    let root = workspace.path();
    let run = azad_env(root, &["run", "//:whoami", "--output", "plain"], &[]);
    assert!(run.status.success(), "{}", stderr(&run));
    let owner = fs::metadata(root).expect("the workspace").uid();
    assert_eq!(read(root, "out/where").trim(), "container");
    assert_eq!(fs::metadata(root.join("out/uid")).expect("the file").uid(), owner, "the files are the owner's");

    let on_host = azad_env(root, &["run", "//:whoami", "--output", "plain", "--local", "host"], &[]);
    assert!(on_host.status.success(), "{}", stderr(&on_host));
    assert_eq!(read(root, "out/where").trim(), "host", "--local host runs it on this host");
}

#[test]
fn an_image_from_a_dockerfile_cache_volumes_and_secrets_work_in_a_container() {
    if !has_container_runtime() {
        return;
    }
    let workspace = container_workspace();
    let root = workspace.path();
    let built = azad_env(root, &["run", "//:built", "--output", "plain"], &[]);
    assert!(built.status.success(), "{}", stderr(&built));
    assert_eq!(read(root, "out/built").trim(), "built");

    for (run, expected) in [(1, "1"), (2, "2")] {
        write(root, "counter", &format!("{run}\n"));
        let counted = azad_env(root, &["run", "//:counted", "--output", "plain"], &[]);
        assert!(counted.status.success(), "{}", stderr(&counted));
        assert_eq!(read(root, "out/count").trim(), expected, "the cache volume keeps its files between runs");
    }

    let missing = azad_env(root, &["run", "//:secret", "--output", "plain"], &[]);
    assert!(!missing.status.success());
    assert!(stderr(&missing).contains("THE_SECRET"), "{}", stderr(&missing));
    let given = azad_env(root, &["run", "//:secret", "--output", "plain"], &[("THE_SECRET", "s3cret")]);
    assert!(given.status.success(), "{}", stderr(&given));
    assert_eq!(read(root, "out/secret").trim(), "s3cret");
}

/// A cache hit does not show what its action wrote, but the run says that it wrote something, and `--replay` shows it
/// as it was, without running the action again.
#[test]
fn a_run_says_that_cache_hits_wrote_output_and_replay_shows_it() {
    let workspace = repository();
    write(
        workspace.path(),
        "tasks.star",
        r#"
talk = task(name = "talk", cmd = "echo said once")
quiet = task(name = "quiet", cmd = "true")
goal("all", require = [talk, quiet])
"#,
    );
    let first = azad(workspace.path(), &["run", "//:all", "--output", "plain"]);
    assert!(first.status.success(), "{}", stderr(&first));

    let cached = stderr(&azad(workspace.path(), &["run", "//:all", "--output", "plain"]));
    let hint = cached.lines().find(|l| l.contains("--replay")).unwrap_or_else(|| panic!("{cached}"));
    assert!(hint.contains("1 cached action wrote output"), "{hint}");
    assert!(!cached.contains("said once"), "{cached}");

    let replayed = stderr(&azad(workspace.path(), &["run", "//:all", "--output", "plain", "--replay"]));
    assert!(replayed.lines().any(|l| l.trim_end().ends_with("//:talk | said once")), "{replayed}");
    assert!(replayed.contains("cached  //:talk") && !replayed.contains("--replay"), "{replayed}");
}

/// A package declared in `tasks.yaml` is a package like one in `tasks.star`: `azad list` shows it, a short name runs
/// its goal from its directory, and a pattern finds it beside a `tasks.star` package (decision 0027).
#[test]
fn a_tasks_yaml_package_lists_and_runs_like_a_tasks_star_package() {
    let workspace = repository();
    let root = workspace.path();
    write(root, ".gitignore", ".store/\nout/\n");
    write(
        root,
        "web/tasks.yaml",
        "tasks:\n  greet:\n    cmd: mkdir -p out && echo from yaml > out/greeting\n    generates: [out]\n    role: build\n",
    );
    write(root, "api/tasks.star", "t = task(name = \"build-api\", cmd = \"true\")\ngoal(\"build\", require = [t])\n");

    let listing = stdout(&azad(root, &["list"]));
    assert!(lists(&listing, "//web:build") && lists(&listing, "//web:greet"), "{listing}");

    let short = azad_in(root, &root.join("web"), &["run", "build", "--output", "plain"]);
    assert!(short.status.success(), "{}", stderr(&short));
    assert_eq!(read(root, "web/out/greeting").trim(), "from yaml");

    let pattern = azad(root, &["run", "//...:build", "--output", "plain"]);
    let output = stderr(&pattern);
    assert!(
        pattern.status.success() && output.contains("//api:build-api") && output.contains("//web:greet"),
        "{output}"
    );
}

/// A task whose tool is missing on the host says that its image has it; a task without an image does not, because
/// `--local image` would not help it.
#[test]
fn a_missing_tool_on_the_host_points_to_the_image_of_the_task() {
    let workspace = repository();
    write(
        workspace.path(),
        "tasks.star",
        r#"
tool = image("busybox@sha256:0000000000000000000000000000000000000000000000000000000000000000")
with_image = task(name = "with-image", cmd = "azad-test-missing-tool", image = tool)
without_image = task(name = "without-image", cmd = "azad-test-missing-tool")
goal("fmt", require = [with_image, without_image])
"#,
    );
    let run = azad(workspace.path(), &["run", "//:fmt", "--output", "plain"]);
    assert!(!run.status.success());
    let error = stderr(&run);
    let note = error.lines().find(|line| line.contains("--local image")).unwrap_or_else(|| panic!("{error}"));
    assert!(note.contains("//:with-image") && note.contains("exited with 127"), "{note}");
    assert!(!note.contains("//:without-image"), "{note}");
}

#[test]
fn without_a_container_runtime_a_task_in_its_image_says_how_to_go_on() {
    let workspace = container_workspace();
    let run = azad_env(
        workspace.path(),
        &["run", "//:whoami", "--output", "plain"],
        &[("AZAD_CONTAINER_RUNTIME", "/nonexistent/docker")],
    );
    assert!(!run.status.success());
    let error = stderr(&run);
    assert!(error.contains("Docker or Podman") && error.contains("--local host"), "{error}");

    let store = workspace.path().join(".store");
    let check = Command::new(env!("CARGO_BIN_EXE_azad"))
        .args(["--store", &store.display().to_string(), "check", "//:whoami"])
        .current_dir(workspace.path())
        .env("AZAD_CONFIG", "")
        .env("AZAD_CONTAINER_RUNTIME", "/nonexistent/docker")
        .output()
        .expect("azad starts");
    let report = stdout(&check);
    assert!(!check.status.success() && report.contains("//:whoami: runs in busybox@sha256:"), "{report}");

    let isolated = azad_env(
        workspace.path(),
        &["--isolated", "run", "//:built", "--output", "plain"],
        &[("AZAD_CONTAINER_RUNTIME", "/nonexistent/docker")],
    );
    assert!(!isolated.status.success());
    assert!(stderr(&isolated).contains("with Docker or Podman, and this host has neither"), "{}", stderr(&isolated));
}

/// An isolated run builds the image of a Dockerfile with this host's runtime, and runs the task in it in Azad's own
/// sandbox, with the image's files and `Env`; the next run finds the stored image of the same build (decision 0024,
/// amendment).
#[test]
fn an_isolated_run_builds_an_image_from_a_dockerfile_and_runs_in_its_sandbox() {
    if !has_container_runtime() {
        return;
    }
    let workspace = container_workspace();
    let run =
        || azad_env(workspace.path(), &["--isolated", "run", "//:from-image", "--force", "--output", "plain"], &[]);
    let first = run();
    let output = stderr(&first);
    assert!(first.status.success(), "{output}");
    assert!(output.contains("file=built env=yes context=hello"), "{output}");
    assert!(output.contains("stored the image of //tools/Dockerfile"), "{output}");
    let second = stderr(&run());
    assert!(second.contains("file=built env=yes context=hello") && !second.contains("stored the image"), "{second}");
}
