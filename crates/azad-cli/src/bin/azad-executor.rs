//! `azad-executor`: Azad's executor as a service on a Unix socket (decision 0019), for the hosts that run CI. Each
//! connection is a sandbox group; with `--sandbox vm`, one microVM serves it.

use std::fs;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use azad::{Engine, EngineConfig, SandboxKind};
use clap::{Parser, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "azad-executor",
    version = azad::VERSION,
    about = "Azad's executor: runs the actions that clients send, in sandboxes, until it is stopped"
)]
struct Cli {
    /// The Unix socket on which to listen. `azad --executor <socket>` and the orchestrator send their actions here.
    #[arg(long)]
    socket: PathBuf,
    /// The store directory. The default is $AZAD_STORE, or ~/.cache/azad.
    #[arg(long)]
    store: Option<PathBuf>,
    /// Where actions run: bubblewrap on this host's kernel, or microVMs, one for each sandbox group, which need Cloud
    /// Hypervisor, /dev/kvm, the guest kernel in $AZAD_VM_KERNEL and azad-guest next to this program; actions with
    /// network in a VM also need slirp4netns and nft.
    #[arg(long, value_enum, default_value_t = SandboxChoice::Bubblewrap)]
    sandbox: SandboxChoice,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum SandboxChoice {
    Bubblewrap,
    Vm,
}

fn main() -> ExitCode {
    match serve(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("azad-executor: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// Serves the executor on the socket. A socket file that nothing listens on is left from an executor that stopped,
/// and is replaced.
fn serve(cli: Cli) -> Result<()> {
    let mut config = EngineConfig::from_environment(cli.store, None)?;
    config.sandbox = match cli.sandbox {
        SandboxChoice::Bubblewrap => SandboxKind::Bubblewrap,
        SandboxChoice::Vm => SandboxKind::Vm,
    };
    let service = Engine::executor_service(&config)?;
    let socket = &cli.socket;
    if socket.exists() {
        if UnixStream::connect(socket).is_ok() {
            bail!("an executor listens on {} already", socket.display());
        }
        fs::remove_file(socket).with_context(|| format!("removing the old socket {}", socket.display()))?;
    }
    let listener = UnixListener::bind(socket).with_context(|| format!("listening on {}", socket.display()))?;
    eprintln!("azad-executor: listening on {}", socket.display());
    service.serve(&listener).with_context(|| format!("serving on {}", socket.display()))
}
