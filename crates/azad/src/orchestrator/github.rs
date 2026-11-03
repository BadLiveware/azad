//! A GitHub App (decision 0032): the orchestrator signs in as the App with its private key, gets a short-lived token
//! for the installation of each repository, writes check runs through GitHub's REST API, and fetches private
//! repositories with the same token.

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ring::rand::SystemRandom;
use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
use serde_json::{Value, json};

use crate::orchestrator::checks::{CheckRuns, CheckStatus, Conclusion};

pub const GITHUB_API: &str = "https://api.github.com";
const ACCEPT: &str = "application/vnd.github+json";
const API_VERSION_HEADER: &str = "X-GitHub-Api-Version";
const API_VERSION: &str = "2022-11-28";
const USER_AGENT: &str = "azad-orchestrator";
const PEM_BEGIN_PKCS1: &str = "-----BEGIN RSA PRIVATE KEY-----";
const PEM_BEGIN_PKCS8: &str = "-----BEGIN PRIVATE KEY-----";
/// GitHub refuses an App's token that lives longer than ten minutes, or that starts in the future by its clock.
const APP_TOKEN_LIFETIME: Duration = Duration::from_secs(9 * 60);
const CLOCK_SKEW: Duration = Duration::from_secs(60);
/// An installation's token lives an hour; it is replaced before that, so that no call starts with a token about to end.
const TOKEN_MARGIN: Duration = Duration::from_secs(5 * 60);
/// What GitHub shows of a check run's summary; it refuses more than 65535 characters.
const SUMMARY_LIMIT: usize = 60_000;
const CHECK_TITLE: &str = "Azad";
/// The user name that git uses with an installation's token over HTTPS.
const GIT_TOKEN_USER: &str = "x-access-token";

#[derive(Debug, thiserror::Error)]
pub enum GitHubError {
    #[error("the GitHub App's private key: {0}")]
    Key(String),
    #[error("GitHub's API, {call}: {reason}")]
    Api { call: String, reason: String },
}

/// A token of an installation, and when it ends.
#[derive(Clone)]
struct Token {
    value: String,
    expires: SystemTime,
}

/// A GitHub App, through GitHub's REST API at `api`.
pub struct GitHubApp {
    app_id: String,
    key: RsaKeyPair,
    api: String,
    agent: ureq::Agent,
    /// The token of each repository's installation.
    tokens: Mutex<HashMap<String, Token>>,
}

impl fmt::Debug for GitHubApp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GitHubApp").field("app_id", &self.app_id).field("api", &self.api).finish_non_exhaustive()
    }
}

impl GitHubApp {
    /// The App `app_id`, with its private key in PEM, as GitHub gives it to download.
    pub fn new(app_id: &str, private_key_pem: &str, api: &str) -> Result<Self, GitHubError> {
        let agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
        Ok(GitHubApp {
            app_id: app_id.to_owned(),
            key: key_pair(private_key_pem)?,
            api: api.trim_end_matches('/').to_owned(),
            agent,
            tokens: Mutex::new(HashMap::new()),
        })
    }

    /// The token that signs the App in, a JSON Web Token (JWT) signed with RS256.
    fn app_token(&self) -> Result<String, GitHubError> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
        let issued = (now - CLOCK_SKEW).as_secs();
        let expires = (now + APP_TOKEN_LIFETIME).as_secs();
        let header = URL_SAFE_NO_PAD.encode(json!({ "alg": "RS256", "typ": "JWT" }).to_string());
        let claims = URL_SAFE_NO_PAD.encode(json!({ "iat": issued, "exp": expires, "iss": self.app_id }).to_string());
        let signed = format!("{header}.{claims}");
        let mut signature = vec![0; self.key.public().modulus_len()];
        self.key
            .sign(&RSA_PKCS1_SHA256, &SystemRandom::new(), signed.as_bytes(), &mut signature)
            .map_err(|_| GitHubError::Key("it cannot sign".to_owned()))?;
        Ok(format!("{signed}.{}", URL_SAFE_NO_PAD.encode(signature)))
    }

    /// The token of the App's installation on `repository` (`owner/name`), from the cache while it lives.
    pub fn token(&self, repository: &str) -> Result<String, GitHubError> {
        let cached = self.tokens.lock().ok().and_then(|tokens| tokens.get(repository).cloned());
        if let Some(token) = cached.filter(|token| SystemTime::now() + TOKEN_MARGIN < token.expires) {
            return Ok(token.value);
        }
        let app = self.app_token()?;
        let installation = self.call(Method::Get, &format!("/repos/{repository}/installation"), &app, None)?;
        let id = installation
            .get("id")
            .and_then(Value::as_u64)
            .ok_or_else(|| api_error("the installation", &installation))?;
        let answer = self.call(Method::Post, &format!("/app/installations/{id}/access_tokens"), &app, None)?;
        let value = answer.get("token").and_then(Value::as_str).ok_or_else(|| api_error("a token", &answer))?;
        let expires = answer
            .get("expires_at")
            .and_then(Value::as_str)
            .and_then(|text| text.parse::<jiff::Timestamp>().ok())
            .map(|time| UNIX_EPOCH + Duration::from_secs(time.as_second().max(0) as u64))
            .ok_or_else(|| api_error("the end of a token", &answer))?;
        let token = Token { value: value.to_owned(), expires };
        if let Ok(mut tokens) = self.tokens.lock() {
            tokens.insert(repository.to_owned(), token.clone());
        }
        Ok(token.value)
    }

    /// The HTTP header that lets git fetch `repository` with the installation's token.
    pub fn git_header(&self, repository: &str) -> Result<String, GitHubError> {
        let credentials = STANDARD.encode(format!("{GIT_TOKEN_USER}:{}", self.token(repository)?));
        Ok(format!("Authorization: Basic {credentials}"))
    }

    fn call(&self, method: Method, path: &str, token: &str, body: Option<&Value>) -> Result<Value, GitHubError> {
        let url = format!("{}{path}", self.api);
        let call = format!("{method:?} {path}");
        let failed = |reason: String| GitHubError::Api { call: call.clone(), reason };
        let body = body.map(Value::to_string).unwrap_or_default();
        let sent = match method {
            Method::Get => headers(self.agent.get(&url), token).call(),
            Method::Post => headers(self.agent.post(&url), token).send(body.as_bytes()),
            Method::Patch => headers(self.agent.patch(&url), token).send(body.as_bytes()),
        };
        let mut response = sent.map_err(|e| failed(e.to_string()))?;
        let status = response.status();
        let text = response.body_mut().read_to_string().map_err(|e| failed(e.to_string()))?;
        if !status.is_success() {
            return Err(failed(format!("{status}: {text}")));
        }
        serde_json::from_str(&text).map_err(|e| failed(format!("{e}: {text}")))
    }

    fn installation_call(&self, method: Method, repository: &str, path: &str, body: &Value) -> io::Result<Value> {
        let token = self.token(repository).map_err(io::Error::other)?;
        self.call(method, &format!("/repos/{repository}{path}"), &token, Some(body)).map_err(io::Error::other)
    }
}

#[derive(Clone, Copy, Debug)]
enum Method {
    Get,
    Post,
    Patch,
}

fn headers<B>(request: ureq::RequestBuilder<B>, token: &str) -> ureq::RequestBuilder<B> {
    request
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", ACCEPT)
        .header(API_VERSION_HEADER, API_VERSION)
        .header("User-Agent", USER_AGENT)
        .header("Content-Type", "application/json")
}

impl CheckRuns for Arc<GitHubApp> {
    fn create(&self, repository: &str, head_sha: &str, name: &str) -> io::Result<u64> {
        let body = json!({ "name": name, "head_sha": head_sha, "status": "queued" });
        let answer = self.installation_call(Method::Post, repository, "/check-runs", &body)?;
        answer.get("id").and_then(Value::as_u64).ok_or_else(|| io::Error::other(api_error("a check run", &answer)))
    }

    fn update(
        &self,
        repository: &str,
        id: u64,
        status: CheckStatus,
        conclusion: Option<Conclusion>,
        summary: &str,
    ) -> io::Result<()> {
        let mut body = json!({ "status": status });
        if let Some(conclusion) = conclusion {
            body["conclusion"] = json!(conclusion);
        }
        if !summary.is_empty() {
            body["output"] = json!({ "title": CHECK_TITLE, "summary": shortened(summary) });
        }
        self.installation_call(Method::Patch, repository, &format!("/check-runs/{id}"), &body).map(|_| ())
    }
}

fn api_error(what: &str, answer: &Value) -> GitHubError {
    GitHubError::Api { call: what.to_owned(), reason: format!("no such field in {answer}") }
}

/// The summary, at most as long as GitHub shows, cut at a character.
fn shortened(summary: &str) -> &str {
    match summary.char_indices().nth(SUMMARY_LIMIT) {
        Some((end, _)) => &summary[..end],
        None => summary,
    }
}

/// The key pair of a PEM file: PKCS #1, as GitHub gives an App's key, or PKCS #8.
fn key_pair(pem: &str) -> Result<RsaKeyPair, GitHubError> {
    let pem = pem.trim();
    let body: String = pem.lines().filter(|line| !line.starts_with("-----")).collect();
    let der = STANDARD.decode(body).map_err(|e| GitHubError::Key(e.to_string()))?;
    let pair = match pem.lines().next() {
        Some(PEM_BEGIN_PKCS1) => RsaKeyPair::from_der(&der),
        Some(PEM_BEGIN_PKCS8) => RsaKeyPair::from_pkcs8(&der),
        _ => return Err(GitHubError::Key("it is not an RSA private key in PEM".to_owned())),
    };
    pair.map_err(|e| GitHubError::Key(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_summary_is_cut_at_a_character_below_github_s_limit() {
        let long = "é".repeat(SUMMARY_LIMIT + 10);
        assert_eq!(shortened(&long).chars().count(), SUMMARY_LIMIT);
        assert_eq!(shortened("short"), "short");
    }
}
