use std::fmt;
use std::io;
use std::str::FromStr;

use sha2::{Digest as _, Sha256};

const SHA256_NAME: &str = "sha256";
const SHA256_LEN: usize = 32;

/// The hash function of a digest. See decision 0001.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HashFunction {
    Sha256,
}

impl HashFunction {
    pub fn name(self) -> &'static str {
        match self {
            HashFunction::Sha256 => SHA256_NAME,
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            SHA256_NAME => Some(HashFunction::Sha256),
            _ => None,
        }
    }
}

/// The identity of content: its hash function, its hash and its size in bytes.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Digest {
    function: HashFunction,
    hash: [u8; SHA256_LEN],
    size: u64,
}

impl Digest {
    pub fn new(function: HashFunction, hash: [u8; SHA256_LEN], size: u64) -> Self {
        Digest { function, hash, size }
    }

    pub fn of_bytes(bytes: &[u8]) -> Self {
        let mut hasher = Hasher::new();
        hasher.update(bytes);
        hasher.finish()
    }

    pub fn function(&self) -> HashFunction {
        self.function
    }

    pub fn hash(&self) -> &[u8; SHA256_LEN] {
        &self.hash
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn hex(&self) -> String {
        to_hex(&self.hash)
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}/{}", self.function.name(), self.hex(), self.size)
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ParseDigestError {
    #[error("a digest has the form <function>:<hex>/<size>")]
    Shape,
    #[error("unknown hash function {0:?}")]
    Function(String),
    #[error("the hash is not {SHA256_LEN} bytes of lower-case hex")]
    Hash,
    #[error("the size is not a number")]
    Size,
}

impl FromStr for Digest {
    type Err = ParseDigestError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (name, rest) = s.split_once(':').ok_or(ParseDigestError::Shape)?;
        let (hex, size) = rest.split_once('/').ok_or(ParseDigestError::Shape)?;
        let function = HashFunction::from_name(name).ok_or_else(|| ParseDigestError::Function(name.to_owned()))?;
        let hash = from_hex(hex).ok_or(ParseDigestError::Hash)?;
        let size = parse_size(size).ok_or(ParseDigestError::Size)?;
        Ok(Digest { function, hash, size })
    }
}

fn parse_size(s: &str) -> Option<u64> {
    let canonical = !s.is_empty() && (s == "0" || !s.starts_with('0')) && s.bytes().all(|b| b.is_ascii_digit());
    if !canonical {
        return None;
    }
    s.parse().ok()
}

/// Computes a digest while the content is written to it.
#[derive(Debug, Default)]
pub struct Hasher {
    inner: Sha256,
    size: u64,
}

impl Hasher {
    pub fn new() -> Self {
        Hasher::default()
    }

    pub fn update(&mut self, bytes: &[u8]) {
        self.inner.update(bytes);
        self.size += bytes.len() as u64;
    }

    pub fn finish(self) -> Digest {
        let hash: [u8; SHA256_LEN] = self.inner.finalize().into();
        Digest { function: HashFunction::Sha256, hash, size: self.size }
    }
}

impl io::Write for Hasher {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.update(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[usize::from(b >> 4)] as char);
        out.push(DIGITS[usize::from(b & 0xf)] as char);
    }
    out
}

fn from_hex(s: &str) -> Option<[u8; SHA256_LEN]> {
    let bytes = s.as_bytes();
    if bytes.len() != SHA256_LEN * 2 {
        return None;
    }
    let mut out = [0u8; SHA256_LEN];
    for (i, [high, low]) in bytes.as_chunks::<2>().0.iter().enumerate() {
        out[i] = (hex_value(*high)? << 4) | hex_value(*low)?;
    }
    Some(out)
}

fn hex_value(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_has_the_known_sha256() {
        assert_eq!(
            Digest::of_bytes(b"").to_string(),
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855/0"
        );
    }

    #[test]
    fn text_form_round_trips() {
        let d = Digest::of_bytes(b"azad");
        assert_eq!(d.to_string().parse::<Digest>(), Ok(d));
    }

    #[test]
    fn text_form_refuses_non_canonical_input() {
        let d = Digest::of_bytes(b"azad");
        let upper = d.to_string().replace(&d.hex(), &d.hex().to_uppercase());
        assert_eq!(upper.parse::<Digest>(), Err(ParseDigestError::Hash));
        let padded = format!("{}:{}/0{}", d.function().name(), d.hex(), d.size());
        assert_eq!(padded.parse::<Digest>(), Err(ParseDigestError::Size));
        assert!(matches!("md5:00/1".parse::<Digest>(), Err(ParseDigestError::Function(_))));
    }

    #[test]
    fn streaming_and_whole_input_agree() {
        let mut h = Hasher::new();
        h.update(b"az");
        h.update(b"ad");
        assert_eq!(h.finish(), Digest::of_bytes(b"azad"));
    }
}
