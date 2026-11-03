//! Events from a local git repository, sent to an orchestrator as GitHub would send them: signed, in GitHub's format,
//! with the repository's path as its clone URL. For a pull request, the simulator keeps its head at
//! `refs/pull/<n>/head`, where GitHub keeps it.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::orchestrator::event::{Event, EventKind, Repository, pull_request_ref};
use crate::orchestrator::signature::{SIGNATURE_HEADER, sign};

const GIT: &str = "git";
const EVENT_HEADER: &str = "X-GitHub-Event";
const DELIVERY_HEADER: &str = "X-GitHub-Delivery";
const LOCAL_OWNER: &str = "local";
const DELIVERY_BYTES: usize = 16;

#[derive(Debug, thiserror::Error)]
pub enum SimulateError {
    #[error("git {command}: {reason}")]
    Git { command: String, reason: String },
    #[error("sending the event to {url}: {reason}")]
    Send { url: String, reason: String },
    #[error("the orchestrator refused the event: {status} {text}")]
    Refused { status: u16, text: String },
}

#[derive(Debug)]
pub struct Simulator {
    repository: PathBuf,
    url: String,
    secret: Vec<u8>,
    default_branch: String,
}

impl Simulator {
    /// A simulator of the repository at `repository`, whose default branch is `default_branch`, that sends events to
    /// the orchestrator at `url` with the webhook secret `secret`.
    pub fn new(repository: &Path, url: &str, secret: &[u8], default_branch: &str) -> Result<Self, SimulateError> {
        let repository = std::fs::canonicalize(repository)
            .map_err(|e| SimulateError::Git { command: "rev-parse".to_owned(), reason: e.to_string() })?;
        Ok(Simulator {
            repository,
            url: url.to_owned(),
            secret: secret.to_vec(),
            default_branch: default_branch.to_owned(),
        })
    }

    /// A push of `branch`, at the commit that it holds now. Returns the orchestrator's answer.
    pub fn push(&self, branch: &str) -> Result<String, SimulateError> {
        let git_ref = format!("refs/heads/{branch}");
        let head_sha = self.git(&["rev-parse", "--verify", &format!("{git_ref}^{{commit}}")])?;
        self.send(&Event {
            kind: EventKind::Push,
            repository: self.repository(),
            git_ref,
            head_sha,
            pull_request: None,
        })
    }

    /// A push of the tag `tag`, at the commit that it names.
    pub fn tag(&self, tag: &str) -> Result<String, SimulateError> {
        let git_ref = format!("refs/tags/{tag}");
        let head_sha = self.git(&["rev-parse", "--verify", &format!("{git_ref}^{{commit}}")])?;
        self.send(&Event {
            kind: EventKind::Push,
            repository: self.repository(),
            git_ref,
            head_sha,
            pull_request: None,
        })
    }

    /// Pull request `number` from `branch`, at the commit that the branch holds now: when it is opened, and when it
    /// gets a new commit.
    pub fn pull_request(&self, number: u64, branch: &str) -> Result<String, SimulateError> {
        let head_sha = self.git(&["rev-parse", "--verify", &format!("refs/heads/{branch}^{{commit}}")])?;
        let git_ref = pull_request_ref(number);
        self.git(&["update-ref", &git_ref, &head_sha])?;
        self.send(&Event {
            kind: EventKind::PullRequest,
            repository: self.repository(),
            git_ref,
            head_sha,
            pull_request: Some(number),
        })
    }

    fn repository(&self) -> Repository {
        let name = self.repository.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        Repository {
            full_name: format!("{LOCAL_OWNER}/{name}"),
            clone_url: self.repository.display().to_string(),
            default_branch: self.default_branch.clone(),
        }
    }

    fn send(&self, event: &Event) -> Result<String, SimulateError> {
        let payload = serde_json::to_vec(&event.to_webhook()).expect("a JSON value serializes");
        let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
        let send_error = |reason: String| SimulateError::Send { url: self.url.clone(), reason };
        let mut response = agent
            .post(&self.url)
            .header(EVENT_HEADER, event.kind.name())
            .header(DELIVERY_HEADER, delivery_id().map_err(send_error)?)
            .header(SIGNATURE_HEADER, sign(&self.secret, &payload))
            .content_type("application/json")
            .send(&payload[..])
            .map_err(|e| send_error(e.to_string()))?;
        let status = response.status().as_u16();
        let text = response.body_mut().read_to_string().map_err(|e| send_error(e.to_string()))?.trim().to_owned();
        match response.status().is_success() {
            true => Ok(text),
            false => Err(SimulateError::Refused { status, text }),
        }
    }

    fn git(&self, args: &[&str]) -> Result<String, SimulateError> {
        let failed = |reason: String| SimulateError::Git { command: args[0].to_owned(), reason };
        let output =
            Command::new(GIT).arg("-C").arg(&self.repository).args(args).output().map_err(|e| failed(e.to_string()))?;
        match output.status.success() {
            true => Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned()),
            false => Err(failed(String::from_utf8_lossy(&output.stderr).trim().to_owned())),
        }
    }
}

/// A new ID for a delivery, as GitHub gives each.
fn delivery_id() -> Result<String, String> {
    let mut bytes = [0u8; DELIVERY_BYTES];
    getrandom::fill(&mut bytes).map_err(|e| format!("no random bytes: {e}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
