//! `azad-orchestrator`: Azad's orchestrator (decision 0020). It takes GitHub's webhooks, runs the goals that name the
//! event at the event's trust level, and writes check runs; `simulate` sends it events from a local repository.

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};
use azad::EngineConfig;
use azad::orchestrator::{self, Orchestrator, Simulator};
use clap::{Parser, Subcommand};

const DEFAULT_LISTEN: &str = "127.0.0.1:8484";
const DEFAULT_WEBHOOK_URL: &str = "http://127.0.0.1:8484/webhook";
const DEFAULT_BRANCH: &str = "main";
const WEBHOOK_SECRET_ENV: &str = "AZAD_WEBHOOK_SECRET";

#[derive(Debug, Parser)]
#[command(name = "azad-orchestrator", version, about = "Azad's orchestrator: runs goals for GitHub's events")]
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
    /// A pull request from a branch, when it is opened or gets a new commit. The default is the branch that is checked
    /// out.
    PullRequest { number: u64, branch: Option<String> },
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
        OrchestratorCommand::Serve { listen } => serve(config, &listen),
        OrchestratorCommand::Simulate { event: SimulatedEvent::Push { branch }, server, default_branch } => {
            let branch = branch_or_current(&config, branch)?;
            let answer = simulator(&config, &server, &default_branch)?.push(&branch)?;
            eprintln!("azad-orchestrator: pushed {branch}: {answer}");
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

fn serve(config: EngineConfig, listen: &str) -> Result<()> {
    let secret = webhook_secret()?;
    let listener = TcpListener::bind(listen).with_context(|| format!("listening on {listen}"))?;
    eprintln!("azad-orchestrator: webhooks to http://{}{}", listener.local_addr()?, orchestrator::WEBHOOK_PATH);
    Orchestrator::new(config, &secret).serve(&listener)?;
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
    let output = Command::new("git").arg("-C").arg(&config.root).args(["symbolic-ref", "--short", "HEAD"]).output()?;
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
