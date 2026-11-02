use std::error::Error as StdError;
use std::fmt;

/// An expression that does not follow the grammar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    pub expression: String,
    /// The 1-based character position of the problem.
    pub position: usize,
    pub message: String,
}

impl ParseError {
    pub(crate) fn new(expression: &str, position: usize, message: impl Into<String>) -> Self {
        Self { expression: expression.to_string(), position, message: message.into() }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid expression `{}`: {} at position {}", self.expression, self.message, self.position)
    }
}

impl StdError for ParseError {}

/// An expression that cannot be evaluated with the given context.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvalError {
    pub expression: Option<String>,
    pub message: String,
}

impl EvalError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self { expression: None, message: message.into() }
    }

    pub(crate) fn in_expression(mut self, expression: &str) -> Self {
        self.expression = Some(expression.to_string());
        self
    }
}

impl fmt::Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.expression {
            Some(expression) => write!(f, "cannot evaluate `{expression}`: {}", self.message),
            None => write!(f, "cannot evaluate: {}", self.message),
        }
    }
}

impl StdError for EvalError {}

/// A template with a `${{` that has no closing `}}`, or a condition that mixes text and expressions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateError {
    pub template: String,
    /// The 1-based byte position of the problem.
    pub position: usize,
    pub message: String,
}

impl fmt::Display for TemplateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid template `{}`: {} at position {}", self.template, self.message, self.position)
    }
}

impl StdError for TemplateError {}

/// Any error of this crate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Parse(ParseError),
    Eval(EvalError),
    Template(TemplateError),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(error) => error.fmt(f),
            Self::Eval(error) => error.fmt(f),
            Self::Template(error) => error.fmt(f),
        }
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Parse(error) => Some(error),
            Self::Eval(error) => Some(error),
            Self::Template(error) => Some(error),
        }
    }
}

impl From<ParseError> for Error {
    fn from(error: ParseError) -> Self {
        Self::Parse(error)
    }
}

impl From<EvalError> for Error {
    fn from(error: EvalError) -> Self {
        Self::Eval(error)
    }
}

impl From<TemplateError> for Error {
    fn from(error: TemplateError) -> Self {
        Self::Template(error)
    }
}
