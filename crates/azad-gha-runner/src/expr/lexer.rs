use crate::expr::error::ParseError;
use crate::expr::value::parse_number_text;

const KEYWORD_NULL: &str = "null";
const KEYWORD_TRUE: &str = "true";
const KEYWORD_FALSE: &str = "false";
const QUOTE: char = '\'';

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Token {
    Null,
    True,
    False,
    Number(f64),
    Str(String),
    Name(String),
    LeftParen,
    RightParen,
    LeftBracket,
    RightBracket,
    Dot,
    Comma,
    Star,
    Not,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
    Equal,
    NotEqual,
    And,
    Or,
}

#[derive(Clone, Debug)]
pub(crate) struct Spanned {
    pub(crate) token: Token,
    /// The 1-based character position of the first character.
    pub(crate) position: usize,
}

pub(crate) fn tokenize(source: &str) -> Result<Vec<Spanned>, ParseError> {
    let mut lexer = Lexer { source, chars: source.chars().collect(), index: 0, tokens: Vec::new() };
    lexer.run()?;
    Ok(lexer.tokens)
}

struct Lexer<'a> {
    source: &'a str,
    chars: Vec<char>,
    index: usize,
    tokens: Vec<Spanned>,
}

fn is_name_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_name_part(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

fn is_number_part(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '.'
}

impl Lexer<'_> {
    fn run(&mut self) -> Result<(), ParseError> {
        loop {
            self.skip_whitespace();
            let Some(c) = self.peek(0) else {
                return Ok(());
            };
            let position = self.index + 1;
            let token = self.next_token(c, position)?;
            self.tokens.push(Spanned { token, position });
        }
    }

    fn error(&self, position: usize, message: impl Into<String>) -> ParseError {
        ParseError::new(self.source, position, message)
    }

    fn peek(&self, offset: usize) -> Option<char> {
        self.chars.get(self.index + offset).copied()
    }

    fn skip_whitespace(&mut self) {
        while self.peek(0).is_some_and(char::is_whitespace) {
            self.index += 1;
        }
    }

    fn follows_dot(&self) -> bool {
        self.tokens.last().is_some_and(|spanned| spanned.token == Token::Dot)
    }

    fn next_token(&mut self, c: char, position: usize) -> Result<Token, ParseError> {
        if self.follows_dot() && is_name_part(c) {
            return Ok(Token::Name(self.take_while(is_name_part)));
        }
        match c {
            '(' => Ok(self.single(Token::LeftParen)),
            ')' => Ok(self.single(Token::RightParen)),
            '[' => Ok(self.single(Token::LeftBracket)),
            ']' => Ok(self.single(Token::RightBracket)),
            '.' => Ok(self.single(Token::Dot)),
            ',' => Ok(self.single(Token::Comma)),
            '*' => Ok(self.single(Token::Star)),
            QUOTE => self.string(position),
            '!' => Ok(self.one_or_two('=', Token::Not, Token::NotEqual)),
            '<' => Ok(self.one_or_two('=', Token::Less, Token::LessOrEqual)),
            '>' => Ok(self.one_or_two('=', Token::Greater, Token::GreaterOrEqual)),
            '=' => self.two('=', Token::Equal, position),
            '&' => self.two('&', Token::And, position),
            '|' => self.two('|', Token::Or, position),
            _ => self.number_or_word(c, position),
        }
    }

    fn number_or_word(&mut self, c: char, position: usize) -> Result<Token, ParseError> {
        let starts_negative = c == '-' && self.peek(1).is_some_and(|next| next.is_ascii_digit() || next == '.');
        if c.is_ascii_digit() || starts_negative {
            return self.number(position);
        }
        if is_name_start(c) {
            return Ok(self.word());
        }
        Err(self.error(position, format!("unexpected character `{c}`")))
    }

    fn single(&mut self, token: Token) -> Token {
        self.index += 1;
        token
    }

    fn one_or_two(&mut self, second: char, one: Token, two: Token) -> Token {
        self.index += 1;
        if self.peek(0) != Some(second) {
            return one;
        }
        self.index += 1;
        two
    }

    fn two(&mut self, second: char, token: Token, position: usize) -> Result<Token, ParseError> {
        if self.peek(1) != Some(second) {
            let first = self.chars[self.index];
            return Err(self.error(position, format!("expected `{first}{second}`")));
        }
        self.index += 2;
        Ok(token)
    }

    fn take_while(&mut self, accept: impl Fn(char) -> bool) -> String {
        let start = self.index;
        while self.peek(0).is_some_and(&accept) {
            self.index += 1;
        }
        self.chars[start..self.index].iter().collect()
    }

    fn word(&mut self) -> Token {
        let word = self.take_while(is_name_part);
        match word.as_str() {
            KEYWORD_NULL => Token::Null,
            KEYWORD_TRUE => Token::True,
            KEYWORD_FALSE => Token::False,
            _ => Token::Name(word),
        }
    }

    fn string(&mut self, position: usize) -> Result<Token, ParseError> {
        self.index += 1;
        let mut text = String::new();
        loop {
            let Some(c) = self.peek(0) else {
                return Err(self.error(position, "the string has no closing quote"));
            };
            self.index += 1;
            if c != QUOTE {
                text.push(c);
                continue;
            }
            if self.peek(0) != Some(QUOTE) {
                return Ok(Token::Str(text));
            }
            self.index += 1;
            text.push(QUOTE);
        }
    }

    fn number(&mut self, position: usize) -> Result<Token, ParseError> {
        let start = self.index;
        self.index += 1;
        while let Some(c) = self.peek(0) {
            let after_exponent = matches!(c, '+' | '-')
                && self.index > start
                && matches!(self.chars[self.index - 1], 'e' | 'E')
                && !self.is_prefixed_radix(start);
            if !is_number_part(c) && !after_exponent {
                break;
            }
            self.index += 1;
        }
        let text: String = self.chars[start..self.index].iter().collect();
        match parse_number_text(&text).filter(|number| number.is_finite()) {
            Some(number) => Ok(Token::Number(number)),
            None => Err(self.error(position, format!("`{text}` is not a valid number"))),
        }
    }

    fn is_prefixed_radix(&self, start: usize) -> bool {
        let digits_start = match self.chars[start] {
            '-' => start + 1,
            _ => start,
        };
        let prefix: String = self.chars[digits_start..].iter().take(2).collect();
        prefix == "0x" || prefix == "0o"
    }
}
