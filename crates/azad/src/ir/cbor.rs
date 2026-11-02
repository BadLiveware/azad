//! Deterministic CBOR, as decision 0002 defines it: the core deterministic encoding of RFC 8949, section 4.2.1,
//! with only text map keys, and with no floating-point values and no tags.

use std::collections::BTreeMap;

const MAJOR_UINT: u8 = 0;
const MAJOR_NINT: u8 = 1;
const MAJOR_BYTES: u8 = 2;
const MAJOR_TEXT: u8 = 3;
const MAJOR_ARRAY: u8 = 4;
const MAJOR_MAP: u8 = 5;
const MAJOR_SIMPLE: u8 = 7;

const SIMPLE_FALSE: u8 = 20;
const SIMPLE_TRUE: u8 = 21;
const SIMPLE_NULL: u8 = 22;

const INFO_U8: u8 = 24;
const INFO_U16: u8 = 25;
const INFO_U32: u8 = 26;
const INFO_U64: u8 = 27;

const MAX_DEPTH: usize = 64;

/// A CBOR value that Azad can encode. Map keys are text, and a map cannot hold a key twice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Uint(u64),
    /// The negative integer `-1 - n`.
    Nint(u64),
    Bytes(Vec<u8>),
    Text(String),
    Array(Vec<Value>),
    Map(BTreeMap<String, Value>),
    Bool(bool),
    Null,
}

pub fn encode(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    write_value(&mut out, value);
    out
}

fn write_value(out: &mut Vec<u8>, value: &Value) {
    match value {
        Value::Uint(n) => write_head(out, MAJOR_UINT, *n),
        Value::Nint(n) => write_head(out, MAJOR_NINT, *n),
        Value::Bytes(b) => {
            write_head(out, MAJOR_BYTES, b.len() as u64);
            out.extend_from_slice(b);
        }
        Value::Text(t) => write_text(out, t),
        Value::Array(items) => {
            write_head(out, MAJOR_ARRAY, items.len() as u64);
            for item in items {
                write_value(out, item);
            }
        }
        Value::Map(map) => {
            write_head(out, MAJOR_MAP, map.len() as u64);
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_by(|(a, _), (b, _)| canonical_key_order(a, b));
            for (key, item) in entries {
                write_text(out, key);
                write_value(out, item);
            }
        }
        Value::Bool(false) => out.push(MAJOR_SIMPLE << 5 | SIMPLE_FALSE),
        Value::Bool(true) => out.push(MAJOR_SIMPLE << 5 | SIMPLE_TRUE),
        Value::Null => out.push(MAJOR_SIMPLE << 5 | SIMPLE_NULL),
    }
}

/// The order of encoded text keys: a shorter key first, then by bytes. For text keys this is the same as the
/// bytewise order of their encodings, because the head grows with the length.
fn canonical_key_order(a: &str, b: &str) -> std::cmp::Ordering {
    a.len().cmp(&b.len()).then_with(|| a.as_bytes().cmp(b.as_bytes()))
}

fn write_text(out: &mut Vec<u8>, text: &str) {
    write_head(out, MAJOR_TEXT, text.len() as u64);
    out.extend_from_slice(text.as_bytes());
}

fn write_head(out: &mut Vec<u8>, major: u8, n: u64) {
    let m = major << 5;
    match n {
        0..=23 => out.push(m | n as u8),
        24..=0xff => out.extend_from_slice(&[m | INFO_U8, n as u8]),
        0x100..=0xffff => {
            out.push(m | INFO_U16);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            out.push(m | INFO_U32);
            out.extend_from_slice(&(n as u32).to_be_bytes());
        }
        _ => {
            out.push(m | INFO_U64);
            out.extend_from_slice(&n.to_be_bytes());
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DecodeError {
    #[error("the input ends inside a value")]
    Truncated,
    #[error("bytes follow the value")]
    Trailing,
    #[error("an integer or length at offset {0} is not in its shortest form")]
    NotShortest(usize),
    #[error("an indefinite length at offset {0}")]
    Indefinite(usize),
    #[error("a floating-point value, a tag or an unknown simple value at offset {0}")]
    Unsupported(usize),
    #[error("text that is not UTF-8 at offset {0}")]
    Utf8(usize),
    #[error("a map key at offset {0} is not text")]
    KeyNotText(usize),
    #[error("map keys at offset {0} are not in canonical order, or a key repeats")]
    KeyOrder(usize),
    #[error("values nest deeper than {MAX_DEPTH}")]
    TooDeep,
}

/// Decodes exactly one value, and refuses every input that `encode` would not produce.
pub fn decode(bytes: &[u8]) -> Result<Value, DecodeError> {
    let mut reader = Reader { bytes, pos: 0 };
    let value = reader.value(0)?;
    if reader.pos != bytes.len() {
        return Err(DecodeError::Trailing);
    }
    Ok(value)
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], DecodeError> {
        let end = self.pos.checked_add(n).ok_or(DecodeError::Truncated)?;
        let slice = self.bytes.get(self.pos..end).ok_or(DecodeError::Truncated)?;
        self.pos = end;
        Ok(slice)
    }

    fn head(&mut self) -> Result<(u8, u8, u64, usize), DecodeError> {
        let start = self.pos;
        let first = self.take(1)?[0];
        let major = first >> 5;
        let info = first & 0x1f;
        let (n, min) = match info {
            0..=23 => (u64::from(info), 0),
            INFO_U8 => (u64::from(self.take(1)?[0]), 24),
            INFO_U16 => (u64::from(u16::from_be_bytes(self.array()?)), 0x100),
            INFO_U32 => (u64::from(u32::from_be_bytes(self.array()?)), 0x1_0000),
            INFO_U64 => (u64::from_be_bytes(self.array()?), 0x1_0000_0000),
            31 => return Err(DecodeError::Indefinite(start)),
            _ => return Err(DecodeError::Unsupported(start)),
        };
        if major != MAJOR_SIMPLE && n < min {
            return Err(DecodeError::NotShortest(start));
        }
        Ok((major, info, n, start))
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }

    fn length(&mut self, n: u64) -> Result<usize, DecodeError> {
        let n = usize::try_from(n).map_err(|_| DecodeError::Truncated)?;
        if n > self.bytes.len() - self.pos {
            return Err(DecodeError::Truncated);
        }
        Ok(n)
    }

    fn text(&mut self, n: u64, start: usize) -> Result<String, DecodeError> {
        let len = self.length(n)?;
        let raw = self.take(len)?;
        String::from_utf8(raw.to_vec()).map_err(|_| DecodeError::Utf8(start))
    }

    fn value(&mut self, depth: usize) -> Result<Value, DecodeError> {
        if depth > MAX_DEPTH {
            return Err(DecodeError::TooDeep);
        }
        let (major, info, n, start) = self.head()?;
        match major {
            MAJOR_UINT => Ok(Value::Uint(n)),
            MAJOR_NINT => Ok(Value::Nint(n)),
            MAJOR_BYTES => {
                let len = self.length(n)?;
                Ok(Value::Bytes(self.take(len)?.to_vec()))
            }
            MAJOR_TEXT => Ok(Value::Text(self.text(n, start)?)),
            MAJOR_ARRAY => {
                let len = self.length(n)?;
                (0..len).map(|_| self.value(depth + 1)).collect::<Result<_, _>>().map(Value::Array)
            }
            MAJOR_MAP => self.map(n, depth),
            MAJOR_SIMPLE => match info {
                SIMPLE_FALSE => Ok(Value::Bool(false)),
                SIMPLE_TRUE => Ok(Value::Bool(true)),
                SIMPLE_NULL => Ok(Value::Null),
                _ => Err(DecodeError::Unsupported(start)),
            },
            _ => Err(DecodeError::Unsupported(start)),
        }
    }

    fn map(&mut self, n: u64, depth: usize) -> Result<Value, DecodeError> {
        let len = self.length(n)?;
        let mut map = BTreeMap::new();
        let mut previous: Option<String> = None;
        for _ in 0..len {
            let (major, _, key_len, key_start) = self.head()?;
            if major != MAJOR_TEXT {
                return Err(DecodeError::KeyNotText(key_start));
            }
            let key = self.text(key_len, key_start)?;
            let in_order = previous.as_deref().is_none_or(|p| canonical_key_order(p, &key).is_lt());
            if !in_order {
                return Err(DecodeError::KeyOrder(key_start));
            }
            let value = self.value(depth + 1)?;
            previous = Some(key.clone());
            map.insert(key, value);
        }
        Ok(Value::Map(map))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn map(entries: &[(&str, Value)]) -> Value {
        Value::Map(entries.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect())
    }

    #[test]
    fn integers_use_their_shortest_form() {
        assert_eq!(encode(&Value::Uint(23)), [0x17]);
        assert_eq!(encode(&Value::Uint(24)), [0x18, 24]);
        assert_eq!(encode(&Value::Uint(256)), [0x19, 1, 0]);
        assert_eq!(encode(&Value::Uint(65536)), [0x1a, 0, 1, 0, 0]);
        assert_eq!(encode(&Value::Uint(1 << 32)), [0x1b, 0, 0, 0, 1, 0, 0, 0, 0]);
        assert_eq!(encode(&Value::Nint(0)), [0x20]);
    }

    #[test]
    fn map_keys_are_sorted_by_length_then_bytes() {
        let value = map(&[("bb", Value::Uint(1)), ("a", Value::Uint(2)), ("ab", Value::Uint(3))]);
        assert_eq!(encode(&value), [0xa3, 0x61, b'a', 2, 0x62, b'a', b'b', 3, 0x62, b'b', b'b', 1]);
    }

    #[test]
    fn decode_refuses_non_canonical_input() {
        assert_eq!(decode(&[0x18, 5]), Err(DecodeError::NotShortest(0)));
        assert_eq!(decode(&[0x9f, 0xff]), Err(DecodeError::Indefinite(0)));
        assert_eq!(decode(&[0xf9, 0, 0]), Err(DecodeError::Unsupported(0)));
        assert_eq!(decode(&[0xc0, 0]), Err(DecodeError::Unsupported(0)));
        assert_eq!(decode(&[0xa1, 1, 2]), Err(DecodeError::KeyNotText(1)));
        assert_eq!(decode(&[0xa2, 0x62, b'b', b'b', 1, 0x61, b'a', 2]), Err(DecodeError::KeyOrder(5)));
        assert_eq!(decode(&[0xa2, 0x61, b'a', 1, 0x61, b'a', 2]), Err(DecodeError::KeyOrder(4)));
        assert_eq!(decode(&[0x01, 0x01]), Err(DecodeError::Trailing));
        assert_eq!(decode(&[0x62, 0xff, 0xfe]), Err(DecodeError::Utf8(0)));
        assert_eq!(decode(&[0x5a, 0xff, 0xff, 0xff, 0xff]), Err(DecodeError::Truncated));
    }

    #[test]
    fn decode_refuses_deep_nesting() {
        let deep = vec![0x81; MAX_DEPTH + 2];
        assert_eq!(decode(&deep), Err(DecodeError::TooDeep));
    }

    fn arb_value() -> impl Strategy<Value = Value> {
        let leaf = prop_oneof![
            any::<u64>().prop_map(Value::Uint),
            any::<u64>().prop_map(Value::Nint),
            proptest::collection::vec(any::<u8>(), 0..40).prop_map(Value::Bytes),
            ".{0,30}".prop_map(Value::Text),
            any::<bool>().prop_map(Value::Bool),
            Just(Value::Null),
        ];
        leaf.prop_recursive(4, 64, 8, |inner| {
            prop_oneof![
                proptest::collection::vec(inner.clone(), 0..8).prop_map(Value::Array),
                proptest::collection::btree_map(".{0,12}", inner, 0..8).prop_map(Value::Map),
            ]
        })
    }

    proptest! {
        #[test]
        fn decode_is_the_inverse_of_encode(value in arb_value()) {
            prop_assert_eq!(decode(&encode(&value)), Ok(value));
        }

        #[test]
        fn decoded_input_encodes_to_the_same_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..64)) {
            if let Ok(value) = decode(&bytes) {
                prop_assert_eq!(encode(&value), bytes);
            }
        }
    }
}
