use std::path::{Path, PathBuf};

use crate::plan::paths;

/// The work directory of the action in the sandbox; only its declared outputs are kept.
pub const WORK_DIR: &str = "/work";

const WORKSPACE: &str = "workspace";
const HOME: &str = "home";
const TEMP: &str = "temp";
const TOOL_CACHE: &str = "toolcache";

/// Where the runner finds its inputs and puts its outputs.
/// The sandbox uses `Layout::sandbox`; tests use temporary directories.
#[derive(Clone, Debug)]
pub struct Layout {
    pub plan_file: PathBuf,
    pub work_dir: PathBuf,
    pub source_dir: PathBuf,
    pub needs_dir: PathBuf,
    pub github_dir: PathBuf,
    /// GHA+ (decision 0013): the state of the part before, the result of the hinted step before, and the hint of a
    /// hinted step.
    pub state_dir: PathBuf,
    pub hinted_dir: PathBuf,
    pub hint_dir: PathBuf,
}

impl Layout {
    pub fn sandbox() -> Self {
        Self {
            plan_file: Path::new(paths::PLAN_DIR).join(paths::PLAN_FILE),
            work_dir: PathBuf::from(WORK_DIR),
            source_dir: PathBuf::from(paths::SOURCE_DIR),
            needs_dir: PathBuf::from(paths::NEEDS_DIR),
            github_dir: PathBuf::from(paths::GITHUB_DIR),
            state_dir: PathBuf::from(paths::STATE_DIR),
            hinted_dir: PathBuf::from(paths::HINTED_DIR),
            hint_dir: PathBuf::from(paths::HINT_DIR),
        }
    }

    pub fn workspace(&self) -> PathBuf {
        self.github_dir.join(WORKSPACE)
    }

    pub fn home(&self) -> PathBuf {
        self.github_dir.join(HOME)
    }

    pub fn temp(&self) -> PathBuf {
        self.github_dir.join(TEMP)
    }

    pub fn tool_cache(&self) -> PathBuf {
        self.github_dir.join(TOOL_CACHE)
    }

    pub fn meta_dir(&self) -> PathBuf {
        self.work_dir.join(paths::META_OUTPUT)
    }

    pub fn artifacts_dir(&self) -> PathBuf {
        self.work_dir.join(paths::ARTIFACTS_OUTPUT)
    }

    pub fn result_file(&self) -> PathBuf {
        self.meta_dir().join(paths::RESULT_FILE)
    }

    /// `<needs>/<job>/meta/result.json` of a job in `needs`.
    pub fn need_result(&self, job: &str) -> PathBuf {
        self.needs_dir.join(job).join(paths::META_OUTPUT).join(paths::RESULT_FILE)
    }
}
