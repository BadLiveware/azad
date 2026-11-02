use serde_json::Value;

use crate::expr::error::EvalError;
use crate::expr::functions::call;
use crate::expr::parser::{CompareOp, Expr, Node};
use crate::expr::value::{Relation, loosely_equal, relation_holds, string_to_number, truthy};

/// The state of a job; `success()`, `failure()` and `cancelled()` read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobStatus {
    Success,
    Failure,
    Cancelled,
}

/// The data that an expression reads.
pub trait Context {
    /// A named context such as `github`, `env`, `matrix`, `steps`, `needs`, `inputs`, `runner`, `job`, `strategy`,
    /// `secrets` or `vars`; `None` when this context does not have it, which is an evaluation error.
    /// The name is in lower case.
    fn context(&self, name: &str) -> Option<Value>;
    /// The status that `success()`, `failure()` and `cancelled()` read.
    fn job_status(&self) -> JobStatus;
    /// `hashFiles(...)`: the hex SHA-256 that GitHub's definition gives, or an error message.
    fn hash_files(&self, patterns: &[String]) -> Result<String, String>;
}

/// Evaluates an expression with a context.
/// `&&` and `||` evaluate their right side only when they need it.
pub fn evaluate(expr: &Expr, context: &dyn Context) -> Result<Value, EvalError> {
    let evaluator = Evaluator { context };
    evaluator.evaluate(&expr.root).map(Evaluated::into_value).map_err(|error| error.in_expression(&expr.source))
}

/// A result of a lookup. A `*` filter makes a list that later lookups apply to each item of.
enum Evaluated {
    Single(Value),
    Filtered(Vec<Value>),
}

impl Evaluated {
    fn into_value(self) -> Value {
        match self {
            Self::Single(value) => value,
            Self::Filtered(items) => Value::Array(items),
        }
    }
}

struct Evaluator<'a> {
    context: &'a dyn Context,
}

impl Evaluator<'_> {
    fn evaluate(&self, node: &Node) -> Result<Evaluated, EvalError> {
        match node {
            Node::Literal(value) => Ok(Evaluated::Single(value.clone())),
            Node::Context(name) => self.named_context(name),
            Node::Property(target, name) => {
                let key = Value::String(name.clone());
                Ok(select(self.evaluate(target)?, &key))
            }
            Node::Index(target, index) => {
                let key = self.value(index)?;
                Ok(select(self.evaluate(target)?, &key))
            }
            Node::Filter(target) => Ok(expand(self.evaluate(target)?)),
            Node::Not(operand) => Ok(single(Value::Bool(!truthy(&self.value(operand)?)))),
            Node::Compare(op, left, right) => {
                let result = compare(*op, &self.value(left)?, &self.value(right)?);
                Ok(single(Value::Bool(result)))
            }
            Node::And(left, right) => self.and(left, right),
            Node::Or(left, right) => self.or(left, right),
            Node::Call(name, arguments) => self.call(name, arguments),
        }
    }

    fn value(&self, node: &Node) -> Result<Value, EvalError> {
        self.evaluate(node).map(Evaluated::into_value)
    }

    fn named_context(&self, name: &str) -> Result<Evaluated, EvalError> {
        match self.context.context(name) {
            Some(value) => Ok(single(value)),
            None => Err(EvalError::new(format!("unknown context `{name}`"))),
        }
    }

    fn and(&self, left: &Node, right: &Node) -> Result<Evaluated, EvalError> {
        let first = self.value(left)?;
        match truthy(&first) {
            true => self.evaluate(right),
            false => Ok(single(first)),
        }
    }

    fn or(&self, left: &Node, right: &Node) -> Result<Evaluated, EvalError> {
        let first = self.value(left)?;
        match truthy(&first) {
            true => Ok(single(first)),
            false => self.evaluate(right),
        }
    }

    fn call(&self, name: &str, arguments: &[Node]) -> Result<Evaluated, EvalError> {
        let values = arguments.iter().map(|argument| self.value(argument)).collect::<Result<Vec<_>, _>>()?;
        call(name, &values, self.context).map(single)
    }
}

fn single(value: Value) -> Evaluated {
    Evaluated::Single(value)
}

fn compare(op: CompareOp, left: &Value, right: &Value) -> bool {
    match op {
        CompareOp::Equal => loosely_equal(left, right),
        CompareOp::NotEqual => !loosely_equal(left, right),
        CompareOp::Less => relation_holds(left, right, Relation::Less),
        CompareOp::LessOrEqual => relation_holds(left, right, Relation::LessOrEqual),
        CompareOp::Greater => relation_holds(left, right, Relation::Greater),
        CompareOp::GreaterOrEqual => relation_holds(left, right, Relation::GreaterOrEqual),
    }
}

fn select(target: Evaluated, key: &Value) -> Evaluated {
    match target {
        Evaluated::Single(value) => single(lookup(&value, key).unwrap_or(Value::Null)),
        Evaluated::Filtered(items) => Evaluated::Filtered(items.iter().filter_map(|item| lookup(item, key)).collect()),
    }
}

fn expand(target: Evaluated) -> Evaluated {
    match target {
        Evaluated::Single(value) => Evaluated::Filtered(children(&value)),
        Evaluated::Filtered(items) => Evaluated::Filtered(items.iter().flat_map(children).collect()),
    }
}

fn children(value: &Value) -> Vec<Value> {
    match value {
        Value::Array(items) => items.clone(),
        Value::Object(map) => map.values().cloned().collect(),
        _ => Vec::new(),
    }
}

fn lookup(container: &Value, key: &Value) -> Option<Value> {
    match (container, key) {
        (Value::Object(map), Value::String(name)) => map
            .get(name)
            .or_else(|| map.iter().find(|(candidate, _)| candidate.eq_ignore_ascii_case(name)).map(|(_, value)| value))
            .cloned(),
        (Value::Array(items), _) => array_position(key).and_then(|at| items.get(at)).cloned(),
        _ => None,
    }
}

fn array_position(key: &Value) -> Option<usize> {
    let number = match key {
        Value::Number(number) => number.as_f64()?,
        Value::String(text) => string_to_number(text),
        _ => return None,
    };
    let is_position = number.fract() == 0.0 && number >= 0.0 && number < usize::MAX as f64;
    match is_position {
        true => Some(number as usize),
        false => None,
    }
}

/// The names of the contexts that an expression reads, and which special functions it calls.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct References {
    pub contexts: std::collections::BTreeSet<String>,
    pub status_function: bool,
    pub hash_files: bool,
}
