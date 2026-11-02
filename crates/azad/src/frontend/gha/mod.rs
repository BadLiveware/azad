//! The GHA (GitHub Actions) frontend: compiles a workflow file to a package of jobs. Each job runs as one impure action
//! whose command is the GHA runner (`azad-gha-runner`), which runs the steps inside the sandbox. See decision 0010.

pub mod annotation;
mod compile;
pub mod display;
mod github;
mod matrix;
mod yaml;

pub use compile::Images;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::frontend::{Finding, Frontend, FrontendError, Listed, LoadRequest, Loaded, Severity, label};
use azad_gha_runner::plan::paths;

use crate::frontend::gha::compile::Compiler;

const YAML_EXTENSIONS: [&str; 2] = [".yml", ".yaml"];
const ALL_GOAL: &str = "all";
const WORKFLOWS_DIR: &str = ".github/workflows";
const JOBS_KEY: &str = "jobs";
const UBUNTU_LABELS: [&str; 3] = ["ubuntu-latest", "ubuntu-24.04", "ubuntu-22.04"];
/// The default image for the Ubuntu labels: Debian with Node, bash, git and curl.
pub const DEFAULT_IMAGE: &str =
    "node:24-bookworm@sha256:e7add2a490bb17d97b485b8641bd88455fff7839ade0f5eba493c3508124d413";

/// Reads `<workflow>.yml` (all jobs) or `<workflow>.yml:<job>` (one job and the jobs it needs).
#[derive(Debug, Clone)]
pub struct GhaFrontend {
    images: Images,
}

impl Default for GhaFrontend {
    fn default() -> Self {
        let images = UBUNTU_LABELS.iter().map(|l| ((*l).to_owned(), DEFAULT_IMAGE.to_owned())).collect();
        GhaFrontend { images }
    }
}

impl GhaFrontend {
    pub fn with_images(images: Images) -> Self {
        GhaFrontend { images }
    }

    /// The workflow file and the goal name of a target.
    fn split(target: &str) -> Option<(&str, &str)> {
        let is_yaml = |path: &str| YAML_EXTENSIONS.iter().any(|e| path.ends_with(e));
        if is_yaml(target) {
            return Some((target, ALL_GOAL));
        }
        let (path, job) = target.rsplit_once(':')?;
        is_yaml(path).then_some((path, job))
    }

    fn compile(
        &self,
        request: &LoadRequest<'_>,
    ) -> Result<(crate::frontend::Package, String, String, Vec<Finding>), FrontendError> {
        let (file, goal) =
            GhaFrontend::split(request.target).ok_or("a workflow target is <file>.yml or <file>.yml:<job>")?;
        let path = request.root.join(file);
        let package_path = path.strip_prefix(request.root).unwrap_or(Path::new(file)).display().to_string();
        let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let (workflow, key_order) = yaml::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let name = workflow.get("name").map(azad_gha_runner::expr::to_text).unwrap_or_else(|| package_path.clone());
        let workspace = format!("{}/workspace", paths::GITHUB_DIR);
        let mut compiler = Compiler {
            package_path: &package_path,
            platform: request.platform,
            images: &self.images,
            key_order: &key_order,
            github: github::context(request.root, &name, &workspace),
            findings: Vec::new(),
        };
        let package = compiler.compile(&workflow);
        let findings = compiler.findings;
        Ok((package, package_path, goal.to_owned(), findings))
    }
}

impl Frontend for GhaFrontend {
    fn name(&self) -> &'static str {
        "gha"
    }

    fn accepts(&self, target: &str) -> bool {
        GhaFrontend::split(target).is_some()
    }

    fn load(&self, request: &LoadRequest<'_>) -> Result<Loaded, FrontendError> {
        let (package, package_path, goal, findings) = self.compile(request)?;
        let unsupported: Vec<String> = findings
            .iter()
            .filter(|f| f.severity == Severity::Unsupported)
            .map(|f| format!("{}: {}", f.location, f.message))
            .collect();
        if !unsupported.is_empty() {
            return Err(format!("the workflow cannot run:\n  {}", unsupported.join("\n  ")).into());
        }
        Ok(Loaded { package, goal: label(&package_path, &goal), scope: Some(package_path) })
    }

    fn check(&self, request: &LoadRequest<'_>) -> Result<Vec<Finding>, FrontendError> {
        let (_, _, _, mut findings) = self.compile(request)?;
        findings.sort_by(|a, b| a.severity.cmp(&b.severity).then_with(|| a.location.cmp(&b.location)));
        Ok(findings)
    }

    fn targets(&self, root: &Path, files: &BTreeSet<PathBuf>, _platform: &str) -> Vec<Listed> {
        files
            .iter()
            .filter(|file| file.parent() == Some(Path::new(WORKFLOWS_DIR)))
            .filter(|file| GhaFrontend::split(&file.display().to_string()).is_some())
            .flat_map(|file| workflow_targets(&root.join(file), &file.display().to_string()))
            .collect()
    }
}

fn workflow_targets(path: &Path, workflow: &str) -> Vec<Listed> {
    let jobs = fs::read_to_string(path)
        .map_err(|e| e.to_string())
        .and_then(|text| yaml::parse(&text).map_err(|e| e.to_string()))
        .map(|(document, order)| {
            let jobs = document.get(JOBS_KEY).and_then(serde_json::Value::as_object);
            let path = [JOBS_KEY.to_owned()];
            jobs.map(|jobs| order.keys(&path, jobs).into_iter().cloned().collect::<Vec<_>>()).unwrap_or_default()
        });
    match jobs {
        Ok(jobs) => std::iter::once(workflow.to_owned())
            .chain(jobs.into_iter().map(|job| format!("{workflow}:{job}")))
            .map(|target| Listed {
                target,
                problem: None,
                doc: String::new(),
                events: BTreeSet::new(),
                tags: BTreeSet::new(),
            })
            .collect(),
        Err(problem) => {
            vec![Listed {
                target: workflow.to_owned(),
                problem: Some(problem),
                doc: String::new(),
                events: BTreeSet::new(),
                tags: BTreeSet::new(),
            }]
        }
    }
}
