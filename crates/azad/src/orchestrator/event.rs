//! The events that the orchestrator takes, from GitHub's webhook payloads, and the trust level of each.

use serde::{Deserialize, Serialize};

use crate::frontend::{PULL_REQUEST_EVENT, PUSH_EVENT};
use crate::trust::Trust;

const BRANCH_PREFIX: &str = "refs/heads/";
/// The pull request actions that give it a commit to check.
const CHECKED_PULL_REQUEST_ACTIONS: [&str; 3] = ["opened", "synchronize", "reopened"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Push,
    PullRequest,
}

impl EventKind {
    /// The name of the event in GitHub, and in a goal's `on`.
    pub fn name(self) -> &'static str {
        match self {
            EventKind::Push => PUSH_EVENT,
            EventKind::PullRequest => PULL_REQUEST_EVENT,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repository {
    /// Such as `owner/name`.
    pub full_name: String,
    /// Where the orchestrator fetches the commit from.
    pub clone_url: String,
    pub default_branch: String,
}

/// A commit to check, and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub kind: EventKind,
    pub repository: Repository,
    /// The ref that holds the commit: `refs/heads/<branch>` or `refs/tags/<tag>` for a push, `refs/pull/<n>/head` for
    /// a pull request, as GitHub keeps it.
    pub git_ref: String,
    pub head_sha: String,
    pub pull_request: Option<u64>,
}

impl Event {
    /// The trust level of the event's runs, which comes from the event, never from the code that runs: a push to the
    /// default branch is `main`; a pull request, and a push to another branch, is `pull-request`; anything else, such
    /// as a tag that may not be on the default branch, is `none`.
    pub fn trust(&self) -> Trust {
        match (self.kind, self.git_ref.strip_prefix(BRANCH_PREFIX)) {
            (EventKind::Push, Some(branch)) if branch == self.repository.default_branch => Trust::Main,
            (EventKind::Push, Some(_)) | (EventKind::PullRequest, _) => Trust::PullRequest,
            (EventKind::Push, None) => Trust::None,
        }
    }

    /// The event of a webhook delivery, from GitHub's event name and payload; none for an event that gives no commit
    /// to check, such as `ping`, a deleted branch or a closed pull request.
    pub fn from_webhook(name: &str, payload: &[u8]) -> Result<Option<Event>, serde_json::Error> {
        match name {
            PUSH_EVENT => Ok(push(serde_json::from_slice(payload)?)),
            PULL_REQUEST_EVENT => Ok(pull_request(serde_json::from_slice(payload)?)),
            _ => Ok(None),
        }
    }

    /// The webhook payload of the event, in GitHub's format, with the fields that the orchestrator reads.
    pub fn to_webhook(&self) -> serde_json::Value {
        let repository = serde_json::json!({
            "full_name": self.repository.full_name,
            "clone_url": self.repository.clone_url,
            "default_branch": self.repository.default_branch,
        });
        match (self.kind, self.pull_request) {
            (EventKind::PullRequest, Some(number)) => serde_json::json!({
                "action": "synchronize",
                "number": number,
                "pull_request": { "head": { "sha": self.head_sha } },
                "repository": repository,
            }),
            _ => serde_json::json!({
                "ref": self.git_ref,
                "after": self.head_sha,
                "deleted": false,
                "repository": repository,
            }),
        }
    }
}

#[derive(Deserialize)]
struct PushPayload {
    #[serde(rename = "ref")]
    git_ref: String,
    after: String,
    #[serde(default)]
    deleted: bool,
    repository: Repository,
}

#[derive(Deserialize)]
struct PullRequestPayload {
    action: String,
    number: u64,
    pull_request: PullRequestFields,
    repository: Repository,
}

#[derive(Deserialize)]
struct PullRequestFields {
    head: Head,
}

#[derive(Deserialize)]
struct Head {
    sha: String,
}

fn push(payload: PushPayload) -> Option<Event> {
    if payload.deleted {
        return None;
    }
    Some(Event {
        kind: EventKind::Push,
        repository: payload.repository,
        git_ref: payload.git_ref,
        head_sha: payload.after,
        pull_request: None,
    })
}

fn pull_request(payload: PullRequestPayload) -> Option<Event> {
    if !CHECKED_PULL_REQUEST_ACTIONS.contains(&payload.action.as_str()) {
        return None;
    }
    Some(Event {
        kind: EventKind::PullRequest,
        repository: payload.repository,
        git_ref: pull_request_ref(payload.number),
        head_sha: payload.pull_request.head.sha,
        pull_request: Some(payload.number),
    })
}

/// Where GitHub keeps the head of a pull request.
pub(super) fn pull_request_ref(number: u64) -> String {
    format!("refs/pull/{number}/head")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: EventKind, git_ref: &str) -> Event {
        let repository = Repository {
            full_name: "owner/name".to_owned(),
            clone_url: "https://example.com/owner/name.git".to_owned(),
            default_branch: "main".to_owned(),
        };
        let pull_request = (kind == EventKind::PullRequest).then_some(7);
        Event { kind, repository, git_ref: git_ref.to_owned(), head_sha: "abc".to_owned(), pull_request }
    }

    #[test]
    fn the_trust_level_comes_from_the_event() {
        assert_eq!(event(EventKind::Push, "refs/heads/main").trust(), Trust::Main);
        assert_eq!(event(EventKind::Push, "refs/heads/feature").trust(), Trust::PullRequest);
        assert_eq!(event(EventKind::Push, "refs/heads/main-not").trust(), Trust::PullRequest);
        assert_eq!(event(EventKind::Push, "refs/tags/v1").trust(), Trust::None);
        assert_eq!(event(EventKind::PullRequest, "refs/pull/7/head").trust(), Trust::PullRequest);
    }

    #[test]
    fn an_event_reads_back_from_its_webhook_payload() {
        for original in [event(EventKind::Push, "refs/heads/main"), event(EventKind::PullRequest, "refs/pull/7/head")] {
            let payload = serde_json::to_vec(&original.to_webhook()).unwrap();
            assert_eq!(Event::from_webhook(original.kind.name(), &payload).unwrap(), Some(original));
        }
    }

    #[test]
    fn events_without_a_commit_to_check_are_ignored() {
        let closed = br#"{"action": "closed", "number": 7, "pull_request": {"head": {"sha": "abc"}},
            "repository": {"full_name": "o/n", "clone_url": "u", "default_branch": "main"}}"#;
        assert_eq!(Event::from_webhook(PULL_REQUEST_EVENT, closed).unwrap(), None);
        assert_eq!(Event::from_webhook("ping", b"{}").unwrap(), None);
    }
}
