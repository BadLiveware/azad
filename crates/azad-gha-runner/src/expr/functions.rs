use serde_json::Value;

use crate::expr::error::EvalError;
use crate::expr::eval::{Context, JobStatus};
use crate::expr::value::{loosely_equal, to_text};

pub(crate) const FN_CONTAINS: &str = "contains";
pub(crate) const FN_STARTS_WITH: &str = "startswith";
pub(crate) const FN_ENDS_WITH: &str = "endswith";
pub(crate) const FN_FORMAT: &str = "format";
pub(crate) const FN_JOIN: &str = "join";
pub(crate) const FN_TO_JSON: &str = "tojson";
pub(crate) const FN_FROM_JSON: &str = "fromjson";
pub(crate) const FN_HASH_FILES: &str = "hashfiles";
pub(crate) const FN_SUCCESS: &str = "success";
pub(crate) const FN_FAILURE: &str = "failure";
pub(crate) const FN_CANCELLED: &str = "cancelled";
pub(crate) const FN_ALWAYS: &str = "always";

const DEFAULT_JOIN_SEPARATOR: &str = ",";

struct Arity {
    min: usize,
    max: Option<usize>,
}

const fn exactly(count: usize) -> Arity {
    Arity { min: count, max: Some(count) }
}

fn arity_of(name: &str) -> Option<Arity> {
    match name {
        FN_CONTAINS | FN_STARTS_WITH | FN_ENDS_WITH => Some(exactly(2)),
        FN_FORMAT | FN_HASH_FILES => Some(Arity { min: 1, max: None }),
        FN_JOIN => Some(Arity { min: 1, max: Some(2) }),
        FN_TO_JSON | FN_FROM_JSON => Some(exactly(1)),
        FN_SUCCESS | FN_FAILURE | FN_CANCELLED | FN_ALWAYS => Some(exactly(0)),
        _ => None,
    }
}

fn check_arity(name: &str, arity: &Arity, given: usize) -> Result<(), EvalError> {
    let too_few = given < arity.min;
    let too_many = arity.max.is_some_and(|max| given > max);
    if !too_few && !too_many {
        return Ok(());
    }
    let expected = match arity.max {
        Some(max) if max == arity.min => format!("{max}"),
        Some(max) => format!("{} to {max}", arity.min),
        None => format!("at least {}", arity.min),
    };
    Err(EvalError::new(format!("function `{name}` takes {expected} argument(s), but {given} given")))
}

/// Calls a function by its lower case name.
pub(crate) fn call(name: &str, arguments: &[Value], context: &dyn Context) -> Result<Value, EvalError> {
    let Some(arity) = arity_of(name) else {
        return Err(EvalError::new(format!("unknown function `{name}`")));
    };
    check_arity(name, &arity, arguments.len())?;
    match name {
        FN_CONTAINS => Ok(Value::Bool(contains(&arguments[0], &arguments[1]))),
        FN_STARTS_WITH => Ok(Value::Bool(text_test(arguments, |a, b| a.starts_with(b)))),
        FN_ENDS_WITH => Ok(Value::Bool(text_test(arguments, |a, b| a.ends_with(b)))),
        FN_FORMAT => format(arguments).map(Value::String),
        FN_JOIN => Ok(Value::String(join(arguments))),
        FN_TO_JSON => to_json(&arguments[0]).map(Value::String),
        FN_FROM_JSON => from_json(&arguments[0]),
        FN_HASH_FILES => hash_files(arguments, context),
        status => Ok(Value::Bool(status_holds(status, context.job_status()))),
    }
}

fn status_holds(name: &str, status: JobStatus) -> bool {
    match name {
        FN_SUCCESS => status == JobStatus::Success,
        FN_FAILURE => status == JobStatus::Failure,
        FN_CANCELLED => status == JobStatus::Cancelled,
        _ => true,
    }
}

fn is_collection(value: &Value) -> bool {
    matches!(value, Value::Array(_) | Value::Object(_))
}

fn contains(search: &Value, item: &Value) -> bool {
    match search {
        Value::Array(items) => items.iter().any(|candidate| loosely_equal(candidate, item)),
        Value::Object(_) => false,
        _ if is_collection(item) => false,
        _ => to_text(search).to_lowercase().contains(&to_text(item).to_lowercase()),
    }
}

fn text_test(arguments: &[Value], test: impl Fn(&str, &str) -> bool) -> bool {
    if arguments.iter().any(is_collection) {
        return false;
    }
    test(&to_text(&arguments[0]).to_lowercase(), &to_text(&arguments[1]).to_lowercase())
}

fn format(arguments: &[Value]) -> Result<String, EvalError> {
    let template = to_text(&arguments[0]);
    let values = &arguments[1..];
    let mut output = String::new();
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                output.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                output.push('}');
            }
            '{' => output.push_str(&placeholder(&mut chars, values, &template)?),
            '}' => return Err(format_error(&template, "a `}` has no matching `{`")),
            other => output.push(other),
        }
    }
    Ok(output)
}

fn placeholder(chars: &mut impl Iterator<Item = char>, values: &[Value], template: &str) -> Result<String, EvalError> {
    let digits: String = chars.by_ref().take_while(|c| *c != '}').collect();
    let index: usize = digits.parse().map_err(|_| format_error(template, "a placeholder is not `{` number `}`"))?;
    match values.get(index) {
        Some(value) => Ok(to_text(value)),
        None => Err(format_error(template, &format!("placeholder {{{index}}} has no argument"))),
    }
}

fn format_error(template: &str, message: &str) -> EvalError {
    EvalError::new(format!("format string `{template}`: {message}"))
}

fn join(arguments: &[Value]) -> String {
    let separator = arguments.get(1).map_or_else(|| DEFAULT_JOIN_SEPARATOR.to_string(), to_text);
    match &arguments[0] {
        Value::Array(items) => items.iter().map(to_text).collect::<Vec<_>>().join(&separator),
        other => to_text(other),
    }
}

fn to_json(value: &Value) -> Result<String, EvalError> {
    serde_json::to_string_pretty(value).map_err(|error| EvalError::new(format!("toJSON failed: {error}")))
}

fn from_json(value: &Value) -> Result<Value, EvalError> {
    let text = to_text(value);
    serde_json::from_str(&text).map_err(|error| EvalError::new(format!("fromJSON cannot read `{text}`: {error}")))
}

fn hash_files(arguments: &[Value], context: &dyn Context) -> Result<Value, EvalError> {
    let patterns: Vec<String> = arguments.iter().map(to_text).collect();
    context
        .hash_files(&patterns)
        .map(Value::String)
        .map_err(|message| EvalError::new(format!("hashFiles failed: {message}")))
}
