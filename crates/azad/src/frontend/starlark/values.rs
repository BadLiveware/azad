//! The values that Azad's built-in functions make: sources, and actions whose outputs other actions can read; and the
//! collector of what a package defines.
//!
//! The derive macros of the `starlark` crate (`ProvidesStaticType`) generate `unsafe impl` blocks, so this module allows
//! `unsafe_code`; it has no `unsafe` code of its own.
#![allow(unsafe_code)]

use std::cell::RefCell;
use std::fmt;

use allocative::Allocative;
use starlark::any::ProvidesStaticType;
use starlark::coerce::Coerce;
use starlark::environment::{Methods, MethodsBuilder};
use starlark::eval::{Arguments, Evaluator};
use starlark::values::list::ListRef;
use starlark::values::{
    Freeze, Heap, NoSerialize, StarlarkValue, Trace, Value, ValueLifetimeless, ValueLike, starlark_value,
};
use starlark::{StarlarkPagablePanic, starlark_complex_value, starlark_module, starlark_simple_value};

use crate::frontend::{Package, SourceSpec};

/// A source that an action can mount: files of the package, an image, or an output of another action.
#[derive(Debug, Clone, PartialEq, Eq, ProvidesStaticType, NoSerialize, Allocative, StarlarkPagablePanic)]
pub struct SourceValue {
    #[allocative(skip)]
    pub(crate) spec: SourceSpec,
}

starlark_simple_value!(SourceValue);

impl fmt::Display for SourceValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.spec)
    }
}

#[starlark_value(type = "source")]
impl<'v> StarlarkValue<'v> for SourceValue {}

/// An action, as Starlark sees it: its label and its declared outputs.
#[derive(Debug, Clone, PartialEq, Eq, ProvidesStaticType, NoSerialize, Allocative, StarlarkPagablePanic)]
pub struct ActionValue {
    pub(crate) label: String,
    pub(crate) outputs: Vec<String>,
}

starlark_simple_value!(ActionValue);

impl fmt::Display for ActionValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "action({})", self.label)
    }
}

#[starlark_value(type = "action")]
impl<'v> StarlarkValue<'v> for ActionValue {
    fn get_methods() -> Option<&'static Methods> {
        Some(ACTION_METHODS.methods())
    }
}

starlark::methods_static!(ACTION_METHODS = action_methods);

#[starlark_module]
fn action_methods(builder: &mut MethodsBuilder) {
    /// The output directory `path` of this action, as a source for another action.
    ///
    /// An action that has it as an input runs after this action.
    ///
    /// # Arguments
    /// * `path`: One of the outputs of the action, such as `bin`.
    fn out(this: &ActionValue, path: &str) -> anyhow::Result<SourceValue> {
        if !this.outputs.iter().any(|o| o == path) {
            anyhow::bail!("{} does not declare the output {path:?}; it declares {:?}", this.label, this.outputs);
        }
        Ok(SourceValue { spec: SourceSpec::Output { action: this.label.clone(), path: path.to_owned() } })
    }

    /// The label of the action, such as `//services/orders:build`.
    #[starlark(attribute)]
    fn label(this: &ActionValue) -> anyhow::Result<String> {
        Ok(this.label.clone())
    }
}

/// Collects the actions and goals of the package that is evaluated. Only `tasks.star` evaluation has one, so a module
/// can define functions that declare actions, but cannot declare actions when it is loaded.
#[derive(Debug, ProvidesStaticType)]
pub(crate) struct Collector {
    /// The workspace root, which `files()` lists.
    pub(crate) root: std::path::PathBuf,
    pub(crate) package: String,
    /// The directory that `glob()` and `files()` read, relative to the root: the package's directory, or the
    /// directory of a call.
    pub(crate) dir: String,
    pub(crate) platform: String,
    pub(crate) defined: RefCell<Package>,
    /// Where a local run runs a task that does not say: `local` of the nearest `config.star` (decision 0024).
    pub(crate) default_local: crate::frontend::LocalRun,
    /// The tags that the `config.star` files from the root down to the package give every task and action of it.
    pub(crate) default_tags: std::collections::BTreeSet<String>,
    /// A source of the history that a shallow clone lacks, for `git` (decision 0021).
    pub(crate) history: Option<std::sync::Arc<dyn crate::history::History>>,
    /// Whether the file called `package()`, which it may do once (decision 0028).
    pub(crate) declared_package: std::cell::Cell<bool>,
}

/// The type of a module parameter (decision 0014). Callers that have only text, such as GHA `with:` inputs, can give
/// text for every type: words for lists, digits for `int`, and `true` or `false` for `bool`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParamType {
    String,
    Strings,
    Int,
    Bool,
    /// Patterns of files below the package, such as `tests/**/*.sh`; one pattern is a list of one.
    Patterns,
    /// An image, from `image()` or a value of `config.star`; text is a reference, as `image()` takes.
    Image,
}

impl ParamType {
    const OPTIONAL_SUFFIX: char = '?';

    /// A type such as `patterns`, and `patterns?` for an optional parameter.
    fn parse(text: &str) -> anyhow::Result<(Self, bool)> {
        let (name, required) = match text.strip_suffix(Self::OPTIONAL_SUFFIX) {
            Some(name) => (name, false),
            None => (text, true),
        };
        let kind = match name {
            "string" => ParamType::String,
            "strings" => ParamType::Strings,
            "int" => ParamType::Int,
            "bool" => ParamType::Bool,
            "patterns" => ParamType::Patterns,
            "image" => ParamType::Image,
            other => anyhow::bail!("{other:?} is not a parameter type: string, strings, int, bool, patterns or image"),
        };
        Ok((kind, required))
    }

    /// The value, checked, and as the implementation gets it.
    fn check<'v>(self, value: Value<'v>, heap: Heap<'v>) -> anyhow::Result<Value<'v>> {
        let text = value.unpack_str();
        match (self, text) {
            (ParamType::String, Some(_)) => Ok(value),
            (ParamType::Strings | ParamType::Patterns, Some(text)) => {
                Ok(heap.alloc(text.split_whitespace().map(str::to_owned).collect::<Vec<_>>()))
            }
            (ParamType::Int, Some(text)) => Ok(heap.alloc(text.trim().parse::<i32>()?)),
            (ParamType::Bool, Some(text)) => Ok(Value::new_bool(text.trim().parse::<bool>()?)),
            (ParamType::Image, Some(text)) => {
                let spec = SourceSpec::Image { reference: text.trim().to_owned(), env: Default::default() };
                Ok(heap.alloc(SourceValue { spec }))
            }
            (ParamType::Image, None) => match value.downcast_ref::<SourceValue>() {
                Some(source) if matches!(source.spec, SourceSpec::Image { .. } | SourceSpec::Dockerfile { .. }) => {
                    Ok(value)
                }
                _ => anyhow::bail!("an image is needed, not {}", value.get_type()),
            },
            (ParamType::Strings | ParamType::Patterns, None) => match ListRef::from_value(value) {
                Some(list) if list.iter().all(|item| item.unpack_str().is_some()) => Ok(value),
                _ => anyhow::bail!("a string or a list of strings is needed, not {}", value.get_type()),
            },
            (ParamType::Int, None) if value.unpack_i32().is_some() => Ok(value),
            (ParamType::Bool, None) if value.unpack_bool().is_some() => Ok(value),
            (ParamType::Int, None) => anyhow::bail!("an int is needed, not {}", value.get_type()),
            (ParamType::Bool, None) => anyhow::bail!("a bool is needed, not {}", value.get_type()),
            (ParamType::String, None) => anyhow::bail!("a string is needed, not {}", value.get_type()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Param {
    pub(crate) name: String,
    pub(crate) kind: ParamType,
    pub(crate) required: bool,
}

/// What a module says about itself: the parameters that a call gives, what a call gives back, and the outputs of the
/// action that the call names, which callers can read without knowing the module's other actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModuleSchema {
    pub(crate) name: String,
    pub(crate) params: Vec<Param>,
    pub(crate) outputs: Vec<String>,
    pub(crate) returns: Returns,
}

/// What a call of a module gives back (decision 0027).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Returns {
    /// The action that the call names, as decision 0014 made modules.
    Action,
    /// Parts for `project()` of `@std//project.star`, which a call without a name gives.
    Parts,
}

impl Returns {
    pub(crate) fn parse(text: &str) -> anyhow::Result<Self> {
        match text {
            "action" => Ok(Returns::Action),
            "parts" => Ok(Returns::Parts),
            other => anyhow::bail!("returns is \"action\" or \"parts\", not {other:?}"),
        }
    }
}

/// Every call has a name; the module names its actions with it, and the action with exactly this name is the call's.
pub(crate) const CALL_NAME_PARAM: &str = "name";

impl ModuleSchema {
    pub(crate) fn new(
        name: &str,
        params: Vec<(String, String)>,
        outputs: Vec<String>,
        returns: Returns,
    ) -> anyhow::Result<Self> {
        if returns == Returns::Parts && !outputs.is_empty() {
            anyhow::bail!("module {name}: a module that returns parts names no action, so it has no outputs");
        }
        let params = params
            .into_iter()
            .map(|(param, text)| {
                if param == CALL_NAME_PARAM {
                    anyhow::bail!("module {name}: {CALL_NAME_PARAM:?} is the call's name; it is not declared");
                }
                let (kind, required) =
                    ParamType::parse(&text).map_err(|e| anyhow::anyhow!("module {name}: {param}: {e}"))?;
                Ok(Param { name: param, kind, required })
            })
            .collect::<anyhow::Result<_>>()?;
        Ok(ModuleSchema { name: name.to_owned(), params, outputs, returns })
    }

    /// The named arguments of a call, checked against the parameters, as the implementation gets them.
    fn check<'v>(&self, args: Vec<(String, Value<'v>)>, heap: Heap<'v>) -> anyhow::Result<Vec<(String, Value<'v>)>> {
        let mut checked = Vec::new();
        for (name, value) in args {
            if name == CALL_NAME_PARAM && self.returns == Returns::Parts {
                anyhow::bail!("{}: a module that returns parts takes no {CALL_NAME_PARAM}", self.name);
            }
            if name == CALL_NAME_PARAM {
                if value.unpack_str().is_none() {
                    anyhow::bail!("{}: {CALL_NAME_PARAM} must be a string", self.name);
                }
                checked.push((name, value));
                continue;
            }
            let Some(param) = self.params.iter().find(|p| p.name == name) else {
                let known: Vec<&str> = self.params.iter().map(|p| p.name.as_str()).collect();
                anyhow::bail!("{} has no parameter {name:?}; it has {known:?}", self.name);
            };
            let value = param.kind.check(value, heap).map_err(|e| anyhow::anyhow!("{}: {name}: {e}", self.name))?;
            checked.push((name, value));
        }
        let given = |name: &str| checked.iter().any(|(n, _)| n == name);
        if self.returns == Returns::Action && !given(CALL_NAME_PARAM) {
            anyhow::bail!("{}: a call needs a {CALL_NAME_PARAM}", self.name);
        }
        if let Some(missing) = self.params.iter().find(|p| p.required && !given(&p.name)) {
            anyhow::bail!("{}: {} is required", self.name, missing.name);
        }
        Ok(checked)
    }
}

/// A module (decision 0014): a function with a schema. Calling it checks the arguments, runs the implementation, and
/// checks that the call's action declares the module's outputs.
#[derive(Debug, Trace, Freeze, Coerce, ProvidesStaticType, NoSerialize, Allocative)]
#[repr(C)]
pub(crate) struct ModuleDefGen<V: ValueLifetimeless> {
    #[trace(static)]
    #[freeze(identity)]
    #[allocative(skip)]
    pub(crate) schema: ModuleSchema,
    pub(crate) implementation: V,
}

starlark_complex_value!(pub(crate) ModuleDef);

impl<V: ValueLifetimeless> fmt::Display for ModuleDefGen<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "module({})", self.schema.name)
    }
}

#[starlark_value(type = "module")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for ModuleDefGen<V>
where
    Self: ProvidesStaticType<'v>,
{
    fn invoke(
        &self,
        _me: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        args.no_positional_args(eval.heap())?;
        let named: Vec<(String, Value<'v>)> =
            args.names_map()?.iter().map(|(name, value)| (name.as_str().to_owned(), *value)).collect();
        let checked = self.schema.check(named, eval.heap()).map_err(starlark::Error::new_other)?;
        let call_name = checked
            .iter()
            .find(|(name, _)| name == CALL_NAME_PARAM)
            .and_then(|(_, value)| value.unpack_str())
            .unwrap_or_default()
            .to_owned();
        let pairs: Vec<(&str, Value<'v>)> = checked.iter().map(|(name, value)| (name.as_str(), *value)).collect();
        let result = eval.eval_function(self.implementation.to_value(), &[], &pairs)?;
        if self.schema.returns == Returns::Action {
            self.check_call(&call_name, eval).map_err(starlark::Error::new_other)?;
        }
        Ok(result)
    }
}

impl<V: ValueLifetimeless> ModuleDefGen<V> {
    /// The call defined an action with the call's name, and it declares the module's outputs.
    fn check_call(&self, call_name: &str, eval: &Evaluator<'_, '_, '_>) -> anyhow::Result<()> {
        let Some(collector) = eval.extra.and_then(|extra| extra.downcast_ref::<Collector>()) else {
            anyhow::bail!("{} can be called only while a tasks.star file is evaluated", self.schema.name);
        };
        let call_label = crate::frontend::label(&collector.package, call_name);
        let defined = collector.defined.borrow();
        let Some(action) = defined.actions.get(&call_label) else {
            anyhow::bail!("{}: the call {call_label} defines no action with its name", self.schema.name);
        };
        if let Some(missing) = self.schema.outputs.iter().find(|output| !action.outputs.contains(*output)) {
            anyhow::bail!("{}: {call_label} does not declare the module's output {missing:?}", self.schema.name);
        }
        Ok(())
    }
}
