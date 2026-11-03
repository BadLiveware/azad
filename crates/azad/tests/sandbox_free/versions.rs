//! `git` and `semver` in evaluation, and `@std//version.star` (decision 0021), on repositories that git makes.
#![allow(clippy::unwrap_used)]

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::thread;

use azad::frontend::starlark::{Context, evaluate, evaluate_in};
use azad::frontend::{NoteLevel, Package};
use azad::history::GitHubHistory;
use serde_json::{Value, json};
use tempfile::TempDir;

const PLATFORM: &str = "linux/amd64";
const BASE: &str = "registry.example/base@sha256:1111111111111111111111111111111111111111111111111111111111111111";

fn git(dir: &Path, args: &[&str], date: &str) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=Azad", "-c", "user.email=azad@example.com", "-c", "commit.gpgsign=false"])
        .args(["-c", "tag.gpgsign=false", "-c", "init.defaultBranch=main"])
        .args(args)
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn commit(dir: &Path, message: &str, date: &str) -> String {
    git(dir, &["commit", "--quiet", "--allow-empty", "-m", message], date);
    git(dir, &["rev-parse", "HEAD"], date)
}

/// The values of `expressions`, by name, as `tasks.star` evaluates them in the repository at `dir`, with the package.
fn values(dir: &Path, expressions: &[(&str, &str)]) -> (Vec<(String, String)>, Package) {
    values_in(&Context::default(), dir, expressions)
}

fn values_in(context: &Context, dir: &Path, expressions: &[(&str, &str)]) -> (Vec<(String, String)>, Package) {
    let env: Vec<String> = expressions.iter().map(|(name, value)| format!("{name:?}: str({value})")).collect();
    let tasks = format!(
        "load(\"@std//version.star\", \"commit_calver\", \"conventional_commit\", \"latest_release\", \"next_release\")\n\
         action(name = \"values\", cmd = [\"true\"], root = image({BASE:?}), env = {{{}}})\n",
        env.join(", ")
    );
    fs::write(dir.join("tasks.star"), tasks).unwrap();
    let package = evaluate_in(context, dir, "", PLATFORM).unwrap();
    let env = package.actions["//:values"].env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    (env, package)
}

fn value(dir: &Path, expression: &str) -> String {
    values(dir, &[("value", expression)]).0.remove(0).1
}

fn error(dir: &Path, expression: &str) -> String {
    let tasks = format!(
        "load(\"@std//version.star\", \"commit_calver\", \"next_release\")\n\
         action(name = \"values\", cmd = [\"true\"], root = image({BASE:?}), env = {{\"v\": str({expression})}})\n"
    );
    fs::write(dir.join("tasks.star"), tasks).unwrap();
    evaluate(dir, "", PLATFORM).unwrap_err().to_string()
}

fn repository() -> TempDir {
    let dir = TempDir::new().unwrap();
    git(dir.path(), &["init", "--quiet"], "2026-01-01T00:00:00Z");
    dir
}

#[test]
fn the_count_is_every_commit_that_the_commit_contains_as_git_counts_it() {
    let repo = repository();
    let dir = repo.path();
    let a = commit(dir, "a", "2026-10-01T10:00:00Z");
    let b = commit(dir, "b", "2026-10-02T10:00:00Z");
    git(dir, &["checkout", "--quiet", "-b", "side", &a], "2026-10-02T10:00:00Z");
    commit(dir, "c", "2026-10-03T10:00:00Z");
    commit(dir, "d", "2026-10-04T10:00:00Z");
    git(dir, &["checkout", "--quiet", "main"], "2026-10-05T10:00:00Z");
    git(dir, &["merge", "--quiet", "--no-ff", "-m", "merge", "side"], "2026-10-05T10:00:00Z");
    let merge = git(dir, &["rev-parse", "HEAD"], "");

    let counted = git(dir, &["rev-list", "--count", "HEAD"], "");
    assert_eq!(counted, "5");
    assert_eq!(value(dir, "git.commit().count()"), counted);
    assert_eq!(value(dir, &format!("git.commit({b:?}).count()")), "2");
    let chain = value(dir, "[c.id for c in git.commit().first_parents(10)]");
    assert_eq!(chain, format!("[{merge:?}, {b:?}, {a:?}]"));
}

#[test]
fn the_calver_takes_its_month_from_the_latest_date_among_the_first_parents() {
    let repo = repository();
    let dir = repo.path();
    commit(dir, "before", "2026-10-30T10:00:00Z");
    commit(dir, "in november", "2026-11-01T09:00:00Z");
    commit(dir, "pushed with an older date", "2026-10-31T23:00:00Z");

    assert_eq!(value(dir, "git.commit().date"), "2026-10-31");
    assert_eq!(value(dir, "commit_calver()"), "2026.11.3");
    assert_eq!(value(dir, "commit_calver(window = 1)"), "2026.10.3");
    assert_eq!(value(dir, "commit_calver(git.commit(\"HEAD~2\"))"), "2026.10.1");
}

#[test]
fn each_ref_that_evaluation_resolves_is_recorded_and_a_moved_tag_shows() {
    let repo = repository();
    let dir = repo.path();
    let first = commit(dir, "first", "2026-10-01T10:00:00Z");
    git(dir, &["tag", "v1.0.0"], "");
    git(dir, &["tag", "-a", "-m", "annotated", "v1.2.0"], "");
    let second = commit(dir, "second", "2026-10-02T10:00:00Z");
    git(dir, &["tag", "v2.0.0-rc.1"], "");
    git(dir, &["tag", "vnext"], "");

    let (found, package) = values(dir, &[("latest", "latest_release()[1]"), ("head", "git.commit().id")]);
    assert_eq!(found, vec![("head".to_owned(), second.clone()), ("latest".to_owned(), "v1.2.0".to_owned())]);
    assert_eq!(package.git_refs["HEAD"], second);
    assert_eq!(package.git_refs["refs/tags/v1.0.0"], first);
    assert_eq!(package.git_refs["refs/tags/v1.2.0"], first, "an annotated tag names the commit it points to");
    assert!(!package.git_refs.contains_key(&first), "a commit ID is fixed, so it is not recorded");

    git(dir, &["tag", "--force", "v1.0.0", &second], "");
    let (_, moved) = values(dir, &[("latest", "latest_release()[1]")]);
    assert_eq!(moved.git_refs["refs/tags/v1.0.0"], second);
    assert_eq!(value(dir, &format!("git.commit({first:?}).id")), first);
}

#[test]
fn a_shallow_clone_gives_the_facts_of_its_commits_and_refuses_the_history_it_lacks() {
    let repo = repository();
    let dir = repo.path();
    commit(dir, "first", "2026-10-01T10:00:00Z");
    commit(dir, "second", "2026-10-02T10:00:00Z");
    let head = commit(dir, "third", "2026-10-03T10:00:00Z");
    let clone = TempDir::new().unwrap();
    let url = format!("file://{}", dir.display());
    git(clone.path(), &["clone", "--quiet", "--depth", "2", &url, "."], "");
    let shallow = clone.path();

    assert_eq!(value(shallow, "git.commit().id"), head);
    assert_eq!(value(shallow, "git.commit().date"), "2026-10-03");
    assert_eq!(value(shallow, "len(git.commit().first_parents(2))"), "2");
    assert!(error(shallow, "git.commit().count()").contains("shallow"));
    assert!(error(shallow, "git.commit().first_parents(3)").contains("shallow"));
    assert!(error(shallow, "commit_calver()").contains("shallow"));
}

#[test]
fn semantic_versions_compare_by_precedence_and_meet_cargo_requirements() {
    let repo = repository();
    let dir = repo.path();
    let sorted =
        "[str(v) for v in sorted([semver.parse(t) for t in [\"1.10.0\", \"1.4.0\", \"1.4.0-rc.1\", \"0.9.9\"]])]";
    assert_eq!(value(dir, sorted), "[\"0.9.9\", \"1.4.0-rc.1\", \"1.4.0\", \"1.10.0\"]");
    assert_eq!(value(dir, "semver.parse(\"1.4.2\") == semver.parse(\"1.4.2\")"), "True");
    assert_eq!(value(dir, "len(dict([(semver.parse(\"1.4.2\"), 1), (semver.parse(\"1.4.2\"), 2)]))"), "1");
    assert_eq!(value(dir, "semver.matches(\"^1.4\", \"1.9.0\")"), "True");
    assert_eq!(value(dir, "semver.matches(\"^1.4\", semver.parse(\"2.0.0\"))"), "False");
    assert_eq!(value(dir, "semver.matches(\"^1.4\", \"1.5.0-rc.1\")"), "False");
    assert_eq!(value(dir, "semver.parse(\"1.3.0-rc.1\").bump(\"minor\")"), "1.3.0");
    assert_eq!(value(dir, "semver.valid(\"01.2.3\")"), "False");
    assert!(error(dir, "semver.parse(\"1.2\")").contains("not a semantic version"));
    assert!(error(dir, "semver.parse(\"1.2.3\") < \"1.2.4\"").contains("compares only with a semver"));
}

const TOKEN: &str = "a token";

/// Answers the GraphQL queries of `GitHubHistory` from the full repository at `full`, as GitHub would, when the request
/// has the token. It stands in for GitHub, which a test cannot reach.
fn github(full: PathBuf) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/graphql", listener.local_addr().unwrap());
    thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let (mut length, mut authorized) = (0, false);
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let line = line.trim_end().to_ascii_lowercase();
                if line.is_empty() {
                    break;
                }
                if let Some(value) = line.strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
                authorized |= line == format!("authorization: bearer {TOKEN}");
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let request: Value = serde_json::from_slice(&body).unwrap();
            let (status, answer) = match authorized {
                true => ("200 OK", answer(&full, &request)),
                false => ("401 Unauthorized", json!({ "message": "Bad credentials" })),
            };
            let text = answer.to_string();
            let response =
                format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}", text.len());
            stream.write_all(response.as_bytes()).unwrap();
        }
    });
    url
}

fn answer(full: &Path, request: &Value) -> Value {
    let query = request["query"].as_str().unwrap();
    let oid = request["variables"]["oid"].as_str().unwrap();
    if query.contains("totalCount") {
        let count: u64 = git(full, &["rev-list", "--count", oid], "").parse().unwrap();
        return json!({ "data": { "repository": { "object": { "history": { "totalCount": count } } } } });
    }
    let depth = query.matches("committedDate").count().to_string();
    let log = git(full, &["log", "--first-parent", "-n", &depth, "--format=%H %cI %P", oid], "");
    let mut chain: Option<Value> = None;
    for line in log.lines().rev() {
        let mut fields = line.split(' ');
        let (id, date) = (fields.next().unwrap(), fields.next().unwrap());
        let parents: Vec<Value> = fields.map(|parent| json!({ "oid": parent })).collect();
        let message = git(full, &["log", "-1", "--format=%B", id], "");
        let mut commit =
            json!({ "oid": id, "committedDate": date, "message": message, "parentIds": { "nodes": parents } });
        if let Some(next) = chain.take() {
            commit["next"] = json!({ "nodes": [next] });
        }
        chain = Some(commit);
    }
    json!({ "data": { "repository": { "object": chain.unwrap() } } })
}

#[test]
fn a_shallow_clone_takes_the_history_it_lacks_from_github_and_says_so() {
    let repo = repository();
    let dir = repo.path();
    for day in 1..=30 {
        commit(dir, &format!("day {day}"), &format!("2026-10-{day:02}T10:00:00Z"));
    }
    commit(dir, "in november", "2026-11-01T09:00:00Z");
    commit(dir, "pushed with an older date", "2026-10-31T23:00:00Z");
    let clone = TempDir::new().unwrap();
    let url = format!("file://{}", dir.display());
    git(clone.path(), &["clone", "--quiet", "--depth", "1", &url, "."], "");
    let shallow = clone.path();
    let github = github(dir.to_path_buf());

    let history = GitHubHistory::new(&github, "owner/name", TOKEN).unwrap();
    let context = Context { history: Some(Arc::new(history)) };
    let expressions = [("calver", "commit_calver()"), ("chain", "len(git.commit().first_parents(25))")];
    let (found, package) = values_in(&context, shallow, &expressions);
    assert_eq!(found, vec![("calver".to_owned(), "2026.11.32".to_owned()), ("chain".to_owned(), "25".to_owned())]);
    assert_eq!(value(dir, "commit_calver()"), "2026.11.32", "the full clone gives the same version by itself");
    let decisions: Vec<&str> =
        package.notes.iter().filter(|note| note.level == NoteLevel::Normal).map(|note| note.text.as_str()).collect();
    assert!(decisions.iter().any(|text| text.contains("commit count") && text.contains("GitHub's API (owner/name)")));
    assert!(decisions.iter().any(|text| text.contains("24 of the first parents") && text.contains("ends after 1")));

    let wrong = GitHubHistory::new(&github, "owner/name", "another token").unwrap();
    let context = Context { history: Some(Arc::new(wrong)) };
    fs::write(shallow.join("tasks.star"), "x = git.commit().first_parents(3)\n").unwrap();
    let refused = evaluate_in(&context, shallow, "", PLATFORM).unwrap_err().to_string();
    assert!(refused.contains("401"), "{refused}");
}

/// `<major>.<yyyymmdd>.<n>` from Conventional Commits: a release only when a commit since the last one is a feature, a
/// fix or breaking; `n` counts the releases of a day; a breaking change raises `major`.
#[test]
fn the_next_release_follows_the_conventional_commits_since_the_latest_one() {
    let repo = repository();
    let dir = repo.path();
    let tag = |name: &str| git(dir, &["tag", name], "");
    commit(dir, "chore: start", "2026-11-01T08:00:00Z");
    commit(dir, "Not a conventional commit", "2026-11-01T08:30:00Z");
    assert_eq!(value(dir, "next_release()"), "None", "no feature, no fix, nothing breaking");
    commit(dir, "feat(cli): a command", "2026-11-01T09:00:00Z");
    assert_eq!(value(dir, "next_release()"), "0.20261101.0", "the first release");
    tag("v0.20261101.0");
    assert_eq!(value(dir, "next_release()"), "None", "nothing since the release");
    commit(dir, "docs: words", "2026-11-01T10:00:00Z");
    assert_eq!(value(dir, "next_release()"), "None");
    commit(dir, "fix: a bug", "2026-11-01T11:00:00Z");
    assert_eq!(value(dir, "next_release()"), "0.20261101.1", "the second release of the day");
    tag("v0.20261101.1");
    commit(dir, "fix: another", "2026-11-02T09:00:00Z");
    assert_eq!(value(dir, "next_release()"), "0.20261102.0", "a new day starts at 0");
    commit(dir, "refactor: the store\n\nBREAKING CHANGE: the old layout is gone", "2026-11-02T10:00:00Z");
    assert_eq!(value(dir, "next_release()"), "1.20261102.0", "a breaking footer raises the major");
    tag("v1.20261102.0");
    commit(dir, "feat!: a new language", "2026-11-02T11:00:00Z");
    assert_eq!(value(dir, "next_release()"), "2.20261102.0", "so does a header with !");
    assert_eq!(value(dir, "conventional_commit(\"feat(cli)!: x\")"), "(\"feat\", True)");
    assert_eq!(value(dir, "conventional_commit(\"Feat: x\")"), "(None, False)");
    assert_eq!(value(dir, "conventional_commit(\"fix:\")"), "(None, False)");

    git(dir, &["checkout", "--quiet", "--orphan", "other"], "");
    commit(dir, "feat: elsewhere", "2026-11-03T09:00:00Z");
    assert!(error(dir, "next_release()").contains("v1.20261102.0 is not among the 1000 first parents"));
}

/// The walk to the latest release reads more first parents at each step, so a release more commits back than the
/// first step still bounds the commits that count.
#[test]
fn the_next_release_finds_a_release_beyond_the_first_step_of_the_walk() {
    let repo = repository();
    let dir = repo.path();
    commit(dir, "feat!: before the release", "2026-11-01T08:00:00Z");
    git(dir, &["tag", "v0.20261101.0"], "");
    for minute in 0..30 {
        commit(dir, &format!("docs: page {minute}"), &format!("2026-11-02T09:{minute:02}:00Z"));
    }
    assert_eq!(value(dir, "next_release()"), "None", "only docs since the release, 30 commits back");
    commit(dir, "fix: a bug", "2026-11-02T10:00:00Z");
    assert_eq!(value(dir, "next_release()"), "0.20261102.0", "the breaking change before the release does not count");
}
