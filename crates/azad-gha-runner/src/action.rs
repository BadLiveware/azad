//! The metadata of an action: `action.yml` or `action.yaml`.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::plan::{Step, StepKind};
use yaml_rust2::{Yaml, YamlLoader};

use crate::error::{Error, WithContext};

const METADATA_FILES: [&str; 2] = ["action.yml", "action.yaml"];
const NODE_RUNTIMES: [&str; 4] = ["node12", "node16", "node20", "node24"];
const COMPOSITE: &str = "composite";
const DOCKER: &str = "docker";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ActionInput {
    pub(crate) name: String,
    pub(crate) default: Option<String>,
}

/// How an action runs.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Runs {
    Node(NodeRuns),
    Composite(Vec<Step>),
    Docker,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NodeRuns {
    pub(crate) main: String,
    pub(crate) pre: Option<String>,
    pub(crate) pre_if: Option<String>,
    pub(crate) post: Option<String>,
    pub(crate) post_if: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ActionDefinition {
    pub(crate) inputs: Vec<ActionInput>,
    /// Each output and its `value:` template; only composite actions have values.
    pub(crate) outputs: BTreeMap<String, String>,
    pub(crate) runs: Runs,
}

/// Reads the metadata file in an action's directory.
pub(crate) fn load(directory: &Path) -> Result<ActionDefinition, Error> {
    let Some(file) = METADATA_FILES.iter().map(|name| directory.join(name)).find(|path| path.is_file()) else {
        return Err(Error::new(format!(
            "the action in {} has no {} or {}",
            directory.display(),
            METADATA_FILES[0],
            METADATA_FILES[1]
        )));
    };
    let text = fs::read_to_string(&file).with_context(|| format!("cannot read {}", file.display()))?;
    parse(&text).with_context(|| format!("invalid action metadata in {}", file.display()))
}

pub(crate) fn parse(text: &str) -> Result<ActionDefinition, Error> {
    let documents = YamlLoader::load_from_str(text).with_context(|| "not valid YAML".to_string())?;
    let Some(root) = documents.first() else {
        return Err(Error::new("the file is empty"));
    };
    Ok(ActionDefinition {
        inputs: inputs(&root["inputs"]),
        outputs: outputs(&root["outputs"]),
        runs: runs(&root["runs"])?,
    })
}

fn inputs(node: &Yaml) -> Vec<ActionInput> {
    entries(node).map(|(name, input)| ActionInput { name, default: text(&input["default"]) }).collect()
}

fn outputs(node: &Yaml) -> BTreeMap<String, String> {
    entries(node).filter_map(|(name, output)| text(&output["value"]).map(|value| (name, value))).collect()
}

fn runs(node: &Yaml) -> Result<Runs, Error> {
    let using = text(&node["using"]).unwrap_or_default();
    match using.as_str() {
        runtime if NODE_RUNTIMES.contains(&runtime) => node_runs(node),
        COMPOSITE => composite_steps(&node["steps"]).map(Runs::Composite),
        DOCKER => Ok(Runs::Docker),
        other => Err(Error::new(format!(
            "`runs.using: {other}` is not known: use one of {NODE_RUNTIMES:?}, `{COMPOSITE}` or `{DOCKER}`"
        ))),
    }
}

fn node_runs(node: &Yaml) -> Result<Runs, Error> {
    let Some(main) = text(&node["main"]) else {
        return Err(Error::new("a Node action needs `runs.main`"));
    };
    Ok(Runs::Node(NodeRuns {
        main,
        pre: text(&node["pre"]),
        pre_if: text(&node["pre-if"]),
        post: text(&node["post"]),
        post_if: text(&node["post-if"]),
    }))
}

fn composite_steps(node: &Yaml) -> Result<Vec<Step>, Error> {
    let Some(steps) = node.as_vec() else {
        return Err(Error::new("a composite action needs a list in `runs.steps`"));
    };
    steps
        .iter()
        .enumerate()
        .map(|(index, step)| {
            composite_step(step).with_context(|| format!("step {} of the composite action", index + 1))
        })
        .collect()
}

fn composite_step(node: &Yaml) -> Result<Step, Error> {
    let kind = match (text(&node["run"]), text(&node["uses"])) {
        (Some(script), None) => StepKind::Run { script },
        (None, Some(action)) => StepKind::Uses { action, with: text_map(&node["with"]) },
        _ => return Err(Error::new("a step needs exactly one of `run` and `uses`")),
    };
    Ok(Step {
        id: text(&node["id"]),
        name: text(&node["name"]),
        condition: text(&node["if"]),
        env: text_map(&node["env"]),
        continue_on_error: text(&node["continue-on-error"]),
        working_directory: text(&node["working-directory"]),
        shell: text(&node["shell"]),
        kind,
        hint: None,
        module: None,
    })
}

fn entries(node: &Yaml) -> impl Iterator<Item = (String, &Yaml)> {
    node.as_hash().into_iter().flatten().filter_map(|(key, value)| text(key).map(|key| (key, value)))
}

fn text_map(node: &Yaml) -> BTreeMap<String, String> {
    entries(node).map(|(key, value)| (key, text(value).unwrap_or_default())).collect()
}

/// A scalar as text, as GitHub reads it; `None` for a missing value, null, a list or a map.
fn text(node: &Yaml) -> Option<String> {
    match node {
        Yaml::String(value) | Yaml::Real(value) => Some(value.clone()),
        Yaml::Integer(value) => Some(value.to_string()),
        Yaml::Boolean(value) => Some(value.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_node_action() {
        let definition = parse(
            "name: x\ninputs:\n  who:\n    default: ${{ github.actor }}\n  count:\n    default: 3\nruns:\n  using: node20\n  main: dist/index.js\n  post: dist/post.js\n",
        )
        .expect("valid");
        assert_eq!(
            definition.inputs,
            vec![
                ActionInput { name: "who".to_string(), default: Some("${{ github.actor }}".to_string()) },
                ActionInput { name: "count".to_string(), default: Some("3".to_string()) },
            ]
        );
        let Runs::Node(node) = definition.runs else {
            panic!("not a node action");
        };
        assert_eq!(node.main, "dist/index.js");
        assert_eq!(node.post.as_deref(), Some("dist/post.js"));
        assert_eq!(node.pre, None);
    }

    #[test]
    fn reads_a_composite_action() {
        let definition = parse(
            "outputs:\n  greeting:\n    value: ${{ steps.say.outputs.text }}\nruns:\n  using: composite\n  steps:\n    - id: say\n      run: echo hi\n      shell: bash\n      continue-on-error: true\n    - uses: ./other\n      with:\n        flag: true\n",
        )
        .expect("valid");
        assert_eq!(definition.outputs.get("greeting").map(String::as_str), Some("${{ steps.say.outputs.text }}"));
        let Runs::Composite(steps) = definition.runs else {
            panic!("not a composite action");
        };
        assert_eq!(steps[0].shell.as_deref(), Some("bash"));
        assert_eq!(steps[0].continue_on_error.as_deref(), Some("true"));
        assert_eq!(
            steps[1].kind,
            StepKind::Uses {
                action: "./other".to_string(),
                with: BTreeMap::from([("flag".to_string(), "true".to_string())])
            }
        );
    }

    #[test]
    fn reads_docker_and_refuses_unknown_runtimes() {
        assert_eq!(parse("runs:\n  using: docker\n  image: Dockerfile\n").expect("valid").runs, Runs::Docker);
        assert!(parse("runs:\n  using: node8\n  main: a.js\n").is_err());
        assert!(parse("runs:\n  using: node20\n").is_err());
    }
}
