#![allow(clippy::unwrap_used)]

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::time::Duration;

use azad::engine::{Engine, EngineConfig, Event, Status, host_platform};
use azad::ir::Image;
use azad::oci::Puller;
use azad::remote::{DirectoryProvider, PublicKey, RemoteConfig, SigningKey};
use azad::sandbox::Limits;
use azad::store::{Selection, Store};
use support::{ROOTFS_PATH, TEST_PROGRAMS, host_rootfs};
use tempfile::TempDir;

fn engine(dir: &TempDir) -> Engine {
    Engine::new(config(dir)).unwrap()
}

fn config(dir: &TempDir) -> EngineConfig {
    EngineConfig {
        store: dir.path().join("store"),
        root: dir.path().join("workspace"),
        platform: host_platform(),
        limits: Limits { timeout: Duration::from_secs(30), ..Limits::default() },
        tools: Default::default(),
        isolated: false,
        color: false,
        force: false,
        sandbox: azad::SandboxKind::Bubblewrap,
        executor: None,
        trust: azad::Trust::Main,
        history: None,
        withheld: Default::default(),
        filter: Default::default(),
        local: None,
    }
}

/// A workspace whose goal `graph.azad:check` copies `src/in.txt`, which an input mounts at `/in`, and counts its lines.
fn copy_and_count(dir: &TempDir) {
    let workspace = dir.path().join("workspace");
    host_rootfs(&workspace.join("rootfs"), TEST_PROGRAMS);
    fs::create_dir_all(workspace.join("src")).unwrap();
    fs::write(workspace.join("src/in.txt"), "input\n").unwrap();
    let action = |name: &str, script: &str, input: &str| {
        format!(
            "action {name}\n  platform {}\n  root local(\"rootfs\")\n  env PATH \"{ROOTFS_PATH}\"\n  command \"sh\" \"-c\" {script:?}\n  input /in {input}\n  output out\n\n",
            host_platform()
        )
    };
    let graph = [
        action("copy", "mkdir out && cp /in/in.txt out/", "local(\"src\")"),
        action("count", "mkdir out && wc -l < /in/in.txt > out/count", "output(copy, \"out\")"),
        "goal check count\n".to_owned(),
    ]
    .concat();
    fs::write(workspace.join("graph.azad"), graph).unwrap();
}

#[test]
fn a_library_user_loads_runs_and_explains_a_goal() {
    let dir = TempDir::new().unwrap();
    copy_and_count(&dir);
    let engine = engine(&dir);
    let goal = engine.load("graph.azad:check", &mut |_| {}).unwrap();
    let first = engine.run(&goal, 2, &mut |_| {}).unwrap();
    assert!(first.outcome.goal_holds());
    assert_eq!(first.number, 1);

    let second = engine.run(&goal, 2, &mut |_| {}).unwrap();
    assert!(second.outcome.statuses.values().all(|s| matches!(s, Status::Cached { .. })));
    let explained = engine.explain(&goal, "count", false).unwrap();
    assert_eq!(explained.run, Some(2));
    assert!(explained.explanation.to_string().contains("the cache has the result"));
}

#[test]
fn why_names_the_changed_files_of_an_input_by_the_path_at_which_the_action_sees_them() {
    let dir = TempDir::new().unwrap();
    copy_and_count(&dir);
    let engine = engine(&dir);
    let goal = engine.load("graph.azad:check", &mut |_| {}).unwrap();
    assert!(engine.run(&goal, 2, &mut |_| {}).unwrap().outcome.goal_holds());
    fs::write(dir.path().join("workspace/src/in.txt"), "input\nmore\n").unwrap();

    let goal = engine.load("graph.azad:check", &mut |_| {}).unwrap();
    let copy = engine.explain(&goal, "copy", true).unwrap().explanation.to_string();
    assert!(copy.ends_with("It must run, because it changed since run 1:\n  /in/in.txt changed"), "{copy}");
    assert!(engine.run(&goal, 2, &mut |_| {}).unwrap().outcome.goal_holds());
    let count = engine.explain(&goal, "count", false).unwrap().explanation.to_string();
    assert!(count.ends_with("It ran, because it changed since run 1:\n  /in/in.txt changed\nIt exited 0."), "{count}");
}

#[test]
fn each_workspace_and_file_has_its_own_run_history() {
    let dir = TempDir::new().unwrap();
    let store = dir.path().join("store");
    let mut numbers = Vec::new();
    for (workspace, file) in [("one", "a.azad"), ("one", "b.azad"), ("two", "a.azad")] {
        let root = dir.path().join(workspace);
        if !root.join("rootfs").exists() {
            host_rootfs(&root.join("rootfs"), TEST_PROGRAMS);
        }
        let action = format!(
            "action hello\n  platform {}\n  root local(\"rootfs\")\n  env PATH \"{ROOTFS_PATH}\"\n  command \"true\"\n\ngoal check hello\n",
            host_platform()
        );
        fs::write(root.join(file), action).unwrap();
        let config = EngineConfig {
            store: store.clone(),
            root,
            platform: host_platform(),
            limits: Limits { timeout: Duration::from_secs(30), ..Limits::default() },
            tools: Default::default(),
            isolated: false,
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
        let engine = Engine::new(config).unwrap();
        let goal = engine.load(&format!("{file}:check"), &mut |_| {}).unwrap();
        numbers.push(engine.run(&goal, 1, &mut |_| {}).unwrap().number);
    }
    assert_eq!(numbers, [1, 1, 1]);
}

#[test]
fn an_unknown_target_or_goal_is_reported() {
    let dir = TempDir::new().unwrap();
    fs::create_dir_all(dir.path().join("workspace")).unwrap();
    fs::write(dir.path().join("workspace/empty.azad"), "").unwrap();
    let engine = engine(&dir);
    let no_frontend = engine.load("something-else", &mut |_| {}).unwrap_err().to_string();
    assert!(no_frontend.contains("no frontend reads"), "{no_frontend}");
    let no_goal = engine.load("empty.azad:check", &mut |_| {}).unwrap_err().to_string();
    assert!(no_goal.contains("there is no goal check"), "{no_goal}");
}

#[test]
fn two_inline_sources_with_the_same_names_but_other_content_stay_apart() {
    use azad::frontend::{Frontend, FrontendError, InlineFile, LoadRequest, Loaded, Package, SourceSpec};
    use azad::ir::{Kind, Spec};

    struct TwoPlans;
    impl Frontend for TwoPlans {
        fn name(&self) -> &'static str {
            "two-plans"
        }
        fn accepts(&self, target: &str) -> bool {
            target == "two"
        }
        fn load(&self, _request: &LoadRequest<'_>) -> Result<Loaded, FrontendError> {
            let action = |content: &str| Spec {
                kind: Kind::Impure,
                platform: host_platform(),
                root: SourceSpec::Inline { files: BTreeMap::new() },
                command: vec!["true".to_owned()],
                env: BTreeMap::new(),
                inputs: BTreeMap::from([(
                    "/plan".to_owned(),
                    SourceSpec::Inline {
                        files: BTreeMap::from([(
                            "job.json".to_owned(),
                            InlineFile { content: content.as_bytes().to_vec(), executable: false },
                        )]),
                    },
                )]),
                outputs: Default::default(),
                caches: BTreeMap::new(),
                work: BTreeMap::new(),
                dir: String::new(),
                secrets: Default::default(),
            };
            let package = Package {
                actions: BTreeMap::from([("a".to_owned(), action("first")), ("b".to_owned(), action("second"))]),
                goals: BTreeMap::from([("both".to_owned(), ["a".to_owned(), "b".to_owned()].into())]),
                ..Package::default()
            };
            Ok(Loaded { package, goal: "both".to_owned(), scope: None })
        }
    }

    let dir = TempDir::new().unwrap();
    fs::create_dir_all(dir.path().join("workspace")).unwrap();
    let engine = engine(&dir).with_frontend(Box::new(TwoPlans));
    let goal = engine.load("two", &mut |_| {}).unwrap();
    assert_ne!(goal.graph.actions["a"].inputs["/plan"], goal.graph.actions["b"].inputs["/plan"]);
}

const IMAGE: &str = "tools@sha256:00000000000000000000000000000000000000000000000000000000000000aa";

fn variables(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(name, value)| ((*name).to_owned(), (*value).to_owned())).collect()
}

/// A workspace whose action has the root `IMAGE`, which the store of `dir` knows as if it were pulled.
fn workspace_with_image(dir: &TempDir) {
    let rootfs = dir.path().join("rootfs");
    host_rootfs(&rootfs, TEST_PROGRAMS);
    let store = Store::open(dir.path().join("store")).unwrap();
    let tree = store.ingest(&rootfs, &Selection::All).unwrap();
    let env = variables(&[("PATH", ROOTFS_PATH), ("LANG", "C.UTF-8"), ("GREETING", "from the image")]);
    Puller::new(store).unwrap().remember(IMAGE, &host_platform(), &Image { tree, env }).unwrap();
    let workspace = dir.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    let tasks = format!(
        "hello = action(name = \"hello\", cmd = [\"sh\", \"-c\", \"echo $GREETING $LANG\"], root = image({IMAGE:?}), env = {{\"GREETING\": \"from the action\"}})\ngoal(\"check\", require = [hello])\n"
    );
    fs::write(workspace.join("tasks.star"), tasks).unwrap();
}

#[test]
fn an_image_root_gives_the_base_of_the_environment_and_the_action_env_replaces_its_variables() {
    let dir = TempDir::new().unwrap();
    workspace_with_image(&dir);
    let engine = engine(&dir);
    let goal = engine.load("//:check", &mut |_| {}).unwrap();
    let key = goal.action_key("hello").unwrap();
    let expected = variables(&[("PATH", ROOTFS_PATH), ("LANG", "C.UTF-8"), ("GREETING", "from the action")]);
    assert_eq!(goal.graph.actions[&key].env, expected);
    let report = engine.run(&goal, 1, &mut |_| {}).unwrap();
    assert!(report.outcome.goal_holds());
    let (_, result) = engine.last_result(&goal, "hello").unwrap().unwrap();
    assert_eq!(String::from_utf8(engine.read_blob(&result.stdout).unwrap()).unwrap(), "from the action C.UTF-8\n");
}

#[test]
fn a_new_machine_gets_the_environment_of_an_image_from_the_remote_cache_without_a_pull() {
    let remote = TempDir::new().unwrap();
    let remote_config = |signing_key: Option<SigningKey>, trusted: Vec<PublicKey>| RemoteConfig {
        provider: Box::new(DirectoryProvider::new(remote.path().to_path_buf())),
        read: true,
        write: signing_key.is_some(),
        signing_key,
        trusted,
    };
    let key = SigningKey::generate("ci").unwrap();
    let public = key.public();

    let first = TempDir::new().unwrap();
    workspace_with_image(&first);
    let writer = Engine::with_remote(config(&first), remote_config(Some(key), vec![])).unwrap();
    let goal = writer.load("//:check", &mut |_| {}).unwrap();
    assert!(writer.run(&goal, 1, &mut |_| {}).unwrap().outcome.goal_holds());

    let second = TempDir::new().unwrap();
    fs::create_dir_all(second.path().join("workspace")).unwrap();
    fs::copy(first.path().join("workspace/tasks.star"), second.path().join("workspace/tasks.star")).unwrap();
    let reader = Engine::with_remote(config(&second), remote_config(None, vec![public])).unwrap();
    let mut pulls = Vec::new();
    let fresh = reader
        .load("//:check", &mut |event| {
            if let Event::Pull { reference } = event {
                pulls.push(reference);
            }
        })
        .unwrap();
    assert_eq!(pulls, Vec::<String>::new());
    assert_eq!(fresh.graph, goal.graph, "the same actions, with the image's environment, without a pull");
    let report = reader.run(&fresh, 1, &mut |_| {}).unwrap();
    assert!(report.outcome.statuses.values().all(|s| matches!(s, Status::Cached { .. })));
}
