//! Trust levels: which state a run may read and write that other runs use, such as cache volumes. The orchestrator
//! derives the level of a run from its event, never from the code that runs.

use std::fmt;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Trust {
    /// Runs of the default branch, and runs on a developer's own machine.
    #[default]
    Main,
    /// Runs of pull requests and of other branches: they never write state that `main` reads.
    PullRequest,
    /// Runs that are proved to be neither: they get new, empty state, which is removed after the run.
    None,
}

impl fmt::Display for Trust {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Trust::Main => "main",
            Trust::PullRequest => "pull-request",
            Trust::None => "none",
        };
        f.write_str(name)
    }
}
