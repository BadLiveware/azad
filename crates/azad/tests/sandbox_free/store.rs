#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;

use azad::ir::{Digest, Directory, Node};
use azad::store::{Selection, Store, StoreError};
use azad::trust::Trust;
use proptest::prelude::*;
use tempfile::TempDir;

fn store() -> (TempDir, Store) {
    let dir = TempDir::new().unwrap();
    let store = Store::open(dir.path().join("store")).unwrap();
    (dir, store)
}

fn write(root: &Path, relative: &str, content: &[u8], executable: bool) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, content).unwrap();
    let mode = match executable {
        true => 0o755,
        false => 0o644,
    };
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
}

#[test]
fn put_file_and_put_bytes_agree_and_read_back() {
    let (dir, store) = store();
    let path = dir.path().join("f");
    fs::write(&path, b"hello").unwrap();
    let from_file = store.put_file(&path).unwrap();
    let from_bytes = store.put_bytes(b"hello").unwrap();
    assert_eq!(from_file, from_bytes);
    assert_eq!(store.read(&from_file).unwrap(), b"hello");
}

#[test]
fn a_corrupt_blob_is_detected() {
    let (_dir, store) = store();
    let digest = store.put_bytes(b"hello").unwrap();
    let path = store.blob_path(&digest);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    fs::write(&path, b"jello").unwrap();
    assert!(matches!(store.read(&digest), Err(StoreError::Corrupt { .. })));
}

#[test]
fn a_missing_blob_is_reported() {
    let (_dir, store) = store();
    assert!(matches!(store.read(&Digest::of_bytes(b"absent")), Err(StoreError::Missing(_))));
}

#[test]
fn content_and_the_executable_bit_change_the_digest() {
    let (dir, store) = store();
    let root = dir.path().join("src");
    write(&root, "a/run.sh", b"echo hi", true);
    let base = store.ingest(&root, &Selection::All).unwrap();

    write(&root, "a/run.sh", b"echo hi", false);
    let not_executable = store.ingest(&root, &Selection::All).unwrap();
    assert_ne!(base, not_executable);

    write(&root, "a/run.sh", b"echo ho", true);
    let other_content = store.ingest(&root, &Selection::All).unwrap();
    assert_ne!(base, other_content);

    write(&root, "a/run.sh", b"echo hi", true);
    assert_eq!(store.ingest(&root, &Selection::All).unwrap(), base);
}

#[test]
fn read_and_write_bits_do_not_change_the_digest() {
    let (dir, store) = store();
    let root = dir.path().join("src");
    write(&root, "f", b"x", false);
    let base = store.ingest(&root, &Selection::All).unwrap();
    fs::set_permissions(root.join("f"), fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(store.ingest(&root, &Selection::All).unwrap(), base);
}

#[test]
fn materialize_gives_back_the_same_tree() {
    let (dir, store) = store();
    let root = dir.path().join("src");
    write(&root, "bin/tool", b"#!/bin/sh", true);
    write(&root, "share/data.txt", b"data", false);
    fs::create_dir_all(root.join("empty")).unwrap();
    symlink("bin/tool", root.join("tool")).unwrap();
    let tree = store.ingest(&root, &Selection::All).unwrap();

    let copy = store.materialize(&tree).unwrap();
    assert_eq!(store.ingest(&copy, &Selection::All).unwrap(), tree);
    assert_eq!(store.materialize(&tree).unwrap(), copy);
    assert_eq!(fs::read_link(copy.join("tool")).unwrap(), Path::new("bin/tool"));
    assert_eq!(fs::metadata(copy.join("bin/tool")).unwrap().permissions().mode() & 0o777, 0o555);
}

/// Calls that need a tree at the same time share one directory, and only one of them makes it, so only that one has
/// the time of the copy to report.
#[test]
fn one_of_the_calls_that_share_a_tree_at_the_same_time_makes_it() {
    const CALLS: usize = 8;
    let (dir, store) = store();
    let root = dir.path().join("src");
    for n in 0..100 {
        write(&root, &format!("files/{n}.txt"), n.to_string().as_bytes(), false);
    }
    let tree = store.ingest(&root, &Selection::All).unwrap();

    let shared: Vec<_> = std::thread::scope(|scope| {
        let calls: Vec<_> = (0..CALLS).map(|_| scope.spawn(|| store.share(&tree).unwrap())).collect();
        calls.into_iter().map(|call| call.join().unwrap()).collect()
    });
    assert_eq!(shared.iter().filter(|one| one.made).count(), 1, "{shared:?}");
    assert!(shared.iter().all(|one| one.path == shared[0].path), "{shared:?}");
    assert!(!store.share(&tree).unwrap().made, "a tree that is there is not made again");
}

#[test]
fn globs_select_files_and_drop_empty_directories() {
    let (dir, store) = store();
    let root = dir.path().join("src");
    write(&root, "src/main.rs", b"fn main() {}", false);
    write(&root, "src/notes.md", b"notes", false);
    write(&root, "docs/guide.md", b"guide", false);
    write(&root, "src/skip.rs", b"skipped", false);
    write(&root, "top.md", b"top", false);
    let tree = store.ingest(&root, &Selection::globs(&["**/*.rs", "*.md"], &["src/skip.rs"]).unwrap()).unwrap();

    let top: Directory = store.get(&tree).unwrap();
    assert_eq!(top.entries().keys().collect::<Vec<_>>(), ["src", "top.md"], "*.md does not match docs/guide.md");
    let Node::Directory { digest } = &top.entries()["src"] else { panic!("src is a directory") };
    let src: Directory = store.get(digest).unwrap();
    assert_eq!(src.entries().keys().collect::<Vec<_>>(), ["main.rs"]);
}

// APFS refuses a name that is not UTF-8, so on macOS no such file can exist.
#[cfg(not(target_os = "macos"))]
#[test]
fn a_name_that_is_not_utf8_is_refused() {
    use std::os::unix::ffi::OsStrExt;
    let (dir, store) = store();
    let root = dir.path().join("src");
    fs::create_dir_all(&root).unwrap();
    let name = std::ffi::OsStr::from_bytes(b"bad\xff");
    fs::write(root.join(name), b"x").unwrap();
    assert!(matches!(store.ingest(&root, &Selection::All), Err(StoreError::NotUtf8(_))));
}

fn arb_files() -> impl Strategy<Value = BTreeMap<String, (Vec<u8>, bool)>> {
    let path = proptest::collection::vec("[a-z]{1,4}", 1..4).prop_map(|parts| parts.join("/"));
    let file = (proptest::collection::vec(any::<u8>(), 0..32), any::<bool>());
    proptest::collection::btree_map(path, file, 1..12).prop_filter("no path is a prefix of another", |files| {
        files.keys().all(|a| files.keys().all(|b| a == b || !b.starts_with(&format!("{a}/"))))
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn the_order_of_creation_does_not_change_the_digest(files in arb_files(), seed in any::<u64>()) {
        let (dir, store) = store();
        let forward = dir.path().join("forward");
        for (path, (content, executable)) in &files {
            write(&forward, path, content, *executable);
        }
        let shuffled = dir.path().join("shuffled");
        let mut order: Vec<_> = files.iter().collect();
        order.sort_by_key(|(path, _)| path.bytes().fold(seed, |h, b| h.rotate_left(5) ^ u64::from(b)));
        for (path, (content, executable)) in order {
            write(&shuffled, path, content, *executable);
        }
        prop_assert_eq!(
            store.ingest(&forward, &Selection::All).unwrap(),
            store.ingest(&shuffled, &Selection::All).unwrap()
        );
    }
}

#[test]
fn a_known_directory_has_a_fixed_digest_on_every_machine() {
    let (dir, store) = store();
    let root = dir.path().join("src");
    write(&root, "bin/build.sh", b"#!/bin/sh\necho build\n", true);
    write(&root, "README", b"azad\n", false);
    fs::create_dir_all(root.join("empty")).unwrap();
    symlink("bin/build.sh", root.join("build")).unwrap();
    assert_eq!(
        store.ingest(&root, &Selection::All).unwrap().to_string(),
        "sha256:1deae43782a909c0fcf6a1848bf1d7baa48542c3de6e1d2792c08f480ae386af/252"
    );
}

fn result_with(store: &Store, out: &[u8]) -> azad::ir::ActionResult {
    azad::ir::ActionResult {
        exit_code: 0,
        outputs: BTreeMap::from([("out".to_owned(), store.put_bytes(out).unwrap())]),
        stdout: store.put_bytes(b"log").unwrap(),
        stderr: store.put_bytes(b"").unwrap(),
        log: None,
    }
}

#[test]
fn the_action_cache_keeps_results_while_their_blobs_exist() {
    let (_dir, store) = store();
    let state = store.state();
    let action = Digest::of_bytes(b"action");
    assert_eq!(state.result(&action).unwrap(), None);

    let result = result_with(&store, b"output");
    state.put_result(&action, &result).unwrap();
    assert_eq!(state.result(&action).unwrap(), Some(result.clone()));

    let blob = store.blob_path(&result.outputs["out"]);
    fs::remove_file(blob).unwrap();
    assert_eq!(state.result(&action).unwrap(), None, "a result whose output is gone is not a hit");
}

#[test]
fn an_alias_names_a_digest_that_is_in_the_store() {
    let (_dir, store) = store();
    let state = store.state();
    let tree = store.put_bytes(b"tree").unwrap();
    state.set_alias("image x linux/amd64", &tree).unwrap();
    assert_eq!(state.alias("image x linux/amd64").unwrap(), Some(tree));
    assert_eq!(state.alias("image y linux/amd64").unwrap(), None);
    state.set_alias("image z", &Digest::of_bytes(b"not stored")).unwrap();
    assert_eq!(state.alias("image z").unwrap(), None);
}

#[test]
fn runs_are_numbered_per_goal() {
    let (_dir, store) = store();
    let state = store.state();
    assert_eq!(state.last_run("//a:check").unwrap(), None);
    assert_eq!(state.record_run("//a:check", b"first").unwrap(), 1);
    assert_eq!(state.record_run("//a:check", b"second").unwrap(), 2);
    assert_eq!(state.record_run("//b:check", b"other").unwrap(), 1);
    assert_eq!(state.last_run("//a:check").unwrap(), Some((2, b"second".to_vec())));
    assert_eq!(store.state().last_run("//b:check").unwrap(), Some((1, b"other".to_vec())));
}

#[test]
fn the_stores_own_trees_share_file_content_with_the_blobs() {
    use std::os::unix::fs::MetadataExt;
    let (dir, store) = store();
    let root = dir.path().join("src");
    write(&root, "data.txt", b"shared", false);
    write(&root, "tool", b"#!/bin/sh", true);
    let tree = store.ingest(&root, &Selection::All).unwrap();
    let shared = store.materialize(&tree).unwrap();
    let blob = store.blob_path(&store.put_bytes(b"shared").unwrap());
    assert_eq!(fs::metadata(shared.join("data.txt")).unwrap().ino(), fs::metadata(&blob).unwrap().ino());
    assert_eq!(fs::metadata(shared.join("tool")).unwrap().permissions().mode() & 0o777, 0o555);

    let copy = dir.path().join("copy");
    store.materialize_into(&tree, &copy).unwrap();
    assert_ne!(fs::metadata(copy.join("data.txt")).unwrap().ino(), fs::metadata(&blob).unwrap().ino());
}

#[test]
fn cache_volumes_are_listed_and_cleared_unless_held() {
    let (_dir, store) = store();
    let a = store.cache_volume("cargo", "linux/amd64", Trust::Main).unwrap();
    fs::create_dir_all(a.path().join("ro")).unwrap();
    fs::set_permissions(a.path().join("ro"), fs::Permissions::from_mode(0o555)).unwrap();
    let b = store.cache_volume("go", "linux/arm64", Trust::Main).unwrap();
    drop(a);
    let names: Vec<(String, String)> = store.caches().unwrap().into_iter().map(|c| (c.name, c.platform)).collect();
    assert_eq!(names, [("cargo".to_owned(), "linux/amd64".to_owned()), ("go".to_owned(), "linux/arm64".to_owned())]);

    assert_eq!(store.clear_caches(None).unwrap(), 1, "a held volume stays");
    assert_eq!(store.caches().unwrap().len(), 1);
    drop(b);
    assert_eq!(store.clear_caches(Some("go")).unwrap(), 1);
    assert!(store.caches().unwrap().is_empty());
}

fn tree_of(store: &Store, root: &Path, files: &[(&str, &[u8])]) -> Digest {
    if root.exists() {
        fs::remove_dir_all(root).unwrap();
    }
    for (path, content) in files {
        write(root, path, content, false);
    }
    store.ingest(root, &Selection::All).unwrap()
}

fn modified(path: &Path) -> std::time::SystemTime {
    fs::metadata(path).unwrap().modified().unwrap()
}

#[test]
fn a_checkout_rewrites_exactly_the_files_whose_content_changed() {
    let (dir, store) = store();
    let src = dir.path().join("src");
    let one = tree_of(&store, &src, &[("a/lib.rs", b"1"), ("app/main.rs", b"x"), ("gone.rs", b"g")]);
    let two = tree_of(&store, &src, &[("a/lib.rs", b"2"), ("app/main.rs", b"x"), ("new/n.rs", b"n")]);
    let back = tree_of(&store, &src, &[("a/lib.rs", b"1"), ("app/main.rs", b"y"), ("new/n.rs", b"n")]);
    let checkout = dir.path().join("checkout");

    store.check_out(&one, &checkout).unwrap();
    let unchanged = modified(&checkout.join("app/main.rs"));
    store.check_out(&two, &checkout).unwrap();
    assert_eq!(store.ingest(&checkout, &Selection::All).unwrap(), two);
    assert_eq!(modified(&checkout.join("app/main.rs")), unchanged, "a file that did not change keeps its time");
    let built = std::time::SystemTime::now();

    // `a/lib.rs` comes back to content that the store has had since before `built`.
    store.check_out(&back, &checkout).unwrap();
    assert_eq!(store.ingest(&checkout, &Selection::All).unwrap(), back);
    assert!(modified(&checkout.join("a/lib.rs")) >= built, "content that came back is newer than the last build");
    assert!(modified(&checkout.join("app/main.rs")) >= built);
}

#[test]
fn a_forgotten_checkout_is_written_again_in_full() {
    let (dir, store) = store();
    let tree = tree_of(&store, &dir.path().join("src"), &[("a.rs", b"a")]);
    let checkout = dir.path().join("checkout");
    store.check_out(&tree, &checkout).unwrap();
    let before = modified(&checkout.join("a.rs"));
    store.check_out(&tree, &checkout).unwrap();
    assert_eq!(modified(&checkout.join("a.rs")), before);

    store.forget_checkout(&checkout).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(10));
    store.check_out(&tree, &checkout).unwrap();
    assert!(modified(&checkout.join("a.rs")) > before);
}

/// Between `fork` and `exec`, a new process has a copy of every open file of Azad, also the lock of a volume. When the
/// holder drops the volume in that time, the volume must still be free for the next action; before, the next action
/// got an empty private volume, and a build that its tool keeps there started cold.
#[test]
#[allow(unsafe_code)]
fn a_volume_is_free_when_its_holder_drops_it_while_a_new_process_starts() {
    use std::os::unix::process::CommandExt;
    use std::process::Command;
    use std::time::Duration;

    const CHILD_WAIT: Duration = Duration::from_millis(600);
    const FORK_TIME: Duration = Duration::from_millis(200);

    let (_dir, store) = store();
    let held = store.cache_volume("cargo", "linux/amd64", Trust::Main).unwrap();
    assert!(held.is_shared());
    let mut starting = Command::new("true");
    // SAFETY: the hook runs in the child between fork and exec, and only sleeps, which is async-signal-safe.
    unsafe {
        starting.pre_exec(move || {
            std::thread::sleep(CHILD_WAIT);
            Ok(())
        });
    }
    let child = std::thread::spawn(move || starting.status());
    std::thread::sleep(FORK_TIME);
    drop(held);
    let next = store.cache_volume("cargo", "linux/amd64", Trust::Main).unwrap();
    assert!(next.is_shared(), "the next action gets the shared volume");
    assert!(child.join().unwrap().unwrap().success());
}
