//! The little of HTTP/1.1 that webhook deliveries need: one request per connection, with a body of a known length.

use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// GitHub's largest payload.
const MAX_BODY: usize = 25 << 20;
const MAX_HEADERS: usize = 100;
const TIMEOUT: Duration = Duration::from_secs(10);
const CONTENT_LENGTH: &str = "content-length";

#[derive(Debug)]
pub(super) struct Request {
    pub method: String,
    pub path: String,
    /// By lower-case name.
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

pub(super) fn read_request(stream: &TcpStream) -> io::Result<Request> {
    stream.set_read_timeout(Some(TIMEOUT))?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut words = line.split_whitespace();
    let (Some(method), Some(path)) = (words.next(), words.next()) else {
        return Err(invalid(format!("a bad request line: {line:?}")));
    };
    let (method, path) = (method.to_owned(), path.to_owned());
    let mut headers = BTreeMap::new();
    loop {
        let mut header = String::new();
        reader.read_line(&mut header)?;
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if headers.len() >= MAX_HEADERS {
            return Err(invalid("too many headers".to_owned()));
        }
        if let Some((name, value)) = header.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    let length: usize = headers.get(CONTENT_LENGTH).and_then(|value| value.parse().ok()).unwrap_or(0);
    if length > MAX_BODY {
        return Err(invalid(format!("a body of {length} bytes is larger than the limit")));
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    Ok(Request { method, path, headers, body })
}

/// An answer with a line of text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub text: String,
}

impl Response {
    pub(super) fn new(status: u16, text: impl Into<String>) -> Self {
        Response { status, text: text.into() }
    }
}

pub(super) fn write_response(mut stream: &TcpStream, response: &Response) -> io::Result<()> {
    let body = format!("{}\n", response.text);
    write!(
        stream,
        "HTTP/1.1 {} {}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        response.status,
        reason(response.status),
        body.len()
    )?;
    stream.flush()
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Internal Server Error",
    }
}

fn invalid(reason: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}
