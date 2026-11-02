//! The text form of a graph, for tests, examples and reading. Nobody writes it for real work; frontends make graphs.
//!
//! ```text
//! action build
//!   kind hermetic
//!   platform linux/amd64
//!   root local("rootfs")
//!   command "sh" "-c" "mkdir out && cp /src/* out/"
//!   env LANG "C"
//!   input /src local("src")
//!   output out
//!
//! goal check build
//! ```
//!
//! A host action (decision 0015) has `kind host "<workspace>"`. `work <path> <source>` places a tree in the work
//! directory, and `dir "<path>"` names the directory in which the command starts. A host action that runs in an image
//! (decision 0024) has `container image "<reference>"`, or `container dockerfile "<file>" "<context>" ["<target>"]`
//! with `build-arg <name> <value>`, and `volume <path> <name>` for each cache volume. `secret <name>` names a variable
//! of `azad`'s environment that the command gets.
//!
//! A source is a digest, `output(<action>, "<path>")`, or `local("<path>")`. A local source is a directory relative to
//! the file; the caller stores it and replaces it with the digest of its tree.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write as _};

use crate::ir::action::{Container, ContainerImage, Kind, Source, Spec};
use crate::ir::digest::Digest;
use crate::ir::graph::Graph;

const INDENT: &str = "  ";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceSpec {
    Source(Source),
    Local(String),
}

/// A parsed text form, before local sources become digests.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Document {
    pub actions: BTreeMap<String, Spec<SourceSpec>>,
    pub goals: BTreeMap<String, BTreeSet<String>>,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("line {line}: {message}")]
pub struct ParseError {
    pub line: usize,
    pub message: String,
}

impl Document {
    /// Replaces each local source with the digest that `store_local` gives for its path.
    pub fn resolve<E>(self, mut store_local: impl FnMut(&str) -> Result<Digest, E>) -> Result<Graph, E> {
        let mut resolve = |spec: SourceSpec| match spec {
            SourceSpec::Source(source) => Ok(source),
            SourceSpec::Local(path) => store_local(&path).map(Source::Tree),
        };
        let mut actions = BTreeMap::new();
        for (name, action) in self.actions {
            let inputs = action
                .inputs
                .into_iter()
                .map(|(path, spec)| resolve(spec).map(|s| (path, s)))
                .collect::<Result<_, E>>()?;
            let work = action
                .work
                .into_iter()
                .map(|(path, spec)| resolve(spec).map(|s| (path, s)))
                .collect::<Result<_, E>>()?;
            let resolved = Spec {
                kind: action.kind,
                platform: action.platform,
                root: resolve(action.root)?,
                command: action.command,
                env: action.env,
                inputs,
                outputs: action.outputs,
                caches: action.caches,
                work,
                dir: action.dir,
                secrets: action.secrets,
            };
            actions.insert(name, resolved);
        }
        Ok(Graph { actions, goals: self.goals })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    Word(String),
    Text(String),
    Open,
    Close,
    Comma,
}

fn tokenize(line: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            ' ' | '\t' => {
                chars.next();
            }
            '#' => break,
            '(' | ')' | ',' => {
                chars.next();
                tokens.push(match c {
                    '(' => Token::Open,
                    ')' => Token::Close,
                    _ => Token::Comma,
                });
            }
            '"' => {
                chars.next();
                tokens.push(Token::Text(quoted(&mut chars)?));
            }
            _ => {
                let mut word = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_whitespace() || matches!(c, '(' | ')' | ',' | '"' | '#') {
                        break;
                    }
                    word.push(c);
                    chars.next();
                }
                tokens.push(Token::Word(word));
            }
        }
    }
    Ok(tokens)
}

fn quoted(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Result<String, String> {
    let mut text = String::new();
    loop {
        match chars.next() {
            None => return Err("a string has no closing quote".to_owned()),
            Some('"') => return Ok(text),
            Some('\\') => match chars.next() {
                Some('"') => text.push('"'),
                Some('\\') => text.push('\\'),
                Some('n') => text.push('\n'),
                Some('t') => text.push('\t'),
                other => {
                    return Err(format!("unknown escape \\{}", other.map(String::from).unwrap_or_default()));
                }
            },
            Some(c) => text.push(c),
        }
    }
}

struct ActionBuilder {
    name: String,
    line: usize,
    kind: Option<String>,
    expected: BTreeMap<String, Digest>,
    platform: Option<String>,
    root: Option<SourceSpec>,
    command: Vec<String>,
    env: BTreeMap<String, String>,
    inputs: BTreeMap<String, SourceSpec>,
    outputs: BTreeSet<String>,
    caches: BTreeMap<String, String>,
    workspace: Option<String>,
    work: BTreeMap<String, SourceSpec>,
    dir: Option<String>,
    container: Option<ContainerImage>,
    build_args: BTreeMap<String, String>,
    build_contexts: BTreeMap<String, String>,
    volumes: BTreeMap<String, String>,
    secrets: BTreeSet<String>,
}

impl ActionBuilder {
    fn new(name: String, line: usize) -> Self {
        ActionBuilder {
            name,
            line,
            kind: None,
            expected: BTreeMap::new(),
            platform: None,
            root: None,
            command: Vec::new(),
            env: BTreeMap::new(),
            inputs: BTreeMap::new(),
            outputs: BTreeSet::new(),
            caches: BTreeMap::new(),
            workspace: None,
            work: BTreeMap::new(),
            dir: None,
            container: None,
            build_args: BTreeMap::new(),
            build_contexts: BTreeMap::new(),
            volumes: BTreeMap::new(),
            secrets: BTreeSet::new(),
        }
    }

    fn field(&mut self, keyword: &str, args: &[Token]) -> Result<(), String> {
        match keyword {
            "kind" => match args {
                [kind, workspace] => {
                    set_once(&mut self.workspace, string(workspace)?, keyword)?;
                    set_once(&mut self.kind, string(kind)?, keyword)
                }
                _ => set_once(&mut self.kind, single_word(args)?, keyword),
            },
            "work" => {
                let [path, rest @ ..] = args else {
                    return Err("work takes a path and a source".to_owned());
                };
                insert_once(&mut self.work, string(path)?, source(rest)?, keyword)
            }
            "dir" => set_once(&mut self.dir, single_word(args)?, keyword),
            "platform" => set_once(&mut self.platform, single_word(args)?, keyword),
            "root" => set_once(&mut self.root, source(args)?, keyword),
            "command" => {
                if !self.command.is_empty() {
                    return Err("command is given twice".to_owned());
                }
                self.command = args.iter().map(string).collect::<Result<_, _>>()?;
                Ok(())
            }
            "env" => {
                let [key, value] = args else {
                    return Err("env takes a name and a value".to_owned());
                };
                insert_once(&mut self.env, string(key)?, string(value)?, keyword)
            }
            "input" => {
                let [path, rest @ ..] = args else {
                    return Err("input takes a path and a source".to_owned());
                };
                insert_once(&mut self.inputs, string(path)?, source(rest)?, keyword)
            }
            "output" => {
                let path = single_word(args)?;
                match self.outputs.insert(path.clone()) {
                    true => Ok(()),
                    false => Err(format!("output {path:?} is given twice")),
                }
            }
            "expect" => {
                let [path, digest] = args else {
                    return Err("expect takes an output path and a digest".to_owned());
                };
                let digest = string(digest)?.parse::<Digest>().map_err(|e| e.to_string())?;
                insert_once(&mut self.expected, string(path)?, digest, keyword)
            }
            "cache" => {
                let [path, name] = args else { return Err("cache takes a path and a cache name".to_owned()) };
                insert_once(&mut self.caches, string(path)?, string(name)?, keyword)
            }
            "container" => {
                let image = match args {
                    [kind, reference] if string(kind)? == "image" => ContainerImage::Reference(string(reference)?),
                    [kind, file, context, target @ ..] if string(kind)? == "dockerfile" && target.len() <= 1 => {
                        ContainerImage::Dockerfile {
                            file: string(file)?,
                            context: string(context)?,
                            target: target.first().map(string).transpose()?.unwrap_or_default(),
                            args: BTreeMap::new(),
                            contexts: BTreeMap::new(),
                        }
                    }
                    _ => return Err(
                        "container takes image \"<reference>\", or dockerfile \"<file>\" \"<context>\" and a target"
                            .to_owned(),
                    ),
                };
                set_once(&mut self.container, image, keyword)
            }
            "build-arg" => {
                let [name, value] = args else { return Err("build-arg takes a name and a value".to_owned()) };
                insert_once(&mut self.build_args, string(name)?, string(value)?, keyword)
            }
            "build-context" => {
                let [name, path] = args else { return Err("build-context takes a name and a path".to_owned()) };
                insert_once(&mut self.build_contexts, string(name)?, string(path)?, keyword)
            }
            "volume" => {
                let [path, name] = args else { return Err("volume takes a path and a cache name".to_owned()) };
                insert_once(&mut self.volumes, string(path)?, string(name)?, keyword)
            }
            "secret" => {
                let name = single_word(args)?;
                match self.secrets.insert(name.clone()) {
                    true => Ok(()),
                    false => Err(format!("secret {name:?} is given twice")),
                }
            }
            other => Err(format!("unknown field {other:?}")),
        }
    }

    /// The container of a host action, from `container`, `build-arg` and `volume`.
    fn container(&self) -> Result<Option<Container>, String> {
        let Some(image) = self.container.clone() else {
            return match self.build_args.is_empty() && self.build_contexts.is_empty() && self.volumes.is_empty() {
                true => Ok(None),
                false => Err("build-arg, build-context and volume need a container".to_owned()),
            };
        };
        let image = match image {
            ContainerImage::Dockerfile { file, context, target, .. } => ContainerImage::Dockerfile {
                file,
                context,
                target,
                args: self.build_args.clone(),
                contexts: self.build_contexts.clone(),
            },
            ContainerImage::Reference(_) if !self.build_args.is_empty() || !self.build_contexts.is_empty() => {
                return Err("build-arg and build-context need a container from a dockerfile".to_owned());
            }
            reference => reference,
        };
        Ok(Some(Container { image, volumes: self.volumes.clone() }))
    }

    fn build(self) -> Result<(String, Spec<SourceSpec>), ParseError> {
        let error = |message: &str| ParseError { line: self.line, message: format!("action {}: {message}", self.name) };
        let kind = match self.kind.as_deref().unwrap_or("hermetic") {
            "hermetic" => Kind::Hermetic,
            "impure" => Kind::Impure,
            "fetch" => Kind::Fetch,
            "fixed-output" => Kind::FixedOutput { expected: self.expected.clone() },
            "host" => Kind::Host {
                workspace: self.workspace.clone().ok_or_else(|| error("a host action names its workspace"))?,
                container: self.container().map_err(|message| error(&message))?,
            },
            other => return Err(error(&format!("unknown kind {other:?}"))),
        };
        if self.container.is_some() && !matches!(kind, Kind::Host { .. }) {
            return Err(error("only a host action has a container"));
        }
        if self.workspace.is_some() && !matches!(kind, Kind::Host { .. }) {
            return Err(error("only a host action names a workspace"));
        }
        if !matches!(kind, Kind::FixedOutput { .. }) && !self.expected.is_empty() {
            return Err(error("only a fixed-output action can expect digests"));
        }
        let platform = self.platform.clone().ok_or_else(|| error("platform is missing"))?;
        let root = self.root.clone().ok_or_else(|| error("root is missing"))?;
        let spec = Spec {
            kind,
            platform,
            root,
            command: self.command,
            env: self.env,
            inputs: self.inputs,
            outputs: self.outputs,
            caches: self.caches,
            work: self.work,
            dir: self.dir.unwrap_or_default(),
            secrets: self.secrets,
        };
        Ok((self.name, spec))
    }
}

fn set_once<T>(slot: &mut Option<T>, value: T, keyword: &str) -> Result<(), String> {
    match slot.replace(value) {
        None => Ok(()),
        Some(_) => Err(format!("{keyword} is given twice")),
    }
}

fn insert_once<V>(map: &mut BTreeMap<String, V>, key: String, value: V, keyword: &str) -> Result<(), String> {
    match map.insert(key.clone(), value) {
        None => Ok(()),
        Some(_) => Err(format!("{keyword} {key:?} is given twice")),
    }
}

fn string(token: &Token) -> Result<String, String> {
    match token {
        Token::Word(w) | Token::Text(w) => Ok(w.clone()),
        _ => Err("expected a word or a string".to_owned()),
    }
}

fn single_word(args: &[Token]) -> Result<String, String> {
    match args {
        [token] => string(token),
        _ => Err("expected one value".to_owned()),
    }
}

fn source(args: &[Token]) -> Result<SourceSpec, String> {
    match args {
        [Token::Word(w)] => w.parse::<Digest>().map(|d| SourceSpec::Source(Source::Tree(d))).map_err(|e| e.to_string()),
        [Token::Word(f), Token::Open, Token::Text(path), Token::Close] if f == "local" => {
            Ok(SourceSpec::Local(path.clone()))
        }
        [Token::Word(f), Token::Open, action, Token::Comma, Token::Text(path), Token::Close] if f == "output" => {
            Ok(SourceSpec::Source(Source::Output { action: string(action)?, path: path.clone() }))
        }
        _ => Err("a source is a digest, output(<action>, \"<path>\") or local(\"<path>\")".to_owned()),
    }
}

pub fn parse(text: &str) -> Result<Document, ParseError> {
    let mut document = Document::default();
    let mut current: Option<ActionBuilder> = None;
    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        let error = |message: String| ParseError { line, message };
        let tokens = tokenize(raw).map_err(error)?;
        let Some((first, args)) = tokens.split_first() else {
            continue;
        };
        let keyword = string(first).map_err(error)?;
        if raw.starts_with(char::is_whitespace) {
            let action = current.as_mut().ok_or_else(|| error("a field outside an action".to_owned()))?;
            action.field(&keyword, args).map_err(error)?;
            continue;
        }
        if let Some(done) = current.take() {
            add_action(&mut document, done)?;
        }
        match keyword.as_str() {
            "action" => current = Some(ActionBuilder::new(single_word(args).map_err(error)?, line)),
            "goal" => {
                let [name, actions @ ..] = args else {
                    return Err(error("goal takes a name and actions".to_owned()));
                };
                let actions = actions.iter().map(string).collect::<Result<_, _>>().map_err(error)?;
                insert_once(&mut document.goals, string(name).map_err(error)?, actions, "goal").map_err(error)?;
            }
            other => return Err(error(format!("unknown statement {other:?}"))),
        }
    }
    if let Some(done) = current {
        add_action(&mut document, done)?;
    }
    Ok(document)
}

fn add_action(document: &mut Document, builder: ActionBuilder) -> Result<(), ParseError> {
    let line = builder.line;
    let (name, spec) = builder.build()?;
    insert_once(&mut document.actions, name, spec, "action").map_err(|message| ParseError { line, message })
}

/// Prints a graph in the text form; `parse` gives back the same graph.
pub fn print(graph: &Graph) -> String {
    let mut out = String::new();
    for (name, action) in &graph.actions {
        writeln!(out, "action {name}").ok();
        match &action.kind {
            Kind::Host { workspace, .. } => {
                writeln!(out, "{INDENT}kind {} {}", action.kind.name(), Quoted(workspace)).ok()
            }
            kind => writeln!(out, "{INDENT}kind {}", kind.name()).ok(),
        };
        if let Kind::Host { container: Some(container), .. } = &action.kind {
            print_container(&mut out, container);
        }
        for name in &action.secrets {
            writeln!(out, "{INDENT}secret {}", Quoted(name)).ok();
        }
        if let Kind::FixedOutput { expected } = &action.kind {
            for (path, digest) in expected {
                writeln!(out, "{INDENT}expect {} {digest}", Quoted(path)).ok();
            }
        }
        writeln!(out, "{INDENT}platform {}", Quoted(&action.platform)).ok();
        writeln!(out, "{INDENT}root {}", PrintSource(&action.root)).ok();
        let command: Vec<String> = action.command.iter().map(|c| Quoted(c).to_string()).collect();
        writeln!(out, "{INDENT}command {}", command.join(" ")).ok();
        for (key, value) in &action.env {
            writeln!(out, "{INDENT}env {} {}", Quoted(key), Quoted(value)).ok();
        }
        for (path, source) in &action.inputs {
            writeln!(out, "{INDENT}input {} {}", Quoted(path), PrintSource(source)).ok();
        }
        for (path, name) in &action.caches {
            writeln!(out, "{INDENT}cache {} {}", Quoted(path), Quoted(name)).ok();
        }
        for (path, source) in &action.work {
            writeln!(out, "{INDENT}work {} {}", Quoted(path), PrintSource(source)).ok();
        }
        if !action.dir.is_empty() {
            writeln!(out, "{INDENT}dir {}", Quoted(&action.dir)).ok();
        }
        for path in &action.outputs {
            writeln!(out, "{INDENT}output {}", Quoted(path)).ok();
        }
        out.push('\n');
    }
    for (name, actions) in &graph.goals {
        let actions: Vec<&str> = actions.iter().map(String::as_str).collect();
        writeln!(out, "goal {name} {}", actions.join(" ")).ok();
    }
    out
}

struct Quoted<'a>(&'a str);

impl fmt::Display for Quoted<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_char('"')?;
        for c in self.0.chars() {
            match c {
                '"' => f.write_str("\\\"")?,
                '\\' => f.write_str("\\\\")?,
                '\n' => f.write_str("\\n")?,
                '\t' => f.write_str("\\t")?,
                c => f.write_char(c)?,
            }
        }
        f.write_char('"')
    }
}

struct PrintSource<'a>(&'a Source);

impl fmt::Display for PrintSource<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Source::Tree(digest) => write!(f, "{digest}"),
            Source::Output { action, path } => write!(f, "output({action}, {})", Quoted(path)),
        }
    }
}

fn print_container(out: &mut String, container: &Container) {
    match &container.image {
        ContainerImage::Reference(reference) => writeln!(out, "{INDENT}container image {}", Quoted(reference)).ok(),
        ContainerImage::Dockerfile { file, context, target, args, contexts } => {
            let target = match target.is_empty() {
                true => String::new(),
                false => format!(" {}", Quoted(target)),
            };
            writeln!(out, "{INDENT}container dockerfile {} {}{target}", Quoted(file), Quoted(context)).ok();
            for (name, value) in args {
                writeln!(out, "{INDENT}build-arg {} {}", Quoted(name), Quoted(value)).ok();
            }
            for (name, path) in contexts {
                writeln!(out, "{INDENT}build-context {} {}", Quoted(name), Quoted(path)).ok();
            }
            Some(())
        }
    };
    for (path, name) in &container.volumes {
        writeln!(out, "{INDENT}volume {} {}", Quoted(path), Quoted(name)).ok();
    }
}
