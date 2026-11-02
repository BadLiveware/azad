//! Running a host action in an image through this host's container runtime, Docker or Podman, with the workspace
//! mounted at its own path, so that the command reads and writes the developer's files (decision 0024).
//!
//! The command runs as the owner of the workspace, so that the files that it writes are theirs: rootful Docker runs it
//! as their user, rootless Podman maps their user with `keep-id`, and rootless Docker runs it as root in the container,
//! which is their user on the host. Secret values reach the runtime through its environment, never its arguments,
//! which other users of the host can read.

use std::collections::{BTreeMap, HashMap};
use std::env;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use crate::ir::action::ContainerImage;
use crate::sandbox::SandboxError;

/// The container runtime to use, by name or path; without it, the first of `docker` and `podman` on `PATH`.
pub const RUNTIME_ENV: &str = "AZAD_CONTAINER_RUNTIME";
const RUNTIMES: [&str; 2] = ["docker", "podman"];
const PODMAN: &str = "podman";
/// The security option that a rootless Docker daemon reports.
const ROOTLESS_OPTION: &str = "name=rootless";
const PATH_ENV: &str = "PATH";
const HOME_ENV: &str = "HOME";
/// A home for tools that write to it, when the action does not set one: the user who runs the command has no home in
/// the image.
const CONTAINER_HOME: &str = "/tmp";
/// The lines of a failed build that an error shows.
const BUILD_ERROR_LINES: usize = 20;
/// The task's command runs itself, as it does in an isolated run, and not as the arguments of the image's entrypoint,
/// such as the `swiftlint` of a SwiftLint image.
const NO_ENTRYPOINT: [&str; 2] = ["--entrypoint", ""];

/// A host action's run in an image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContainerRun {
    pub runtime: PathBuf,
    pub image: ContainerImage,
    /// The workspace, which the container sees at the same path.
    pub workspace: PathBuf,
    /// Directories of this host, by the path at which the container sees each: the action's cache volumes.
    pub volumes: Vec<(PathBuf, String)>,
    /// Secret values by name.
    pub secrets: BTreeMap<String, String>,
    /// The name of the container, so that it can be stopped when the action takes too long.
    pub name: String,
}

/// The container runtime of this host: `$AZAD_CONTAINER_RUNTIME`, or `docker` or `podman` on `PATH`.
pub fn find_runtime() -> Option<PathBuf> {
    match env::var_os(RUNTIME_ENV).filter(|named| !named.is_empty()) {
        Some(named) => on_path(Path::new(&named)),
        None => RUNTIMES.iter().find_map(|name| on_path(Path::new(name))),
    }
}

/// Whether the Docker daemon of `runtime` runs rootless, where root in a container is the user who runs the daemon. It
/// is asked once for each runtime.
fn is_rootless_docker(runtime: &Path) -> bool {
    static ROOTLESS: OnceLock<Mutex<HashMap<PathBuf, bool>>> = OnceLock::new();
    let known = ROOTLESS.get_or_init(Mutex::default);
    if let Some(rootless) = known.lock().expect("never poisoned").get(runtime) {
        return *rootless;
    }
    let output = Command::new(runtime).args(["info", "--format", "{{.SecurityOptions}}"]).output();
    let rootless = output.is_ok_and(|output| String::from_utf8_lossy(&output.stdout).contains(ROOTLESS_OPTION));
    known.lock().expect("never poisoned").insert(runtime.to_path_buf(), rootless);
    rootless
}

fn on_path(program: &Path) -> Option<PathBuf> {
    if program.components().count() > 1 {
        return program.is_file().then(|| program.to_path_buf());
    }
    let path = env::var_os(PATH_ENV)?;
    env::split_paths(&path).map(|dir| dir.join(program)).find(|candidate| candidate.is_file())
}

impl ContainerRun {
    fn is_podman(&self) -> bool {
        self.runtime.file_name().is_some_and(|name| name.to_string_lossy().contains(PODMAN))
    }

    /// The command that runs `command` in the image, in `dir` of the workspace, with `env`. The runtime gets the
    /// environment of this process, which it needs to reach its daemon, and the secret values.
    pub(crate) fn command(
        &self,
        dir: &Path,
        command: &[String],
        env: &BTreeMap<String, String>,
    ) -> Result<Command, SandboxError> {
        let image = self.image()?;
        let workspace = self.workspace.display().to_string();
        let mut run = Command::new(&self.runtime);
        run.args(["run", "--rm", "--init", "--pull", "missing", "--name", &self.name]);
        run.args(["--volume", &format!("{workspace}:{workspace}"), "--workdir", &dir.display().to_string()]);
        run.args(self.user_args()?);
        for (host, path) in &self.volumes {
            run.args(["--volume", &format!("{}:{path}", host.display())]);
        }
        let home = (!env.contains_key(HOME_ENV)).then(|| (HOME_ENV.to_owned(), CONTAINER_HOME.to_owned()));
        for (name, value) in env.iter().map(|(n, v)| (n.clone(), v.clone())).chain(home) {
            run.args(["--env", &format!("{name}={value}")]);
        }
        for (name, value) in &self.secrets {
            run.args(["--env", name]);
            run.env(name, value);
        }
        run.args(NO_ENTRYPOINT).arg(image).args(command);
        Ok(run)
    }

    /// The user of the command: the owner of the workspace.
    fn user_args(&self) -> Result<Vec<String>, SandboxError> {
        let owner =
            self.workspace.metadata().map_err(|error| SandboxError::Io { path: self.workspace.clone(), error })?;
        match (self.is_podman(), is_rootless_docker(&self.runtime)) {
            (true, _) => Ok(vec!["--userns=keep-id".to_owned()]),
            (false, true) => Ok(Vec::new()),
            (false, false) => Ok(vec!["--user".to_owned(), format!("{}:{}", owner.uid(), owner.gid())]),
        }
    }

    /// The image to run: the reference, or the ID of the image that the runtime builds from the Dockerfile. The
    /// runtime's own cache makes a build of an unchanged Dockerfile and context fast.
    fn image(&self) -> Result<String, SandboxError> {
        let ContainerImage::Dockerfile { file, context, target, args, contexts } = &self.image else {
            let ContainerImage::Reference(reference) = &self.image else { unreachable!("an image is one of the two") };
            return Ok(reference.clone());
        };
        let dockerfile = Dockerfile { file, context, target, args, contexts };
        build_image(&self.runtime, &self.workspace, &dockerfile, &self.secrets)
    }

    /// Stops the container, when the action took too long: stopping the runtime's client does not stop it.
    pub(crate) fn remove(&self) {
        Command::new(&self.runtime).args(["rm", "--force", &self.name]).output().ok();
    }
}

/// A Dockerfile of the workspace to build, by paths relative to the workspace root.
#[derive(Debug)]
pub struct Dockerfile<'d> {
    pub file: &'d str,
    pub context: &'d str,
    pub target: &'d str,
    pub args: &'d BTreeMap<String, String>,
    /// Named build contexts, by name, as paths from the workspace root.
    pub contexts: &'d BTreeMap<String, String>,
}

/// Builds the image of a Dockerfile with the runtime, and gives the image's ID; the runtime's own cache makes a build of
/// an unchanged Dockerfile and context fast.
pub fn build_image(
    runtime: &Path,
    workspace: &Path,
    dockerfile: &Dockerfile<'_>,
    secrets: &BTreeMap<String, String>,
) -> Result<String, SandboxError> {
    let mut build = Command::new(runtime);
    build.args(["build", "--quiet", "--file", &workspace.join(dockerfile.file).display().to_string()]);
    if !dockerfile.target.is_empty() {
        build.args(["--target", dockerfile.target]);
    }
    for (name, value) in dockerfile.args {
        build.args(["--build-arg", &format!("{name}={value}")]);
    }
    for (name, path) in dockerfile.contexts {
        build.args(["--build-context", &format!("{name}={}", workspace.join(path).display())]);
    }
    for (name, value) in secrets {
        build.args(["--secret", &format!("id={name},env={name}")]);
        build.env(name, value);
    }
    build.arg(workspace.join(dockerfile.context));
    let output = build.output().map_err(SandboxError::Spawn)?;
    if !output.status.success() {
        return Err(failed(runtime, &format!("cannot build the image of //{}", dockerfile.file), &output.stderr));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// What an image holds: the digests of its layers' contents, and the `Env` entries of its configuration, as
/// `NAME=value`. A rebuild of the same steps can give a new image ID, because BuildKit records when it built; these
/// stay the same.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageContent {
    pub layers: Vec<String>,
    pub env: Vec<String>,
}

/// What the image `id` holds.
pub fn image_content(runtime: &Path, id: &str) -> Result<ImageContent, SandboxError> {
    let field = |template: &str| -> Result<Vec<String>, SandboxError> {
        let text = runtime_output(runtime, &["image", "inspect", "--format", template, id], &format!("inspect {id}"))?;
        let values: Option<Vec<String>> = serde_json::from_str(&text)
            .map_err(|e| SandboxError::Setup(format!("{template} of the image {id}: {e}")))?;
        Ok(values.unwrap_or_default())
    };
    Ok(ImageContent { layers: field("{{json .RootFS.Layers}}")?, env: field("{{json .Config.Env}}")? })
}

/// Writes the file system of the image `id` as a tar archive at `archive`. The runtime exports a container that it
/// makes for this, and that never starts.
pub fn export_image(runtime: &Path, id: &str, archive: &Path) -> Result<(), SandboxError> {
    let what = format!("make a container of {id}");
    let container = runtime_output(runtime, &["create", "--entrypoint", "", id, EXPORT_COMMAND], &what)?;
    let output = archive.display().to_string();
    let exported = runtime_output(runtime, &["export", "--output", &output, &container], &format!("export {id}"));
    runtime_output(runtime, &["rm", &container], "remove an exported container").ok();
    exported.map(|_| ())
}

/// What a command of the runtime wrote, trimmed; an error, with the end of its errors, when it failed.
fn runtime_output(runtime: &Path, args: &[&str], what: &str) -> Result<String, SandboxError> {
    let output = Command::new(runtime).args(args).output().map_err(SandboxError::Spawn)?;
    if !output.status.success() {
        return Err(failed(runtime, &format!("cannot {what}"), &output.stderr));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// A command for the container that `export_image` makes: the runtime needs one, and never runs it.
const EXPORT_COMMAND: &str = "/azad-export";

/// The error of a runtime's command, with the end of what it wrote.
fn failed(runtime: &Path, what: &str, stderr: &[u8]) -> SandboxError {
    let stderr = String::from_utf8_lossy(stderr);
    let lines: Vec<&str> = stderr.lines().collect();
    let shown = lines[lines.len().saturating_sub(BUILD_ERROR_LINES)..].join("\n");
    SandboxError::Setup(format!("{} {what}:\n{shown}", runtime.display()))
}
