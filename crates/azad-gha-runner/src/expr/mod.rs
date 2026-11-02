//! The GitHub Actions expression language: the `${{ ... }}` syntax.
//!
//! A frontend uses it to evaluate expressions at compile time.
//! A runner uses it inside a sandbox at run time.
//! The crate has one dependency: `serde_json`.

mod error;
mod eval;
mod functions;
mod lexer;
mod parser;
mod references;
mod template;
mod value;

#[cfg(test)]
mod tests;

pub use error::{Error, EvalError, ParseError, TemplateError};
pub use eval::{Context, JobStatus, References, evaluate};
pub use parser::{Expr, parse};
pub use references::references;
pub use serde_json::Value;
pub use template::{condition, evaluate_template, interpolate, template_expressions};
pub use value::{to_text, truthy};
