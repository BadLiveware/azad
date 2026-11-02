use std::path::PathBuf;
use std::process::ExitCode;

use azad_gha_runner::{Layout, run, run_hinted};

/// Runs a hinted step of GHA+ in its own action (decision 0013).
const STEP_FLAG: &str = "--step";

/// `azad-gha-runner [<plan file>]`: runs the job of the plan, by default the one in the sandbox.
/// `azad-gha-runner --step`: runs the hinted step in the sandbox's hint directory.
fn main() -> ExitCode {
    let mut layout = Layout::sandbox();
    if std::env::args().nth(1).as_deref() == Some(STEP_FLAG) {
        return ExitCode::from(run_hinted(&layout).exit_code());
    }
    if let Some(plan_file) = std::env::args_os().nth(1) {
        layout.plan_file = PathBuf::from(plan_file);
    }
    ExitCode::from(run(&layout).exit_code())
}
