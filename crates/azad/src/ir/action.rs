//! Actions. An `ActionDef` is an action in a graph, whose inputs can be the outputs of other actions. An `Action` is a
//! resolved action: every input is a digest, so its encoding gives the action digest, which is the cache key.

use std::collections::{BTreeMap, BTreeSet};

use crate::ir::canonical::{
    Canonical, CanonicalError, Fields, digest_from_value, digest_to_value, text_list, text_map, text_values, texts,
};
use crate::ir::cbor::Value;
use crate::ir::digest::Digest;

/// The directory in which an action starts, and under which its outputs are.
pub const WORK_DIR: &str = "/work";

const KEY_KIND: &str = "k";
const KEY_EXPECTED: &str = "x";
const KEY_PLATFORM: &str = "p";
const KEY_ROOT: &str = "r";
const KEY_COMMAND: &str = "c";
const KEY_ENV: &str = "e";
const KEY_INPUTS: &str = "i";
const KEY_OUTPUTS: &str = "o";
const KEY_CACHES: &str = "m";
const KEY_WORK: &str = "s";
const KEY_DIR: &str = "d";
const KEY_WORKSPACE: &str = "h";
const KEY_CONTAINER: &str = "n";
const KEY_SECRETS: &str = "z";
const KEY_IMAGE: &str = "image";
const KEY_DOCKERFILE: &str = "dockerfile";
const KEY_CONTEXT: &str = "context";
const KEY_TARGET: &str = "target";
const KEY_ARGS: &str = "args";
const KEY_CONTEXTS: &str = "contexts";
const KEY_VOLUMES: &str = "volumes";
const KEY_TREE: &str = "tree";
const KEY_ACTION: &str = "action";
const KEY_PATH: &str = "path";

const KIND_HERMETIC: &str = "hermetic";
const KIND_FIXED_OUTPUT: &str = "fixed-output";
const KIND_IMPURE: &str = "impure";
const KIND_FETCH: &str = "fetch";
const KIND_HOST: &str = "host";

/// What an action may reach, and how its result is cached. The sandbox enforces the kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// No network; cached by the digest of the action.
    Hermetic,
    /// Network; each output must have the declared digest.
    FixedOutput { expected: BTreeMap<String, Digest> },
    /// Network; cached by the digest of the action, whose inputs hold the lock files. See decision 0007.
    Fetch,
    /// Network; never cached.
    Impure,
    /// Runs on this host, with its tools and environment, in the workspace at `workspace` instead of a sandbox
    /// (decision 0015). Its work directory is the workspace, so it reads and writes the files there. Cached by the
    /// digest of the action on this host only, because the digest cannot describe the host. With a container, it runs
    /// in an image through this host's container runtime, with the workspace mounted at the same path (decision 0024).
    Host { workspace: String, container: Option<Container> },
}

/// The image in which a host action runs, through this host's container runtime, and its cache volumes (decision
/// 0024).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Container {
    pub image: ContainerImage,
    /// Cache volumes at absolute paths of the container, by path, and the name of each.
    pub volumes: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContainerImage {
    /// An image by a reference with its digest, which the runtime pulls.
    Reference(String),
    /// An image that the runtime builds from a Dockerfile of the workspace, by paths relative to the workspace. The
    /// action's `work` holds the Dockerfile and the context, so that their content is part of the digest. `contexts`
    /// are BuildKit's named build contexts, by name, which `COPY --from=<name>` reads (decision 0024, amendment).
    Dockerfile {
        file: String,
        context: String,
        target: String,
        args: BTreeMap<String, String>,
        contexts: BTreeMap<String, String>,
    },
}

impl Container {
    fn to_value(&self) -> Value {
        let mut map = match &self.image {
            ContainerImage::Reference(reference) => {
                BTreeMap::from([(KEY_IMAGE.to_owned(), Value::Text(reference.clone()))])
            }
            ContainerImage::Dockerfile { file, context, target, args, contexts } => {
                let mut map = BTreeMap::from([
                    (KEY_DOCKERFILE.to_owned(), Value::Text(file.clone())),
                    (KEY_CONTEXT.to_owned(), Value::Text(context.clone())),
                ]);
                if !target.is_empty() {
                    map.insert(KEY_TARGET.to_owned(), Value::Text(target.clone()));
                }
                if !args.is_empty() {
                    map.insert(KEY_ARGS.to_owned(), text_values(args));
                }
                if !contexts.is_empty() {
                    map.insert(KEY_CONTEXTS.to_owned(), text_values(contexts));
                }
                map
            }
        };
        if !self.volumes.is_empty() {
            map.insert(KEY_VOLUMES.to_owned(), text_values(&self.volumes));
        }
        Value::Map(map)
    }

    fn from_value(value: Value) -> Result<Self, CanonicalError> {
        let Value::Map(map) = value else { return Err(CanonicalError::Field(KEY_CONTAINER)) };
        let mut fields = Fields::new(map);
        let text = |value: Option<Value>, key: &'static str| match value {
            Some(Value::Text(text)) => Ok(Some(text)),
            Some(_) => Err(CanonicalError::Field(key)),
            None => Ok(None),
        };
        let texts = |value: Option<Value>, key: &'static str| match value {
            Some(Value::Map(map)) => text_map(map, key),
            Some(_) => Err(CanonicalError::Field(key)),
            None => Ok(BTreeMap::new()),
        };
        let image = match text(fields.take_optional(KEY_IMAGE), KEY_IMAGE)? {
            Some(reference) => ContainerImage::Reference(reference),
            None => ContainerImage::Dockerfile {
                file: text(fields.take_optional(KEY_DOCKERFILE), KEY_DOCKERFILE)?
                    .ok_or(CanonicalError::Field(KEY_DOCKERFILE))?,
                context: text(fields.take_optional(KEY_CONTEXT), KEY_CONTEXT)?
                    .ok_or(CanonicalError::Field(KEY_CONTEXT))?,
                target: text(fields.take_optional(KEY_TARGET), KEY_TARGET)?.unwrap_or_default(),
                args: texts(fields.take_optional(KEY_ARGS), KEY_ARGS)?,
                contexts: texts(fields.take_optional(KEY_CONTEXTS), KEY_CONTEXTS)?,
            },
        };
        Ok(Container { image, volumes: texts(fields.take_optional(KEY_VOLUMES), KEY_VOLUMES)? })
    }
}

impl Kind {
    pub fn name(&self) -> &'static str {
        match self {
            Kind::Hermetic => KIND_HERMETIC,
            Kind::FixedOutput { .. } => KIND_FIXED_OUTPUT,
            Kind::Fetch => KIND_FETCH,
            Kind::Impure => KIND_IMPURE,
            Kind::Host { .. } => KIND_HOST,
        }
    }

    pub fn has_network(&self) -> bool {
        !matches!(self, Kind::Hermetic)
    }

    pub fn is_cacheable(&self) -> bool {
        !matches!(self, Kind::Impure)
    }

    /// Whether another machine can use a result of the action, through a remote cache.
    pub fn is_shareable(&self) -> bool {
        self.is_cacheable() && !matches!(self, Kind::Host { .. })
    }
}

/// Where an input comes from, in a graph.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    Tree(Digest),
    Output { action: String, path: String },
}

/// The parts that an `ActionDef` and an `Action` share; `S` is the type of an input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spec<S> {
    pub kind: Kind,
    pub platform: String,
    pub root: S,
    pub command: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// Inputs by the absolute path at which the sandbox mounts them, read-only.
    pub inputs: BTreeMap<String, S>,
    /// Output directories, relative to `WORK_DIR`.
    pub outputs: BTreeSet<String>,
    /// Cache volumes by the absolute path at which the sandbox mounts them, writable. Their content is outside the
    /// digest; see decision 0011.
    pub caches: BTreeMap<String, String>,
    /// Trees that the work directory holds when the command starts, by the path relative to it at which each is
    /// placed; the command may change them. Shallower paths are placed first.
    pub work: BTreeMap<String, S>,
    /// The directory in which the command starts, relative to the work directory; empty for the work directory.
    pub dir: String,
    /// The variables of `azad`'s environment that the command gets, by name, such as a registry's token. Their names
    /// are part of the digest, and their values are not: a secret authorizes, it does not change a result (decision
    /// 0024).
    pub secrets: BTreeSet<String>,
}

pub type ActionDef = Spec<Source>;
pub type Action = Spec<Digest>;

impl ActionDef {
    /// The actions whose outputs this action reads.
    pub fn dependencies(&self) -> BTreeSet<&str> {
        std::iter::once(&self.root)
            .chain(self.inputs.values())
            .chain(self.work.values())
            .filter_map(|source| match source {
                Source::Output { action, .. } => Some(action.as_str()),
                Source::Tree(_) => None,
            })
            .collect()
    }

    /// Replaces each output reference with the digest that `lookup` gives, or returns the first reference that it
    /// cannot resolve.
    pub fn resolve(&self, lookup: impl Fn(&str, &str) -> Option<Digest>) -> Result<Action, Source> {
        let resolve_one = |source: &Source| match source {
            Source::Tree(digest) => Ok(*digest),
            Source::Output { action, path } => lookup(action, path).ok_or_else(|| source.clone()),
        };
        let inputs = self
            .inputs
            .iter()
            .map(|(path, source)| resolve_one(source).map(|digest| (path.clone(), digest)))
            .collect::<Result<_, _>>()?;
        let work = self
            .work
            .iter()
            .map(|(path, source)| resolve_one(source).map(|digest| (path.clone(), digest)))
            .collect::<Result<_, _>>()?;
        Ok(Action {
            kind: self.kind.clone(),
            platform: self.platform.clone(),
            root: resolve_one(&self.root)?,
            command: self.command.clone(),
            env: self.env.clone(),
            inputs,
            outputs: self.outputs.clone(),
            caches: self.caches.clone(),
            work,
            dir: self.dir.clone(),
            secrets: self.secrets.clone(),
        })
    }
}

impl Canonical for Action {
    const TYPE: &'static str = "action";

    fn fields(&self) -> BTreeMap<String, Value> {
        spec_fields(self, digest_to_value)
    }

    fn from_fields(fields: &mut Fields) -> Result<Self, CanonicalError> {
        spec_from_fields(fields, |v, key| digest_from_value(&v).ok_or(CanonicalError::Field(key)))
    }
}

pub(crate) fn spec_fields<S>(spec: &Spec<S>, source: impl Fn(&S) -> Value) -> BTreeMap<String, Value> {
    let mut map = BTreeMap::from([
        (KEY_KIND.to_owned(), Value::Text(spec.kind.name().to_owned())),
        (KEY_PLATFORM.to_owned(), Value::Text(spec.platform.clone())),
        (KEY_ROOT.to_owned(), source(&spec.root)),
        (KEY_COMMAND.to_owned(), texts(&spec.command)),
        (KEY_ENV.to_owned(), text_values(&spec.env)),
        (KEY_INPUTS.to_owned(), Value::Map(spec.inputs.iter().map(|(k, s)| (k.clone(), source(s))).collect())),
        (KEY_OUTPUTS.to_owned(), Value::Array(spec.outputs.iter().cloned().map(Value::Text).collect())),
    ]);
    if let Kind::FixedOutput { expected } = &spec.kind {
        let expected = expected.iter().map(|(k, d)| (k.clone(), digest_to_value(d))).collect();
        map.insert(KEY_EXPECTED.to_owned(), Value::Map(expected));
    }
    if !spec.caches.is_empty() {
        map.insert(KEY_CACHES.to_owned(), text_values(&spec.caches));
    }
    if let Kind::Host { workspace, container } = &spec.kind {
        map.insert(KEY_WORKSPACE.to_owned(), Value::Text(workspace.clone()));
        if let Some(container) = container {
            map.insert(KEY_CONTAINER.to_owned(), container.to_value());
        }
    }
    if !spec.secrets.is_empty() {
        map.insert(KEY_SECRETS.to_owned(), Value::Array(spec.secrets.iter().cloned().map(Value::Text).collect()));
    }
    if !spec.work.is_empty() {
        map.insert(KEY_WORK.to_owned(), Value::Map(spec.work.iter().map(|(k, s)| (k.clone(), source(s))).collect()));
    }
    if !spec.dir.is_empty() {
        map.insert(KEY_DIR.to_owned(), Value::Text(spec.dir.clone()));
    }
    map
}

pub(crate) fn spec_from_fields<S>(
    fields: &mut Fields,
    source: impl Fn(Value, &'static str) -> Result<S, CanonicalError>,
) -> Result<Spec<S>, CanonicalError> {
    let kind = match fields.text(KEY_KIND)?.as_str() {
        KIND_HERMETIC => Kind::Hermetic,
        KIND_IMPURE => Kind::Impure,
        KIND_FETCH => Kind::Fetch,
        KIND_HOST => Kind::Host {
            workspace: fields.text(KEY_WORKSPACE)?,
            container: fields.take_optional(KEY_CONTAINER).map(Container::from_value).transpose()?,
        },
        KIND_FIXED_OUTPUT => {
            let expected = fields
                .map(KEY_EXPECTED)?
                .into_iter()
                .map(|(k, v)| digest_from_value(&v).map(|d| (k, d)).ok_or(CanonicalError::Field(KEY_EXPECTED)))
                .collect::<Result<_, _>>()?;
            Kind::FixedOutput { expected }
        }
        other => return Err(CanonicalError::Invalid(format!("unknown action kind {other:?}"))),
    };
    let outputs = text_list(fields.array(KEY_OUTPUTS)?, KEY_OUTPUTS)?;
    let output_count = outputs.len();
    let outputs: BTreeSet<String> = outputs.into_iter().collect();
    if outputs.len() != output_count {
        return Err(CanonicalError::Invalid("an output is declared twice".to_owned()));
    }
    let inputs = fields
        .map(KEY_INPUTS)?
        .into_iter()
        .map(|(k, v)| source(v, KEY_INPUTS).map(|s| (k, s)))
        .collect::<Result<_, _>>()?;
    Ok(Spec {
        kind,
        platform: fields.text(KEY_PLATFORM)?,
        root: source(fields.take(KEY_ROOT)?, KEY_ROOT)?,
        command: text_list(fields.array(KEY_COMMAND)?, KEY_COMMAND)?,
        env: text_map(fields.map(KEY_ENV)?, KEY_ENV)?,
        inputs,
        outputs,
        caches: match fields.take_optional(KEY_CACHES) {
            Some(Value::Map(map)) if !map.is_empty() => text_map(map, KEY_CACHES)?,
            Some(_) => return Err(CanonicalError::Field(KEY_CACHES)),
            None => BTreeMap::new(),
        },
        work: match fields.take_optional(KEY_WORK) {
            Some(Value::Map(map)) if !map.is_empty() => {
                map.into_iter().map(|(k, v)| source(v, KEY_WORK).map(|s| (k, s))).collect::<Result<_, _>>()?
            }
            Some(_) => return Err(CanonicalError::Field(KEY_WORK)),
            None => BTreeMap::new(),
        },
        dir: match fields.take_optional(KEY_DIR) {
            Some(Value::Text(dir)) if !dir.is_empty() => dir,
            Some(_) => return Err(CanonicalError::Field(KEY_DIR)),
            None => String::new(),
        },
        secrets: match fields.take_optional(KEY_SECRETS) {
            Some(Value::Array(names)) if !names.is_empty() => text_list(names, KEY_SECRETS)?.into_iter().collect(),
            Some(_) => return Err(CanonicalError::Field(KEY_SECRETS)),
            None => BTreeSet::new(),
        },
    })
}

pub(crate) fn source_to_value(source: &Source) -> Value {
    match source {
        Source::Tree(digest) => Value::Map(BTreeMap::from([(KEY_TREE.to_owned(), digest_to_value(digest))])),
        Source::Output { action, path } => Value::Map(BTreeMap::from([
            (KEY_ACTION.to_owned(), Value::Text(action.clone())),
            (KEY_PATH.to_owned(), Value::Text(path.clone())),
        ])),
    }
}

pub(crate) fn source_from_value(value: Value, key: &'static str) -> Result<Source, CanonicalError> {
    let Value::Map(map) = value else {
        return Err(CanonicalError::Field(key));
    };
    let mut fields = Fields::new(map);
    let source = match fields.take_optional(KEY_TREE) {
        Some(tree) => Source::Tree(digest_from_value(&tree).ok_or(CanonicalError::Field(key))?),
        None => Source::Output { action: fields.text(KEY_ACTION)?, path: fields.text(KEY_PATH)? },
    };
    fields.finish()?;
    Ok(source)
}
