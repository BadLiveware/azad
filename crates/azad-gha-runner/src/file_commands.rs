//! The files that a step writes to: `GITHUB_OUTPUT`, `GITHUB_ENV`, `GITHUB_STATE` and `GITHUB_PATH`.

use crate::error::Error;

const HEREDOC: &str = "<<";
const ASSIGN: char = '=';
const CARRIAGE_RETURN: char = '\r';
const NEWLINE: &str = "\n";

/// Reads `name=value` lines and `name<<DELIMITER` blocks, in the order the step wrote them.
pub(crate) fn parse_key_values(text: &str) -> Result<Vec<(String, String)>, Error> {
    let mut pairs = Vec::new();
    let mut lines = text.split('\n').map(|line| line.strip_suffix(CARRIAGE_RETURN).unwrap_or(line));
    while let Some(line) = lines.next() {
        if line.is_empty() {
            continue;
        }
        let pair = match heredoc_start(line) {
            Some((name, delimiter)) => (name.to_string(), heredoc_value(&mut lines, name, delimiter)?),
            None => assignment(line)?,
        };
        pairs.push(pair);
    }
    Ok(pairs)
}

/// The name and delimiter when the line opens a `name<<DELIMITER` block, that is when `<<` comes before any `=`.
fn heredoc_start(line: &str) -> Option<(&str, &str)> {
    let heredoc_at = line.find(HEREDOC)?;
    let is_assignment_first = line.find(ASSIGN).is_some_and(|assign_at| assign_at < heredoc_at);
    if is_assignment_first {
        return None;
    }
    Some((&line[..heredoc_at], &line[heredoc_at + HEREDOC.len()..]))
}

fn heredoc_value<'a>(lines: &mut impl Iterator<Item = &'a str>, name: &str, delimiter: &str) -> Result<String, Error> {
    if name.is_empty() || delimiter.is_empty() {
        return Err(Error::new(format!("`{name}{HEREDOC}{delimiter}` needs a name and a delimiter")));
    }
    let mut value_lines = Vec::new();
    for line in lines.by_ref() {
        if line == delimiter {
            return Ok(value_lines.join(NEWLINE));
        }
        value_lines.push(line);
    }
    Err(Error::new(format!("the value of `{name}` has no line with its delimiter `{delimiter}`")))
}

fn assignment(line: &str) -> Result<(String, String), Error> {
    match line.split_once(ASSIGN) {
        Some((name, value)) if !name.is_empty() => Ok((name.to_string(), value.to_string())),
        _ => Err(Error::new(format!("`{line}` is not `name=value` or `name<<DELIMITER`"))),
    }
}

/// The lines of `GITHUB_PATH`, one directory each.
pub(crate) fn parse_paths(text: &str) -> Vec<String> {
    text.lines().map(str::trim).filter(|line| !line.is_empty()).map(str::to_string).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(text: &str) -> Vec<(String, String)> {
        parse_key_values(text).expect("valid file")
    }

    fn pair(name: &str, value: &str) -> (String, String) {
        (name.to_string(), value.to_string())
    }

    #[test]
    fn reads_assignments() {
        assert_eq!(pairs("a=1\nb=x=y\n\nc=\n"), vec![pair("a", "1"), pair("b", "x=y"), pair("c", "")]);
    }

    #[test]
    fn reads_multi_line_values() {
        let text = "text<<EOF\nline one\nline two\nEOF\nnext=1\n";
        assert_eq!(pairs(text), vec![pair("text", "line one\nline two"), pair("next", "1")]);
    }

    #[test]
    fn reads_windows_line_ends() {
        assert_eq!(pairs("a=1\r\nb<<X\r\nv\r\nX\r\n"), vec![pair("a", "1"), pair("b", "v")]);
    }

    #[test]
    fn an_equals_sign_before_the_heredoc_marker_is_an_assignment() {
        assert_eq!(pairs("a=b<<c\n"), vec![pair("a", "b<<c")]);
    }

    #[test]
    fn an_empty_multi_line_value_is_empty() {
        assert_eq!(pairs("a<<END\nEND\n"), vec![pair("a", "")]);
    }

    #[test]
    fn refuses_a_block_without_its_delimiter() {
        let error = parse_key_values("a<<END\nvalue\n").expect_err("no delimiter");
        assert!(error.to_string().contains("END"), "{error}");
    }

    #[test]
    fn refuses_a_line_without_a_form() {
        assert!(parse_key_values("just text\n").is_err());
        assert!(parse_key_values("=value\n").is_err());
    }

    #[test]
    fn reads_paths() {
        assert_eq!(parse_paths("/a/bin\n\n  /b/bin  \n"), vec!["/a/bin", "/b/bin"]);
    }
}
