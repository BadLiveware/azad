//! The `azad` command: arguments in, the engine's work, and readable output.

mod commands;
mod output;
mod personal;
mod remote;

use output::{Format, Verbosity};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use azad::frontend::LocalRun;
use azad::history::{GitHubHistory, History};

use azad::{Engine, EngineConfig};
use clap::{Parser, Subcommand, ValueEnum};

const DEFAULT_GITHUB_GRAPHQL_URL: &str = "https://api.github.com/graphql";
const GITHUB_TOKEN_ENV: &str = "AZAD_GITHUB_TOKEN";

#[derive(Debug, Parser)]
#[command(name = "azad", version, about = "A task runner and CI system where work is a function of its inputs")]
struct Cli {
    /// The store directory. The default is $AZAD_STORE, or ~/.cache/azad.
    #[arg(long, global = true)]
    store: Option<PathBuf>,
    /// The workspace root, against which labels such as //services/orders:check resolve. The default is the nearest
    /// directory above the current one that holds .git, or the current directory.
    #[arg(long, global = true)]
    root: Option<PathBuf>,
    /// A remote cache: gha (the GitHub Actions cache, in the Azad action), dir:<path>, or none. The default is
    /// $AZAD_CACHE. Writes need a signing key in $AZAD_SIGNING_KEY; entries are believed when a key in
    /// .azad/trusted-keys or $AZAD_TRUSTED_KEYS signed them.
    #[arg(long, global = true)]
    cache: Option<String>,
    /// Run tasks in their images, in a sandbox, as CI runs them. Without it, tasks run on this host with its tools.
    #[arg(long, global = true)]
    isolated: bool,
    /// Run actions through the executor service on this Unix socket (`azad-executor`), instead of in this process.
    #[arg(long, global = true)]
    executor: Option<PathBuf>,
    /// The repository, as owner/name, whose history GitHub's API gives when this clone is shallow, with the token in
    /// $AZAD_GITHUB_TOKEN. The Azad action sets it in GitHub Actions, whose checkouts are shallow.
    #[arg(long, global = true)]
    github_history: Option<String>,
    /// GitHub's GraphQL endpoint, for --github-history.
    #[arg(long, global = true, default_value = DEFAULT_GITHUB_GRAPHQL_URL)]
    github_graphql_url: String,
    /// What to show beyond the results and errors: minimal shows only them; normal also the decisions that change
    /// what happens, such as facts from another source; diagnostic every decision and where each fact came from.
    #[arg(long, global = true, value_enum, default_value_t = Verbosity::Normal)]
    verbosity: Verbosity,
    /// Run only the tasks and actions with one of these tags, of those that the target selects, such as dotnet,node.
    /// What they need runs whatever its tags. Without --include and --exclude, the personal defaults apply: `include`
    /// and `exclude` in $AZAD_CONFIG, or in ~/.config/azad/config.toml.
    #[arg(long, global = true, value_delimiter = ',')]
    include: Vec<String>,
    /// Do not run the tasks and actions with one of these tags, of those that the target selects, such as app; it
    /// wins over --include.
    #[arg(long, global = true, value_delimiter = ',')]
    exclude: Vec<String>,
    /// Where a local run runs every task with an image: host, on this host's tools, or image, in its image through
    /// Docker or Podman, on the files of the workspace. Without it, each task's `local` decides, or `local` in the
    /// personal defaults.
    #[arg(long, global = true, value_enum)]
    local: Option<LocalChoice>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum LocalChoice {
    Host,
    Image,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run what a goal needs. Actions whose result is known do not run again. Without a goal, list the goals.
    Run {
        /// A goal: a label such as //services/orders:check; a short name such as check or :check, which is the goal
        /// of the package of the current directory, or <dir>:check, the goal of the package of <dir> (the package of
        /// a directory is the nearest directory at or above it that holds a tasks.star); a workflow such as
        /// .github/workflows/ci.yml (all jobs) or ci.yml:<job>; or <file>.azad:<goal>. `azad list` shows the goals of
        /// the workspace.
        target: Option<String>,
        /// How many actions may run at the same time.
        #[arg(long, short = 'j', default_value_t = 4)]
        jobs: usize,
        /// The limit on the run time of one action, in seconds.
        #[arg(long, default_value_t = 3600)]
        timeout: u64,
        /// How to show the run.
        #[arg(long, value_enum, default_value_t = Format::Auto)]
        output: Format,
        /// Write the outcome as JSON to this file, for other programs.
        #[arg(long)]
        report: Option<PathBuf>,
        /// Run every action that the goal needs again, even when its result is known.
        #[arg(long)]
        force: bool,
        /// Show what each action wrote, also when it succeeded, and when its result was known: then as it was when it
        /// ran. After a run, a run with --replay runs nothing and shows every action's output.
        #[arg(long)]
        replay: bool,
    },
    /// Explain what happened to an action in the last run of a goal: why it ran, was a cache hit, failed or was
    /// skipped. When it ran because the cache had no result, name the files and values that changed since an earlier
    /// run. With --next, explain what the next run would do, from the cache alone; nothing runs.
    Why {
        /// A goal, as `azad run` takes it: a label such as //services/orders:check, a short name such as check (the
        /// goal of the package of the current directory), a workflow, or <file>.azad:<goal>.
        target: String,
        /// The action: a label, or a name in the package of the goal. The default is the action that failed, or the
        /// goal's only action.
        action: Option<String>,
        /// Explain the next run instead of the last one.
        #[arg(long)]
        next: bool,
    },
    /// Print the output of an action in the last run of a goal, in the order in which it was written.
    Log {
        /// A goal, as `azad run` takes it: a label such as //services/orders:check, a short name such as check (the
        /// goal of the package of the current directory), a workflow, or <file>.azad:<goal>.
        target: String,
        /// The action: a label, or a name in the package of the goal. The default is the action that failed, or the
        /// goal's only action.
        action: Option<String>,
    },
    /// List the goals of the workspace: the goals of every tasks.star, and every workflow and job in .github/workflows.
    List,
    /// Report what the target's frontend cannot run, runs differently, or ignores, before anything runs.
    Check {
        /// A goal, as `azad run` takes it: a label such as //services/orders:check, a short name such as check (the
        /// goal of the package of the current directory), a workflow, or <file>.azad:<goal>.
        target: String,
    },
    /// Print the graph of a goal in the text form of the IR, with every source stored and named by its digest.
    Ir {
        /// A goal, as `azad run` takes it: a label such as //services/orders:check, a short name such as check (the
        /// goal of the package of the current directory), a workflow, or <file>.azad:<goal>.
        target: String,
    },
    /// Run one action of a file in the text form. The action must not read the outputs of other actions.
    Exec {
        file: PathBuf,
        action: String,
        #[arg(long, default_value_t = 3600)]
        timeout: u64,
    },
    /// List or clear cache volumes: the directories that actions' tools keep between runs.
    Cache {
        #[command(subcommand)]
        command: CacheCommand,
    },
    /// Make and show the keys that sign remote cache entries.
    Key {
        #[command(subcommand)]
        command: KeyCommand,
    },
    /// Store a directory, and print the digest of its tree.
    Ingest { dir: PathBuf },
    /// Pull an image, and print its reference by digest, the form that image() takes, then the digest of its tree.
    Pull {
        /// The image: by digest, such as python@sha256:<hex>, or by tag, such as python:3.13-slim. A tag is resolved
        /// at the registry to the digest that it points to now.
        reference: String,
        /// The platform, such as linux/amd64. The default is the host's.
        #[arg(long)]
        platform: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum KeyCommand {
    /// Print a new signing key, which is a secret, and its public key on standard error.
    Generate { name: String },
    /// Print the public key of the signing key on standard input.
    Public,
}

#[derive(Debug, Subcommand)]
enum CacheCommand {
    /// List the cache volumes, with their size and last use.
    List,
    /// Remove cache volumes: all of them, or those of one name. A volume that a run holds now stays.
    Clear { name: Option<String> },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = run(cli);
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("azad: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// GitHub's API as the source of the history that a shallow clone lacks, when `--github-history` names a repository.
fn github_history(repository: Option<&str>, url: &str) -> anyhow::Result<Option<Arc<dyn History>>> {
    let Some(repository) = repository else { return Ok(None) };
    let token = std::env::var(GITHUB_TOKEN_ENV)
        .ok()
        .filter(|token| !token.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("--github-history needs a token in ${GITHUB_TOKEN_ENV}"))?;
    Ok(Some(Arc::new(GitHubHistory::new(url, repository, token.trim())?)))
}

fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    match &cli.command {
        Command::Key { command: KeyCommand::Generate { name } } => return remote::generate(name),
        Command::Key { command: KeyCommand::Public } => return remote::public(),
        _ => {}
    }
    let mut config = EngineConfig::from_environment(cli.store, cli.root)?;
    config.isolated = cli.isolated;
    config.executor = cli.executor;
    config.history = github_history(cli.github_history.as_deref(), &cli.github_graphql_url)?;
    config.withheld = [remote::SIGNING_KEY_ENV, GITHUB_TOKEN_ENV]
        .into_iter()
        .chain(azad::remote::gha::SECRET_VARIABLES)
        .map(str::to_owned)
        .collect();
    let verbosity = cli.verbosity;
    let personal = personal::read()?;
    config.filter = personal.filter(&cli.include, &cli.exclude);
    config.local = match cli.local {
        Some(LocalChoice::Host) => Some(LocalRun::Host),
        Some(LocalChoice::Image) => Some(LocalRun::Image),
        None => personal.local()?,
    };
    if let Command::Run { output, force, .. } = &cli.command {
        config.color = output::shows_color(*output);
        config.force = *force;
    }
    if let Command::Run { timeout, .. } | Command::Exec { timeout, .. } = &cli.command {
        config.limits.timeout = std::time::Duration::from_secs(*timeout);
    }
    let engine = match remote::config(cli.cache, &config.root)? {
        Some(remote) => Engine::with_remote(config, remote)?,
        None => Engine::new(config)?,
    };
    match cli.command {
        Command::Run { target: None, .. } => commands::no_target(&engine),
        Command::Run { target: Some(target), jobs, output, report, replay, .. } => {
            commands::run(&engine, &target, jobs, output, verbosity, report, replay)
        }
        Command::List => commands::list(&engine),
        Command::Key { .. } => unreachable!("handled before the engine starts"),
        Command::Why { target, action, next } => commands::why(&engine, &target, action.as_deref(), next, verbosity),
        Command::Log { target, action } => commands::log(&engine, &target, action.as_deref(), verbosity),
        Command::Check { target } => commands::check(&engine, &target),
        Command::Ir { target } => commands::ir(&engine, &target, verbosity),
        Command::Exec { file, action, .. } => commands::exec(&engine, &file, &action),
        Command::Cache { command: CacheCommand::List } => commands::cache_list(&engine),
        Command::Cache { command: CacheCommand::Clear { name } } => commands::cache_clear(&engine, name.as_deref()),
        Command::Ingest { dir } => commands::ingest(&engine, &dir),
        Command::Pull { reference, platform } => commands::pull(&engine, &reference, platform),
    }
}
