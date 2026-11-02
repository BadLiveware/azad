use serde_json::Value;

use crate::expr::error::ParseError;
use crate::expr::lexer::{Spanned, Token, tokenize};
use crate::expr::value::number_value;

const MAX_DEPTH: usize = 50;

/// A parsed expression, without the `${{ }}` delimiters.
#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    pub(crate) source: String,
    pub(crate) root: Node,
}

impl Expr {
    /// The text that was parsed.
    pub fn source(&self) -> &str {
        &self.source
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompareOp {
    Equal,
    NotEqual,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Node {
    Literal(Value),
    Context(String),
    Property(Box<Node>, String),
    Index(Box<Node>, Box<Node>),
    Filter(Box<Node>),
    Not(Box<Node>),
    Compare(CompareOp, Box<Node>, Box<Node>),
    And(Box<Node>, Box<Node>),
    Or(Box<Node>, Box<Node>),
    Call(String, Vec<Node>),
}

/// Parses an expression without the `${{ }}` delimiters.
pub fn parse(source: &str) -> Result<Expr, ParseError> {
    let tokens = tokenize(source)?;
    let mut parser = Parser { source, tokens, index: 0, depth: 0 };
    let root = parser.expression()?;
    if let Some(extra) = parser.tokens.get(parser.index) {
        return Err(parser.error(extra.position, "unexpected token after the expression"));
    }
    Ok(Expr { source: source.to_string(), root })
}

struct Parser<'a> {
    source: &'a str,
    tokens: Vec<Spanned>,
    index: usize,
    depth: usize,
}

impl Parser<'_> {
    fn error(&self, position: usize, message: impl Into<String>) -> ParseError {
        ParseError::new(self.source, position, message)
    }

    fn end_position(&self) -> usize {
        self.source.chars().count() + 1
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.index).map(|spanned| &spanned.token)
    }

    fn advance(&mut self) -> Result<Spanned, ParseError> {
        let Some(spanned) = self.tokens.get(self.index).cloned() else {
            return Err(self.error(self.end_position(), "the expression ends too early"));
        };
        self.index += 1;
        Ok(spanned)
    }

    fn accept(&mut self, token: &Token) -> bool {
        if self.peek() != Some(token) {
            return false;
        }
        self.index += 1;
        true
    }

    fn expect(&mut self, token: &Token, description: &str) -> Result<(), ParseError> {
        let position = self.tokens.get(self.index).map_or(self.end_position(), |spanned| spanned.position);
        match self.accept(token) {
            true => Ok(()),
            false => Err(self.error(position, format!("expected {description}"))),
        }
    }

    fn nested<T>(&mut self, parse: impl FnOnce(&mut Self) -> Result<T, ParseError>) -> Result<T, ParseError> {
        if self.depth >= MAX_DEPTH {
            let position = self.tokens.get(self.index).map_or(self.end_position(), |spanned| spanned.position);
            return Err(self.error(position, "the expression is nested too deeply"));
        }
        self.depth += 1;
        let result = parse(self);
        self.depth -= 1;
        result
    }

    fn expression(&mut self) -> Result<Node, ParseError> {
        self.or()
    }

    fn or(&mut self) -> Result<Node, ParseError> {
        let mut left = self.and()?;
        while self.accept(&Token::Or) {
            let right = self.and()?;
            left = Node::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn and(&mut self) -> Result<Node, ParseError> {
        let mut left = self.equality()?;
        while self.accept(&Token::And) {
            let right = self.equality()?;
            left = Node::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn equality(&mut self) -> Result<Node, ParseError> {
        let mut left = self.relation()?;
        loop {
            let op = match self.peek() {
                Some(Token::Equal) => CompareOp::Equal,
                Some(Token::NotEqual) => CompareOp::NotEqual,
                _ => return Ok(left),
            };
            self.index += 1;
            let right = self.relation()?;
            left = Node::Compare(op, Box::new(left), Box::new(right));
        }
    }

    fn relation(&mut self) -> Result<Node, ParseError> {
        let mut left = self.unary()?;
        loop {
            let op = match self.peek() {
                Some(Token::Less) => CompareOp::Less,
                Some(Token::LessOrEqual) => CompareOp::LessOrEqual,
                Some(Token::Greater) => CompareOp::Greater,
                Some(Token::GreaterOrEqual) => CompareOp::GreaterOrEqual,
                _ => return Ok(left),
            };
            self.index += 1;
            let right = self.unary()?;
            left = Node::Compare(op, Box::new(left), Box::new(right));
        }
    }

    fn unary(&mut self) -> Result<Node, ParseError> {
        if self.accept(&Token::Not) {
            let operand = self.nested(Self::unary)?;
            return Ok(Node::Not(Box::new(operand)));
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Result<Node, ParseError> {
        let mut node = self.primary()?;
        loop {
            node = match self.peek() {
                Some(Token::Dot) => {
                    self.index += 1;
                    self.after_dot(node)?
                }
                Some(Token::LeftBracket) => {
                    self.index += 1;
                    self.bracket(node)?
                }
                _ => return Ok(node),
            };
        }
    }

    fn after_dot(&mut self, target: Node) -> Result<Node, ParseError> {
        let spanned = self.advance()?;
        match spanned.token {
            Token::Star => Ok(Node::Filter(Box::new(target))),
            Token::Name(name) => Ok(Node::Property(Box::new(target), name)),
            _ => Err(self.error(spanned.position, "expected a property name or `*` after `.`")),
        }
    }

    fn bracket(&mut self, target: Node) -> Result<Node, ParseError> {
        if self.peek() == Some(&Token::Star) {
            self.index += 1;
            self.expect(&Token::RightBracket, "`]`")?;
            return Ok(Node::Filter(Box::new(target)));
        }
        let index = self.nested(Self::expression)?;
        self.expect(&Token::RightBracket, "`]`")?;
        Ok(Node::Index(Box::new(target), Box::new(index)))
    }

    fn primary(&mut self) -> Result<Node, ParseError> {
        let spanned = self.advance()?;
        match spanned.token {
            Token::Null => Ok(Node::Literal(Value::Null)),
            Token::True => Ok(Node::Literal(Value::Bool(true))),
            Token::False => Ok(Node::Literal(Value::Bool(false))),
            Token::Number(number) => Ok(Node::Literal(number_value(number))),
            Token::Str(text) => Ok(Node::Literal(Value::String(text))),
            Token::LeftParen => {
                let inner = self.nested(Self::expression)?;
                self.expect(&Token::RightParen, "`)`")?;
                Ok(inner)
            }
            Token::Name(name) => self.name(name),
            _ => Err(self.error(spanned.position, "expected a value")),
        }
    }

    fn name(&mut self, name: String) -> Result<Node, ParseError> {
        let lowered = name.to_lowercase();
        if !self.accept(&Token::LeftParen) {
            return Ok(Node::Context(lowered));
        }
        let arguments = self.nested(Self::arguments)?;
        Ok(Node::Call(lowered, arguments))
    }

    fn arguments(&mut self) -> Result<Vec<Node>, ParseError> {
        let mut arguments = Vec::new();
        if self.accept(&Token::RightParen) {
            return Ok(arguments);
        }
        loop {
            arguments.push(self.expression()?);
            if self.accept(&Token::RightParen) {
                return Ok(arguments);
            }
            self.expect(&Token::Comma, "`,` or `)`")?;
        }
    }
}
