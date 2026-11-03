#![allow(clippy::unwrap_used)]

use std::collections::{BTreeMap, BTreeSet};

use azad::ir::action::{Container, ContainerImage};
use azad::ir::text::{self, SourceSpec};
use azad::ir::{Action, Canonical, Digest, Graph, Kind, Source, Spec};
use proptest::prelude::*;

const ROOT: &str = "sha256:76a40fb768ec1ccbb0c762081b3557ebaddfb870ad9a58f5349ddf7deb22fe72/19";

fn sample_text() -> String {
    format!(
        r#"
# A sample graph.
action build
  platform linux/amd64
  root {ROOT}
  command "sh" "-c" "mkdir out && echo \"hi\" > out/hello"
  env LANG "C"
  input /src {ROOT}
  output out

action test
  kind hermetic
  platform linux/amd64
  root {ROOT}
  command "sh" "-c" "cat /build/hello"
  input /build output(build, "out")

goal check build test
"#
    )
}

fn sample_graph() -> Graph {
    text::parse(&sample_text()).unwrap().resolve(|_| Err::<Digest, ()>(())).unwrap()
}

#[test]
fn the_sample_parses_and_verifies() {
    let graph = sample_graph();
    assert_eq!(graph.verify(), Ok(()));
    assert_eq!(graph.topological_order().unwrap(), ["build", "test"]);
    assert_eq!(graph.actions["build"].command[2], "mkdir out && echo \"hi\" > out/hello");
}

#[test]
fn a_resolved_action_has_a_fixed_digest() {
    let action = sample_graph().actions["build"].resolve(|_, _| None).unwrap();
    assert_eq!(
        action.digest().to_string(),
        "sha256:004541678af60c714d3d6846463dcf51201e5d88205bab9b0bc513b632a1e2fd/208"
    );
}

#[test]
fn printing_and_parsing_give_back_the_same_graph() {
    let graph = sample_graph();
    let printed = text::print(&graph);
    let reparsed = text::parse(&printed).unwrap().resolve(|_| Err::<Digest, ()>(())).unwrap();
    assert_eq!(reparsed, graph);
}

#[test]
fn local_sources_are_replaced_by_the_caller() {
    let document = text::parse("action a\n  platform p\n  root local(\"rootfs\")\n  command x\n").unwrap();
    assert_eq!(document.actions["a"].root, SourceSpec::Local("rootfs".to_owned()));
    let tree = Digest::of_bytes(b"rootfs");
    let graph = document.resolve(|path| match path {
        "rootfs" => Ok::<_, ()>(tree),
        _ => Err(()),
    });
    assert_eq!(graph.unwrap().actions["a"].root, Source::Tree(tree));
}

#[test]
fn the_graph_and_the_action_round_trip_through_their_encoding() {
    let graph = sample_graph();
    assert_eq!(Graph::decode(&graph.encode()), Ok(graph.clone()));
    let action = graph.actions["build"].resolve(|_, _| None).unwrap();
    assert_eq!(Action::decode(&action.encode()), Ok(action));
}

#[test]
fn resolve_reports_the_first_missing_output() {
    let graph = sample_graph();
    let missing = graph.actions["test"].resolve(|_, _| None).unwrap_err();
    assert_eq!(missing, Source::Output { action: "build".to_owned(), path: "out".to_owned() });
    let tree = Digest::of_bytes(b"out");
    let resolved = graph.actions["test"].resolve(|a, p| (a == "build" && p == "out").then_some(tree)).unwrap();
    assert_eq!(resolved.inputs["/build"], tree);
}

#[test]
fn parse_errors_name_the_line() {
    let err = text::parse("action a\n  platform p\n  bogus 1\n").unwrap_err();
    assert_eq!(err.line, 3);
    assert!(text::parse("action a\n  command \"open\n").is_err());
    assert!(text::parse("  command x\n").is_err());
    assert!(text::parse("action a\n  command x\n").unwrap_err().message.contains("platform is missing"));
}

fn messages(graph: &Graph) -> Vec<String> {
    graph.verify().unwrap_err().into_iter().map(|e| e.to_string()).collect()
}

fn with_action(mutate: impl FnOnce(&mut Spec<Source>)) -> Graph {
    let mut graph = sample_graph();
    mutate(graph.actions.get_mut("build").unwrap());
    graph
}

#[test]
fn the_verifier_refuses_each_kind_of_bad_ir() {
    let cases: Vec<(Graph, &str)> = vec![
        (with_action(|a| a.command.clear()), "the command is empty"),
        (with_action(|a| a.platform.clear()), "the platform is empty"),
        (with_action(|a| _ = a.env.insert("A=B".to_owned(), String::new())), "environment variable name"),
        (with_action(|a| _ = a.inputs.insert("relative".to_owned(), a.root.clone())), "must be absolute"),
        (with_action(|a| _ = a.inputs.insert("/src/../etc".to_owned(), a.root.clone())), "must be absolute"),
        (with_action(|a| _ = a.inputs.insert("/work/x".to_owned(), a.root.clone())), "overlaps the work directory"),
        (with_action(|a| _ = a.inputs.insert("/src/sub".to_owned(), a.root.clone())), "input paths"),
        (with_action(|a| _ = a.outputs.insert("/abs".to_owned())), "must be relative"),
        (with_action(|a| _ = a.outputs.insert("out/sub".to_owned())), "output paths"),
        (
            with_action(|a| a.kind = Kind::FixedOutput { expected: BTreeMap::new() }),
            "expect a digest for exactly its declared outputs",
        ),
        (
            with_action(|a| {
                a.root = Source::Output { action: "nope".to_owned(), path: "out".to_owned() };
            }),
            "which does not exist",
        ),
        (
            with_action(|a| {
                a.root = Source::Output { action: "test".to_owned(), path: "nothing".to_owned() };
            }),
            "which does not declare it",
        ),
    ];
    for (graph, expected) in cases {
        let found = messages(&graph);
        assert!(found.iter().any(|m| m.contains(expected)), "expected {expected:?}, found {found:?}");
    }
}

#[test]
fn the_verifier_refuses_cycles_and_bad_goals() {
    let mut graph = with_action(|a| {
        a.inputs.insert("/test".to_owned(), Source::Output { action: "test".to_owned(), path: "out".to_owned() });
        a.outputs.insert("other".to_owned());
    });
    graph.actions.get_mut("test").unwrap().outputs.insert("out".to_owned());
    assert!(messages(&graph).iter().any(|m| m.contains("cycle")));

    let mut graph = sample_graph();
    graph.goals.insert("empty".to_owned(), BTreeSet::new());
    graph.goals.insert("ghost".to_owned(), BTreeSet::from(["missing".to_owned()]));
    let found = messages(&graph);
    assert!(found.iter().any(|m| m.contains("at least one action")));
    assert!(found.iter().any(|m| m.contains("there is no action missing")));
}

fn arb_digest() -> impl Strategy<Value = Digest> {
    proptest::collection::vec(any::<u8>(), 0..8).prop_map(|b| Digest::of_bytes(&b))
}

fn arb_action() -> impl Strategy<Value = Action> {
    let kind = prop_oneof![
        Just(Kind::Hermetic),
        Just(Kind::Impure),
        ("/[a-z]{1,8}", proptest::option::of(arb_container()))
            .prop_map(|(workspace, container)| Kind::Host { workspace, container }),
    ];
    (
        kind,
        "[a-z/]{1,12}",
        arb_digest(),
        proptest::collection::vec(".{0,10}", 1..4),
        proptest::collection::btree_map("[A-Z]{1,5}", ".{0,8}", 0..4),
        proptest::collection::btree_map("/[a-z]{1,6}", arb_digest(), 0..4),
        proptest::collection::btree_set("[a-z]{1,6}", 0..4),
        proptest::collection::btree_map("/cache/[a-z]{1,6}", "[a-z]{1,6}", 0..3),
        (
            proptest::collection::btree_map("[a-z]{0,6}", arb_digest(), 0..3),
            "[a-z/]{0,8}",
            proptest::collection::btree_set("[A-Z_]{1,8}", 0..3),
        ),
    )
        .prop_map(|(kind, platform, root, command, env, inputs, outputs, caches, (work, dir, secrets))| Spec {
            kind,
            platform,
            root,
            command,
            env,
            inputs,
            outputs,
            caches,
            work,
            dir,
            secrets,
        })
}

fn arb_container() -> impl Strategy<Value = Container> {
    let image = prop_oneof![
        "[a-z]{1,8}@sha256:[0-9a-f]{8}".prop_map(ContainerImage::Reference),
        (
            "[a-z/]{1,10}",
            "[a-z/]{0,6}",
            "[a-z]{0,5}",
            proptest::collection::btree_map("[A-Z]{1,5}", "[a-z0-9]{0,6}", 0..3),
            proptest::collection::btree_map("[a-z-]{1,8}", "[a-z/]{0,8}", 0..3),
        )
            .prop_map(|(file, context, target, args, contexts)| ContainerImage::Dockerfile {
                file,
                context,
                target,
                args,
                contexts,
            }),
    ];
    (image, proptest::collection::btree_map("/[a-z]{1,6}", "[a-z]{1,6}", 0..3))
        .prop_map(|(image, volumes)| Container { image, volumes })
}

proptest! {
    #[test]
    fn decoding_is_the_inverse_of_encoding_for_actions(action in arb_action()) {
        prop_assert_eq!(Action::decode(&action.encode()), Ok(action));
    }
}

#[test]
fn caches_are_encoded_only_when_declared_and_checked() {
    let graph = sample_graph();
    let without = graph.actions["build"].resolve(|_, _| None).unwrap();
    assert!(!String::from_utf8_lossy(&without.encode()).contains("cargo-target"));

    let mut with_cache = graph.clone();
    with_cache.actions.get_mut("build").unwrap().caches.insert("/tmp/target".to_owned(), "cargo-target".to_owned());
    let action = with_cache.actions["build"].resolve(|_, _| None).unwrap();
    assert_ne!(action.digest(), without.digest());
    assert_eq!(Action::decode(&action.encode()), Ok(action));
    let printed = text::print(&with_cache);
    assert!(printed.contains("cache \"/tmp/target\" \"cargo-target\""), "{printed}");
    assert_eq!(text::parse(&printed).unwrap().resolve(|_| Err::<Digest, ()>(())).unwrap(), with_cache);

    let cases = [
        ("relative", "x", "must be absolute"),
        ("/work", "x", "not \"/\" or /work"),
        ("/work/out/cache", "x", "overlaps an output"),
        ("/src/cache", "x", "overlaps an input"),
        ("/cache", "bad/name", "cache name"),
    ];
    for (path, name, expected) in cases {
        let bad = with_action(|a| _ = a.caches.insert(path.to_owned(), name.to_owned()));
        let found = messages(&bad);
        assert!(found.iter().any(|m| m.contains(expected)), "expected {expected:?}, found {found:?}");
    }
}

proptest! {
    #[test]
    fn printing_and_parsing_keep_the_container_and_the_secrets_of_a_host_action(
        container in proptest::option::of(arb_container()),
        secrets in proptest::collection::btree_set("[A-Z_]{1,8}", 0..3),
    ) {
        let mut graph = sample_graph();
        let action = graph.actions.get_mut("build").unwrap();
        action.kind = Kind::Host { workspace: "/home/me/repo".to_owned(), container };
        action.secrets = secrets;
        action.inputs.clear();
        let reparsed = text::parse(&text::print(&graph)).unwrap().resolve(|_| Err::<Digest, ()>(())).unwrap();
        prop_assert_eq!(reparsed, graph);
    }
}
