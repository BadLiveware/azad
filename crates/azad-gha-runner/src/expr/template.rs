use serde_json::Value;

use crate::expr::error::{Error, TemplateError};
use crate::expr::eval::{Context, JobStatus, evaluate};
use crate::expr::parser::{Expr, parse};
use crate::expr::references::references;
use crate::expr::value::{to_text, truthy};

const OPEN: &str = "${{";
const CLOSE: &str = "}}";
const QUOTE: char = '\'';

#[derive(Debug)]
enum Segment {
    Text(String),
    Expression(Expr),
}

fn template_error(template: &str, position: usize, message: &str) -> Error {
    Error::Template(TemplateError { template: template.to_string(), position, message: message.to_string() })
}

/// The byte position of the `}}` that ends an expression; a `}}` inside a string literal does not count.
fn find_close(text: &str) -> Option<usize> {
    let mut in_string = false;
    for (position, c) in text.char_indices() {
        if c == QUOTE {
            in_string = !in_string;
        }
        if !in_string && text[position..].starts_with(CLOSE) {
            return Some(position);
        }
    }
    None
}

fn split(template: &str) -> Result<Vec<Segment>, Error> {
    let mut segments = Vec::new();
    let mut rest = template;
    while let Some(open) = rest.find(OPEN) {
        if open > 0 {
            segments.push(Segment::Text(rest[..open].to_string()));
        }
        let inside = &rest[open + OPEN.len()..];
        let Some(close) = find_close(inside) else {
            let position = template.len() - inside.len() - OPEN.len() + 1;
            return Err(template_error(template, position, "`${{` has no closing `}}`"));
        };
        segments.push(Segment::Expression(parse(&inside[..close])?));
        rest = &inside[close + CLOSE.len()..];
    }
    if !rest.is_empty() {
        segments.push(Segment::Text(rest.to_string()));
    }
    Ok(segments)
}

fn interpolate_segments(segments: &[Segment], context: &dyn Context) -> Result<String, Error> {
    let mut output = String::new();
    for segment in segments {
        match segment {
            Segment::Text(text) => output.push_str(text),
            Segment::Expression(expr) => output.push_str(&to_text(&evaluate(expr, context)?)),
        }
    }
    Ok(output)
}

/// Replaces each `${{ ... }}` in `template` with its value as text; text outside the delimiters is unchanged.
pub fn interpolate(template: &str, context: &dyn Context) -> Result<String, Error> {
    interpolate_segments(&split(template)?, context)
}

/// The value of a template that is exactly one `${{ ... }}` (keeps its type), or `interpolate` otherwise
/// (a string). GitHub uses this for values such as `strategy.matrix` and `with:` inputs.
pub fn evaluate_template(template: &str, context: &dyn Context) -> Result<Value, Error> {
    let segments = split(template)?;
    if let [Segment::Expression(expr)] = segments.as_slice() {
        return Ok(evaluate(expr, context)?);
    }
    interpolate_segments(&segments, context).map(Value::String)
}

/// The `${{ }}` parts of a template, for the same purpose.
pub fn template_expressions(template: &str) -> Result<Vec<Expr>, Error> {
    let expressions = split(template)?
        .into_iter()
        .filter_map(|segment| match segment {
            Segment::Expression(expr) => Some(expr),
            Segment::Text(_) => None,
        })
        .collect();
    Ok(expressions)
}

fn condition_expression(source: &str) -> Result<Expr, Error> {
    if !source.contains(OPEN) {
        return Ok(parse(source)?);
    }
    let mut segments = split(source)?;
    match (segments.pop(), segments.is_empty()) {
        (Some(Segment::Expression(expr)), true) => Ok(expr),
        _ => Err(template_error(source, 1, "a condition is one expression, or text without `${{ }}`")),
    }
}

/// An `if:` condition: the `${{ }}` delimiters are optional, and when the expression calls none of `success()`,
/// `failure()`, `cancelled()` and `always()`, it is `success() && (<expression>)`.
pub fn condition(source: &str, context: &dyn Context) -> Result<bool, Error> {
    let expr = condition_expression(source.trim())?;
    let needs_success = !references(&expr).status_function;
    if needs_success && context.job_status() != JobStatus::Success {
        return Ok(false);
    }
    Ok(truthy(&evaluate(&expr, context)?))
}
