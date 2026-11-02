//! GitHub's GraphQL API as a source of history: the commit count (`history.totalCount`) and chains of first parents,
//! for a shallow clone, as a GitHub Actions checkout is.

use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

use crate::history::{CommitFacts, History, HistoryError};

/// GitHub rejects a query deeper than 25 levels; each first parent adds two.
const COMMITS_PER_READ: usize = 10;
/// More parents than an octopus merge has in practice; GitHub gives at most 100 in one list.
const PARENTS_PER_COMMIT: usize = 100;
const ATTEMPTS: u32 = 3;
const RETRY_WAIT: Duration = Duration::from_secs(5);

/// GitHub's GraphQL API for one repository.
#[derive(Debug)]
pub struct GitHubHistory {
    agent: ureq::Agent,
    url: String,
    owner: String,
    name: String,
    token: String,
}

/// The query of `commits` commits of the first-parent chain of `$oid`, nested, with each commit's parents.
fn chain_query(commits: usize) -> String {
    let fields = format!("oid committedDate parentIds: parents(first: {PARENTS_PER_COMMIT}) {{ nodes {{ oid }} }}");
    let mut chain = fields.clone();
    for _ in 1..commits {
        chain = format!("{fields} next: parents(first: 1) {{ nodes {{ {chain} }} }}");
    }
    format!(
        "query($owner: String!, $name: String!, $oid: GitObjectID!) {{ repository(owner: $owner, name: $name) {{ \
         object(oid: $oid) {{ ... on Commit {{ {chain} }} }} }} }}"
    )
}

const COUNT_QUERY: &str = "query($owner: String!, $name: String!, $oid: GitObjectID!) { repository(owner: $owner, \
                           name: $name) { object(oid: $oid) { ... on Commit { history(first: 0) { totalCount } } } } }";

fn error(message: String) -> HistoryError {
    HistoryError::Read(format!("GitHub's API: {message}"))
}

/// A failure that a later attempt can avoid: the network, or GitHub's own errors.
enum Failure {
    Transient(String),
    Final(String),
}

impl GitHubHistory {
    /// The repository `owner/name`, through the GraphQL endpoint `url`, such as `https://api.github.com/graphql`.
    pub fn new(url: &str, repository: &str, token: &str) -> Result<Self, HistoryError> {
        let Some((owner, name)) = repository.split_once('/') else {
            return Err(error(format!("{repository:?} is not a repository such as owner/name")));
        };
        let agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
        Ok(GitHubHistory {
            agent,
            url: url.to_owned(),
            owner: owner.to_owned(),
            name: name.to_owned(),
            token: token.to_owned(),
        })
    }

    /// The commit object of the answer to `query` about `id`.
    fn commit(&self, query: &str, id: &str) -> Result<Value, HistoryError> {
        let body = json!({ "query": query, "variables": { "owner": self.owner, "name": self.name, "oid": id } });
        let mut attempt = 1;
        loop {
            match self.post(&body) {
                Ok(answer) => return self.commit_of(answer, id),
                Err(Failure::Transient(_)) if attempt < ATTEMPTS => {
                    attempt += 1;
                    thread::sleep(RETRY_WAIT);
                }
                Err(Failure::Transient(message) | Failure::Final(message)) => return Err(error(message)),
            }
        }
    }

    fn post(&self, body: &Value) -> Result<Value, Failure> {
        let body = body.to_string();
        let mut response = self
            .agent
            .post(&self.url)
            .header("Authorization", format!("bearer {}", self.token))
            .header("Content-Type", "application/json")
            .header("User-Agent", "azad")
            .send(body.as_bytes())
            .map_err(|e| Failure::Transient(e.to_string()))?;
        let status = response.status();
        let text = response.body_mut().read_to_string().map_err(|e| Failure::Transient(e.to_string()))?;
        match (status.is_success(), status.is_server_error()) {
            (true, _) => serde_json::from_str(&text).map_err(|e| Failure::Final(format!("{e}: {text}"))),
            (false, true) => Err(Failure::Transient(format!("{status}: {text}"))),
            (false, false) => Err(Failure::Final(format!("{status}: {text}"))),
        }
    }

    fn commit_of(&self, answer: Value, id: &str) -> Result<Value, HistoryError> {
        if let Some(errors) = answer.get("errors") {
            return Err(error(format!("{id}: {errors}")));
        }
        match answer.pointer("/data/repository/object") {
            Some(commit) if !commit.is_null() => Ok(commit.clone()),
            _ => Err(error(format!("{id} is not a commit of {}/{}", self.owner, self.name))),
        }
    }
}

fn facts(commit: &Value) -> Result<CommitFacts, HistoryError> {
    let field =
        |name: &str| commit.get(name).and_then(Value::as_str).ok_or_else(|| error(format!("no {name} in {commit}")));
    let date = field("committedDate")?;
    let time = date.parse::<jiff::Timestamp>().map_err(|e| error(format!("the date {date:?}: {e}")))?.as_second();
    let parents = commit
        .pointer("/parentIds/nodes")
        .and_then(Value::as_array)
        .map(|nodes| nodes.iter().filter_map(|node| node.get("oid")?.as_str().map(str::to_owned)).collect())
        .unwrap_or_default();
    Ok(CommitFacts { id: field("oid")?.to_owned(), time, parents })
}

impl History for GitHubHistory {
    fn name(&self) -> String {
        format!("GitHub's API ({}/{})", self.owner, self.name)
    }

    fn count(&self, id: &str) -> Result<u64, HistoryError> {
        let commit = self.commit(COUNT_QUERY, id)?;
        commit
            .pointer("/history/totalCount")
            .and_then(Value::as_u64)
            .ok_or_else(|| error(format!("no commit count for {id}")))
    }

    fn first_parents(&self, id: &str, limit: usize) -> Result<Vec<CommitFacts>, HistoryError> {
        let mut chain: Vec<CommitFacts> = Vec::new();
        let mut next = id.to_owned();
        while chain.len() < limit {
            let read = (limit - chain.len()).min(COMMITS_PER_READ);
            let mut commit = self.commit(&chain_query(read), &next)?;
            for _ in 0..read {
                chain.push(facts(&commit)?);
                match commit.pointer("/next/nodes/0") {
                    Some(parent) => commit = parent.clone(),
                    None => break,
                }
            }
            let last = chain.last().expect("a read gives at least one commit");
            match last.parents.first() {
                Some(parent) if chain.len() < limit => next = parent.clone(),
                _ => break,
            }
        }
        chain.truncate(limit);
        Ok(chain)
    }
}
