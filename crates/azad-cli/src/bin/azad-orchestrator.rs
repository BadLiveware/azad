//! `azad-orchestrator`: Azad's orchestrator (decision 0020). It takes GitHub's webhooks, runs the goals that name the
//! event at the event's trust level, and writes check runs; `simulate` sends it events from a local repository.

use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};
use azad::EngineConfig;
use azad::orchestrator::{self, GITHUB_API, GitHubApp, Orchestrator, Simulator};
use clap::{Parser, Subcommand};

const DEFAULT_LISTEN: &str = "127.0.0.1:8484";
const DEFAULT_WEBHOOK_URL: &str = "http://127.0.0.1:8484/webhook";
const DEFAULT_BRANCH: &str = "main";
const WEBHOOK_SECRET_ENV: &str = "AZAD_WEBHOOK_SECRET";
const CHECKED_OUT: &str = "HEAD";

#[derive(Debug, Parser)]
#[command(name = "azad-orchestrator", version = azad::VERSION, about = "Azad's orchestrator: runs goals for GitHub's events")]
struct Cli {
    /// The store directory. The default is $AZAD_STORE, or ~/.cache/azad.
    #[arg(long, global = true)]
    store: Option<PathBuf>,
    /// The workspace root, whose git repository `simulate` sends events from. The default is the nearest directory
    /// above the current one that holds .git, or the current directory.
    #[arg(long, global = true)]
    root: Option<PathBuf>,
    /// Run actions through the executor service on this Unix socket (`azad-executor`), instead of in this process.
    #[arg(long, global = true)]
    executor: Option<PathBuf>,
    #[command(subcommand)]
    command: OrchestratorCommand,
}

#[derive(Debug, Subcommand)]
enum OrchestratorCommand {
    /// Take GitHub's webhooks for pushes and pull requests on /webhook, run the goals whose `on` names the event, in
    /// their images and at the event's trust level, and write check runs, which `checks` shows. Deliveries must be
    /// signed with the secret in $AZAD_WEBHOOK_SECRET.
    Serve {
        /// The address on which to listen.
        #[arg(long, default_value = DEFAULT_LISTEN)]
        listen: String,
        /// The client ID or the ID of a GitHub App, whose installations get check runs on GitHub, and whose private repositories are
        /// fetched with its token (decision 0032). Without it, check runs go to the fake GitHub in the store.
        #[arg(long, requires = "github_app_key")]
        github_app_id: Option<String>,
        /// The file of the GitHub App's private key, in PEM, as GitHub gives it to download.
        #[arg(long, requires = "github_app_id")]
        github_app_key: Option<PathBuf>,
        /// GitHub's REST API.
        #[arg(long, default_value = GITHUB_API)]
        github_api: String,
        /// Where `azad-site` shows the runs, such as http://127.0.0.1:8485, so that each check run links to its run's
        /// page (decision 0034).
        #[arg(long)]
        site_url: Option<String>,
        /// How many actions run at the same time, for all runs together.
        #[arg(long, default_value_t = orchestrator::DEFAULT_PARALLELISM)]
        parallelism: usize,
    },
    /// Send an event from the workspace's git repository to `serve`, as GitHub would, signed with the secret in
    /// $AZAD_WEBHOOK_SECRET.
    Simulate {
        #[command(subcommand)]
        event: SimulatedEvent,
        /// Where `serve` takes deliveries.
        #[arg(long, default_value = DEFAULT_WEBHOOK_URL)]
        server: String,
        /// The default branch of the repository, whose pushes run at the trust level main.
        #[arg(long, default_value = DEFAULT_BRANCH)]
        default_branch: String,
    },
    /// List the check runs that `serve` wrote, oldest first.
    Checks,
}

#[derive(Debug, Subcommand)]
enum SimulatedEvent {
    /// A push of a branch, at the commit that it holds now. The default is the branch that is checked out.
    Push { branch: Option<String> },
    /// A push of a tag. It runs at the trust level main when the default branch holds its commit, and else at none.
    Tag { tag: String },
    /// A pull request from a branch, when it is opened or gets a new commit. The default is the branch that is checked
    /// out.
    PullRequest { number: u64, branch: Option<String> },
    /// A request to run a check run again, by its ID, as `checks` shows it: "Re-run" on GitHub.
    Rerun { check: u64 },
    /// A request to run the check runs of a commit again: "Re-run all checks" on GitHub. The default is the commit
    /// that is checked out.
    RerunAll { commit: Option<String> },
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("azad-orchestrator: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    let mut config = EngineConfig::from_environment(cli.store, cli.root)?;
    config.executor = cli.executor;
    match cli.command {
        OrchestratorCommand::Serve { listen, github_app_id, github_app_key, github_api, site_url, parallelism } => {
            let app = match (github_app_id, github_app_key) {
                (Some(id), Some(key)) => Some(github_app(&id, &key, &github_api)?),
                _ => None,
            };
            serve(config, &listen, app, site_url.as_deref(), parallelism)
        }
        OrchestratorCommand::Simulate { event: SimulatedEvent::Rerun { check }, server, default_branch } => {
            let answer = simulator(&config, &server, &default_branch)?.rerun(check)?;
            eprintln!("azad-orchestrator: asked to run check run {check} again: {answer}");
            Ok(())
        }
        OrchestratorCommand::Simulate { event: SimulatedEvent::RerunAll { commit }, server, default_branch } => {
            let commit = commit.unwrap_or_else(|| CHECKED_OUT.to_owned());
            let answer = simulator(&config, &server, &default_branch)?.rerun_all(&commit)?;
            eprintln!("azad-orchestrator: asked to run the check runs of {commit} again: {answer}");
            Ok(())
        }
        OrchestratorCommand::Simulate { event: SimulatedEvent::Push { branch }, server, default_branch } => {
            let branch = branch_or_current(&config, branch)?;
            let answer = simulator(&config, &server, &default_branch)?.push(&branch)?;
            eprintln!("azad-orchestrator: pushed {branch}: {answer}");
            Ok(())
        }
        OrchestratorCommand::Simulate { event: SimulatedEvent::Tag { tag }, server, default_branch } => {
            let answer = simulator(&config, &server, &default_branch)?.tag(&tag)?;
            eprintln!("azad-orchestrator: pushed the tag {tag}: {answer}");
            Ok(())
        }
        OrchestratorCommand::Simulate {
            event: SimulatedEvent::PullRequest { number, branch },
            server,
            default_branch,
        } => {
            let branch = branch_or_current(&config, branch)?;
            let answer = simulator(&config, &server, &default_branch)?.pull_request(number, &branch)?;
            eprintln!("azad-orchestrator: pull request {number} from {branch}: {answer}");
            Ok(())
        }
        OrchestratorCommand::Checks => checks(&config),
    }
}

fn webhook_secret() -> Result<Vec<u8>> {
    match std::env::var_os(WEBHOOK_SECRET_ENV).filter(|secret| !secret.is_empty()) {
        Some(secret) => Ok(secret.into_encoded_bytes()),
        None => bail!("set {WEBHOOK_SECRET_ENV} to the secret that signs webhook deliveries"),
    }
}

fn github_app(id: &str, key: &Path, api: &str) -> Result<GitHubApp> {
    let pem = fs::read_to_string(key).with_context(|| format!("reading the GitHub App's key {}", key.display()))?;
    Ok(GitHubApp::new(id, &pem, api)?)
}

fn serve(
    config: EngineConfig,
    listen: &str,
    app: Option<GitHubApp>,
    site_url: Option<&str>,
    parallelism: usize,
) -> Result<()> {
    let secret = webhook_secret()?;
    let listener = TcpListener::bind(listen).with_context(|| format!("listening on {listen}"))?;
    eprintln!("azad-orchestrator: webhooks to http://{}{}", listener.local_addr()?, orchestrator::WEBHOOK_PATH);
    let orchestrator = match app {
        Some(app) => {
            eprintln!("azad-orchestrator: check runs go to GitHub through {app:?}");
            Orchestrator::with_github(config, &secret, app)
        }
        None => Orchestrator::new(config, &secret),
    }
    .with_parallelism(parallelism);
    let orchestrator = match site_url {
        Some(url) => orchestrator.with_site(url),
        None => orchestrator,
    };
    orchestrator.serve(&listener)?;
    Ok(())
}

fn simulator(config: &EngineConfig, server: &str, default_branch: &str) -> Result<Simulator> {
    Ok(Simulator::new(&config.root, server, &webhook_secret()?, default_branch)?)
}

/// The branch to send: the one named, or the one that is checked out.
fn branch_or_current(config: &EngineConfig, branch: Option<String>) -> Result<String> {
    if let Some(branch) = branch {
        return Ok(branch);
    }
    let output =
        Command::new("git").arg("-C").arg(&config.root).args(["symbolic-ref", "--short", CHECKED_OUT]).output()?;
    if !output.status.success() {
        bail!("no branch is checked out in {}; name one", config.root.display());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn checks(config: &EngineConfig) -> Result<()> {
    let runs = orchestrator::fake_github(&config.store).check_runs()?;
    if runs.is_empty() {
        eprintln!("No check runs yet; `azad-orchestrator serve` writes them.");
    }
    for run in runs {
        println!("{run}");
    }
    Ok(())
}
