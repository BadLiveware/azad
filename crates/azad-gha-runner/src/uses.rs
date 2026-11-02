//! `uses:` steps: the actions that the runner does itself, and Node and composite actions.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::expr::{Value, condition, interpolate};
use crate::plan::Step;
use globset::Glob;

use crate::action::{self, ActionDefinition, NodeRuns, Runs};
use crate::artifacts::{self, DEFAULT_ARTIFACT_NAME, Download, IfNoFilesFound, Uploaded};
use crate::error::{Error, WithContext};
use crate::files::copy_tree;
use crate::job::{EXIT_SUCCESS, Job, PostAction, Scope, StepRun, path_text};

const CHECKOUT: &str = "actions/checkout";
const UPLOAD_ARTIFACT: &str = "actions/upload-artifact";
const DOWNLOAD_ARTIFACT: &str = "actions/download-artifact";
const CACHE_ACTIONS: [&str; 3] = ["actions/cache", "actions/cache/restore", "actions/cache/save"];
const LOCAL_PREFIX: &str = "./";
const DOCKER_PREFIX: &str = "docker://";
const REF_SEPARATOR: char = '@';
const OWNER_REPO_PARTS: usize = 2;
const ALWAYS: &str = "always()";
const NODE_PROGRAM: &str = "node";
const INPUT_PREFIX: &str = "INPUT_";
const STATE_PREFIX: &str = "STATE_";
const HOME_PREFIX: &str = "~";
const TRUE_TEXT: &str = "true";
const FALSE_TEXT: &str = "false";

const WITH_PATH: &str = "path";
const WITH_NAME: &str = "name";
const WITH_PATTERN: &str = "pattern";
const WITH_MERGE_MULTIPLE: &str = "merge-multiple";
const WITH_IF_NO_FILES_FOUND: &str = "if-no-files-found";
const OUTPUT_CACHE_HIT: &str = "cache-hit";
const OUTPUT_DOWNLOAD_PATH: &str = "download-path";
const OUTPUT_COMMIT: &str = "commit";
const OUTPUT_REF: &str = "ref";

/// A `uses:` step of an action with metadata, and its inputs.
struct ActionCall<'s> {
    step: &'s Step,
    uses: &'s str,
    directory: PathBuf,
    inputs: BTreeMap<String, String>,
}

/// The actions that the runner does itself, because Azad gives what they fetch as inputs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Builtin {
    Checkout,
    UploadArtifact,
    DownloadArtifact,
    Cache,
}

fn builtin(uses: &str) -> Option<Builtin> {
    if uses.starts_with(LOCAL_PREFIX) {
        return None;
    }
    let name = uses.split(REF_SEPARATOR).next().unwrap_or_default().to_lowercase();
    match name.as_str() {
        CHECKOUT => Some(Builtin::Checkout),
        UPLOAD_ARTIFACT => Some(Builtin::UploadArtifact),
        DOWNLOAD_ARTIFACT => Some(Builtin::DownloadArtifact),
        name if CACHE_ACTIONS.contains(&name) => Some(Builtin::Cache),
        _ => None,
    }
}

/// `owner/repo/path@ref` gives `path`; `owner/repo@ref` gives nothing.
fn sub_path(uses: &str) -> Option<&str> {
    let name = uses.split(REF_SEPARATOR).next().unwrap_or_default();
    name.splitn(OWNER_REPO_PARTS + 1, '/').nth(OWNER_REPO_PARTS)
}

/// `INPUT_<NAME>`: upper case, with spaces as `_`.
pub(crate) fn input_variable(name: &str) -> String {
    format!("{INPUT_PREFIX}{}", name.replace(' ', "_").to_uppercase())
}

fn with_value<'w>(with: &'w BTreeMap<String, String>, key: &str) -> Option<&'w str> {
    with.get(key).map(|value| value.trim()).filter(|value| !value.is_empty())
}

impl Job<'_> {
    pub(crate) fn run_uses(
        &mut self,
        step: &Step,
        uses: &str,
        with: &BTreeMap<String, String>,
        scope: &Scope,
        layer: &BTreeMap<String, String>,
    ) -> Result<StepRun, Error> {
        let env = self.env_context(layer);
        let with = self.interpolate_map(with, &env, scope)?;
        match builtin(uses) {
            Some(Builtin::Checkout) => self.checkout(&with),
            Some(Builtin::UploadArtifact) => self.upload_artifact(&with),
            Some(Builtin::DownloadArtifact) => self.download_artifact(&with),
            Some(Builtin::Cache) => Ok(cache(uses)),
            None => self.run_action(step, uses, &with, scope, layer),
        }
    }

    /// A path from a `with:` input, below the workspace.
    fn workspace_path(&self, relative: Option<&str>) -> Result<PathBuf, Error> {
        let Some(relative) = relative else {
            return Ok(self.workspace().to_path_buf());
        };
        let expanded = match relative.strip_prefix(HOME_PREFIX) {
            Some(rest) => format!("{}{rest}", path_text(&self.layout.home())),
            None => relative.to_string(),
        };
        Ok(self.workspace().join(expanded))
    }

    fn checkout(&self, with: &BTreeMap<String, String>) -> Result<StepRun, Error> {
        let source = &self.layout.source_dir;
        if !source.is_dir() {
            return Err(Error::new(format!(
                "actions/checkout needs the repository source at {}, but it is not there; the job plan gives no source",
                source.display()
            )));
        }
        let target = self.workspace_path(with_value(with, WITH_PATH))?;
        if !target.starts_with(self.workspace()) {
            return Err(Error::new(format!(
                "actions/checkout: `path` {} is not below the workspace",
                target.display()
            )));
        }
        copy_tree(source, &target)?;
        println!("Copied the repository source to {}", target.display());
        let github = self.contexts.get(crate::context::GITHUB);
        let field = |key: &str| github.and_then(|github| github.get(key)).map(crate::expr::to_text).unwrap_or_default();
        Ok(StepRun::success(BTreeMap::from([
            (OUTPUT_COMMIT.to_string(), field("sha")),
            (OUTPUT_REF.to_string(), field("ref")),
        ])))
    }

    fn upload_artifact(&self, with: &BTreeMap<String, String>) -> Result<StepRun, Error> {
        let name = with_value(with, WITH_NAME).unwrap_or(DEFAULT_ARTIFACT_NAME);
        let Some(path) = with_value(with, WITH_PATH) else {
            return Err(Error::new("actions/upload-artifact needs `path`"));
        };
        let if_no_files_found = IfNoFilesFound::parse(with.get(WITH_IF_NO_FILES_FOUND).map(String::as_str))?;
        let uploaded = artifacts::upload(self.workspace(), &self.layout.artifacts_dir(), name, path)?;
        let message = format!("No files were found with the provided path: {path}. No artifacts will be uploaded.");
        match (uploaded, if_no_files_found) {
            (Uploaded::Files(count), _) => println!("Uploaded {count} file(s) as the artifact `{name}`"),
            (Uploaded::NothingFound, IfNoFilesFound::Warn) => println!("::warning::{message}"),
            (Uploaded::NothingFound, IfNoFilesFound::Ignore) => println!("{message}"),
            (Uploaded::NothingFound, IfNoFilesFound::Error) => return Err(Error::new(message)),
        }
        Ok(StepRun::success(BTreeMap::new()))
    }

    fn download_artifact(&self, with: &BTreeMap<String, String>) -> Result<StepRun, Error> {
        let pattern = match with_value(with, WITH_PATTERN) {
            Some(pattern) => Some(
                Glob::new(pattern).with_context(|| format!("invalid artifact pattern `{pattern}`"))?.compile_matcher(),
            ),
            None => None,
        };
        let request = Download {
            name: with_value(with, WITH_NAME).map(str::to_string),
            pattern,
            merge_multiple: with_value(with, WITH_MERGE_MULTIPLE) == Some(TRUE_TEXT),
            target: self.workspace_path(with_value(with, WITH_PATH))?,
        };
        let names = artifacts::download(&self.layout.needs_dir, &request)?;
        println!("Downloaded the artifact(s) {names:?} to {}", request.target.display());
        Ok(StepRun::success(BTreeMap::from([(OUTPUT_DOWNLOAD_PATH.to_string(), path_text(&request.target))])))
    }

    fn action_directory(&self, uses: &str) -> Result<PathBuf, Error> {
        if uses.starts_with(LOCAL_PREFIX) {
            return Ok(self.workspace().join(uses));
        }
        if uses.starts_with(DOCKER_PREFIX) {
            return Err(Error::new(format!("`{uses}`: Docker actions are not supported")));
        }
        let Some(directory) = self.plan.actions.get(uses) else {
            return Err(Error::new(format!("the job plan has no directory for the action `{uses}`")));
        };
        let directory = PathBuf::from(directory);
        match sub_path(uses) {
            Some(sub_path) => Ok(directory.join(sub_path)),
            None => Ok(directory),
        }
    }

    fn run_action(
        &mut self,
        step: &Step,
        uses: &str,
        with: &BTreeMap<String, String>,
        scope: &Scope,
        layer: &BTreeMap<String, String>,
    ) -> Result<StepRun, Error> {
        let directory = self.action_directory(uses)?;
        let definition = action::load(&directory).with_context(|| format!("action `{uses}`"))?;
        let call = ActionCall { step, uses, inputs: self.action_inputs(&definition, with, layer, scope)?, directory };
        match &definition.runs {
            Runs::Docker => {
                Err(Error::new(format!("`{uses}` is a Docker action, and Docker actions are not supported")))
            }
            Runs::Node(node) => self.run_node_action(&call, node, scope, layer),
            Runs::Composite(steps) => self.run_composite(call, &definition.outputs, steps, scope, layer),
        }
    }

    /// Runs the steps of a composite action with their own `steps` context; the action's outputs read it.
    fn run_composite(
        &mut self,
        call: ActionCall<'_>,
        outputs: &BTreeMap<String, String>,
        steps: &[Step],
        scope: &Scope,
        layer: &BTreeMap<String, String>,
    ) -> Result<StepRun, Error> {
        let inputs = call.inputs.into_iter().map(|(name, value)| (name, Value::String(value))).collect();
        let mut inner = Scope::composite(scope, call.directory, inputs, layer.clone());
        for nested in steps {
            self.run_step(nested, &mut inner);
        }
        let env = self.env_context(&inner.inherited_env);
        let outputs = self.interpolate_map(outputs, &env, &inner)?;
        let exit_code = inner.failure.unwrap_or(EXIT_SUCCESS);
        Ok(StepRun { outputs, state: BTreeMap::new(), exit_code })
    }

    /// Each input from `with:`, else from its default; both can hold expressions.
    fn action_inputs(
        &self,
        definition: &ActionDefinition,
        with: &BTreeMap<String, String>,
        layer: &BTreeMap<String, String>,
        scope: &Scope,
    ) -> Result<BTreeMap<String, String>, Error> {
        let env = self.env_context(layer);
        let expressions = self.expressions(&env, scope);
        let mut inputs = BTreeMap::new();
        for input in &definition.inputs {
            let given = with.iter().find(|(key, _)| key.eq_ignore_ascii_case(&input.name));
            let value = match (given, &input.default) {
                (Some((_, value)), _) => value.clone(),
                (None, Some(default)) => interpolate(default, &expressions)
                    .with_context(|| format!("cannot evaluate the default of input `{}`", input.name))?,
                (None, None) => String::new(),
            };
            inputs.insert(input.name.clone(), value);
        }
        for (key, value) in with {
            let is_declared = definition.inputs.iter().any(|input| input.name.eq_ignore_ascii_case(key));
            if !is_declared {
                println!("::warning::Unexpected input `{key}`; the action does not declare it");
                inputs.insert(key.clone(), value.clone());
            }
        }
        Ok(inputs)
    }

    /// Runs `pre` (when `pre-if` holds) and `main`, and keeps `post` for the end of the job.
    fn run_node_action(
        &mut self,
        call: &ActionCall<'_>,
        node: &NodeRuns,
        scope: &Scope,
        layer: &BTreeMap<String, String>,
    ) -> Result<StepRun, Error> {
        let ActionCall { step, uses, directory, inputs } = call;
        let env = self.env_context(layer);
        let mut extra: BTreeMap<String, String> =
            inputs.iter().map(|(name, value)| (input_variable(name), value.clone())).collect();
        extra.insert("GITHUB_ACTION".to_string(), step.id.clone().unwrap_or_else(|| uses.to_string()));
        extra.insert("GITHUB_ACTION_REPOSITORY".to_string(), action_repository(uses));
        extra.insert("GITHUB_ACTION_REF".to_string(), action_ref(uses));
        let mut state = BTreeMap::new();
        if let Some(pre) = &node.pre {
            let pre_condition = node.pre_if.as_deref().unwrap_or(ALWAYS);
            let should_run = condition(pre_condition, &self.expressions(&env, scope))
                .with_context(|| format!("cannot evaluate `pre-if: {pre_condition}`"))?;
            if should_run {
                let pre_run = self.run_node_script(&directory.join(pre), directory, &env, &extra)?;
                if !pre_run.succeeded() {
                    return Ok(pre_run);
                }
                state.extend(pre_run.state);
            }
        }
        let main = self.run_node_script(&directory.join(&node.main), directory, &env, &with_state(&extra, &state))?;
        state.extend(main.state.clone());
        if let Some(post) = &node.post {
            self.posts.push(PostAction {
                name: step.name.clone().unwrap_or_else(|| uses.to_string()),
                script: directory.join(post),
                condition: node.post_if.clone().unwrap_or_else(|| ALWAYS.to_string()),
                action_path: directory.to_path_buf(),
                env: layer.clone(),
                process_env: with_state(&extra, &state),
            });
        }
        Ok(main)
    }

    /// Runs one part of a Node action with the `node` on `PATH`, in the workspace.
    pub(crate) fn run_node_script(
        &mut self,
        script: &Path,
        action_path: &Path,
        env: &BTreeMap<String, String>,
        extra: &BTreeMap<String, String>,
    ) -> Result<StepRun, Error> {
        let files = self.new_step_files()?;
        let mut process_env = self.process_env(env, &files, Some(action_path));
        process_env.extend(extra.iter().map(|(name, value)| (name.clone(), value.clone())));
        let workspace = self.workspace().to_path_buf();
        let run = self.run_process(NODE_PROGRAM, &[path_text(script)], &workspace, &process_env)?;
        self.finish_step(run, &files)
    }
}

fn with_state(extra: &BTreeMap<String, String>, state: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut combined = extra.clone();
    combined.extend(state.iter().map(|(name, value)| (format!("{STATE_PREFIX}{name}"), value.clone())));
    combined
}

fn action_repository(uses: &str) -> String {
    let name = uses.split(REF_SEPARATOR).next().unwrap_or_default();
    name.split('/').take(OWNER_REPO_PARTS).collect::<Vec<_>>().join("/")
}

fn action_ref(uses: &str) -> String {
    uses.split_once(REF_SEPARATOR).map(|(_, reference)| reference.to_string()).unwrap_or_default()
}

fn cache(uses: &str) -> StepRun {
    println!("::notice::{uses} does nothing in Azad: Azad's own cache applies instead, by the inputs of each action");
    StepRun::success(BTreeMap::from([(OUTPUT_CACHE_HIT.to_string(), FALSE_TEXT.to_string())]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_builtin_actions_by_the_name_before_the_ref() {
        assert_eq!(builtin("actions/checkout@v4"), Some(Builtin::Checkout));
        assert_eq!(builtin("Actions/Upload-Artifact@v4"), Some(Builtin::UploadArtifact));
        assert_eq!(builtin("actions/cache/restore@v4"), Some(Builtin::Cache));
        assert_eq!(builtin("actions/setup-node@v4"), None);
        assert_eq!(builtin("./actions/checkout"), None);
    }

    #[test]
    fn finds_the_sub_path_of_an_action() {
        assert_eq!(sub_path("owner/repo@v1"), None);
        assert_eq!(sub_path("owner/repo/path/to@v1"), Some("path/to"));
        assert_eq!(action_repository("owner/repo/path@v1"), "owner/repo");
        assert_eq!(action_ref("owner/repo/path@v1"), "v1");
    }

    #[test]
    fn input_variables_are_upper_case_with_underscores() {
        assert_eq!(input_variable("node-version"), "INPUT_NODE-VERSION");
        assert_eq!(input_variable("my input"), "INPUT_MY_INPUT");
    }
}
