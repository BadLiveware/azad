//! Converting Azad's types to and from CBOR values. Every encoded object is a map with a type and a schema version,
//! so two types can never share an encoding, and a reader can refuse a version that it does not know.

use std::collections::BTreeMap;

use crate::ir::cbor::{self, Value};
use crate::ir::digest::{Digest, HashFunction};

pub const SCHEMA_VERSION: u64 = 1;

const KEY_TYPE: &str = "t";
const KEY_VERSION: &str = "v";
const KEY_FUNCTION: &str = "f";
const KEY_HASH: &str = "h";
const KEY_SIZE: &str = "s";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CanonicalError {
    #[error(transparent)]
    Cbor(#[from] cbor::DecodeError),
    #[error("expected an object of type {expected:?}, found {found:?}")]
    Type { expected: &'static str, found: String },
    #[error("schema version {0} is not supported")]
    Version(u64),
    #[error("field {0:?} is missing or has the wrong type")]
    Field(&'static str),
    #[error("unknown field {0:?}")]
    UnknownField(String),
    #[error("{0}")]
    Invalid(String),
}

/// A type with exactly one encoding.
pub trait Canonical: Sized {
    const TYPE: &'static str;

    fn fields(&self) -> BTreeMap<String, Value>;
    fn from_fields(fields: &mut Fields) -> Result<Self, CanonicalError>;

    fn encode(&self) -> Vec<u8> {
        let mut map = self.fields();
        map.insert(KEY_TYPE.to_owned(), Value::Text(Self::TYPE.to_owned()));
        map.insert(KEY_VERSION.to_owned(), Value::Uint(SCHEMA_VERSION));
        cbor::encode(&Value::Map(map))
    }

    fn digest(&self) -> Digest {
        Digest::of_bytes(&self.encode())
    }

    fn decode(bytes: &[u8]) -> Result<Self, CanonicalError> {
        let Value::Map(map) = cbor::decode(bytes)? else {
            return Err(CanonicalError::Field(KEY_TYPE));
        };
        let mut fields = Fields(map);
        let found = fields.text(KEY_TYPE)?;
        if found != Self::TYPE {
            return Err(CanonicalError::Type { expected: Self::TYPE, found });
        }
        let version = fields.uint(KEY_VERSION)?;
        if version != SCHEMA_VERSION {
            return Err(CanonicalError::Version(version));
        }
        let value = Self::from_fields(&mut fields)?;
        fields.finish()?;
        Ok(value)
    }
}

/// The fields of an object while it is read. Each field is taken once, and `finish` refuses fields that remain.
#[derive(Debug)]
pub struct Fields(BTreeMap<String, Value>);

impl Fields {
    pub fn new(map: BTreeMap<String, Value>) -> Self {
        Fields(map)
    }

    pub fn take(&mut self, key: &'static str) -> Result<Value, CanonicalError> {
        self.0.remove(key).ok_or(CanonicalError::Field(key))
    }

    pub fn take_optional(&mut self, key: &'static str) -> Option<Value> {
        self.0.remove(key)
    }

    pub fn text(&mut self, key: &'static str) -> Result<String, CanonicalError> {
        match self.take(key)? {
            Value::Text(t) => Ok(t),
            _ => Err(CanonicalError::Field(key)),
        }
    }

    pub fn uint(&mut self, key: &'static str) -> Result<u64, CanonicalError> {
        match self.take(key)? {
            Value::Uint(n) => Ok(n),
            _ => Err(CanonicalError::Field(key)),
        }
    }

    pub fn bool(&mut self, key: &'static str) -> Result<bool, CanonicalError> {
        match self.take(key)? {
            Value::Bool(b) => Ok(b),
            _ => Err(CanonicalError::Field(key)),
        }
    }

    pub fn array(&mut self, key: &'static str) -> Result<Vec<Value>, CanonicalError> {
        match self.take(key)? {
            Value::Array(a) => Ok(a),
            _ => Err(CanonicalError::Field(key)),
        }
    }

    pub fn map(&mut self, key: &'static str) -> Result<BTreeMap<String, Value>, CanonicalError> {
        match self.take(key)? {
            Value::Map(m) => Ok(m),
            _ => Err(CanonicalError::Field(key)),
        }
    }

    pub fn digest(&mut self, key: &'static str) -> Result<Digest, CanonicalError> {
        let value = self.take(key)?;
        digest_from_value(&value).ok_or(CanonicalError::Field(key))
    }

    pub fn finish(self) -> Result<(), CanonicalError> {
        match self.0.into_keys().next() {
            Some(key) => Err(CanonicalError::UnknownField(key)),
            None => Ok(()),
        }
    }
}

pub fn digest_to_value(digest: &Digest) -> Value {
    Value::Map(BTreeMap::from([
        (KEY_FUNCTION.to_owned(), Value::Text(digest.function().name().to_owned())),
        (KEY_HASH.to_owned(), Value::Bytes(digest.hash().to_vec())),
        (KEY_SIZE.to_owned(), Value::Uint(digest.size())),
    ]))
}

pub fn digest_from_value(value: &Value) -> Option<Digest> {
    let Value::Map(map) = value else { return None };
    if map.len() != 3 {
        return None;
    }
    let function = match map.get(KEY_FUNCTION)? {
        Value::Text(name) => HashFunction::from_name(name)?,
        _ => return None,
    };
    let hash = match map.get(KEY_HASH)? {
        Value::Bytes(bytes) => bytes.as_slice().try_into().ok()?,
        _ => return None,
    };
    let size = match map.get(KEY_SIZE)? {
        Value::Uint(n) => *n,
        _ => return None,
    };
    Some(Digest::new(function, hash, size))
}

pub fn text_list(values: Vec<Value>, key: &'static str) -> Result<Vec<String>, CanonicalError> {
    values
        .into_iter()
        .map(|v| match v {
            Value::Text(t) => Ok(t),
            _ => Err(CanonicalError::Field(key)),
        })
        .collect()
}

pub fn text_map(map: BTreeMap<String, Value>, key: &'static str) -> Result<BTreeMap<String, String>, CanonicalError> {
    map.into_iter()
        .map(|(k, v)| match v {
            Value::Text(t) => Ok((k, t)),
            _ => Err(CanonicalError::Field(key)),
        })
        .collect()
}

pub fn texts(items: &[String]) -> Value {
    Value::Array(items.iter().cloned().map(Value::Text).collect())
}

pub fn text_values(map: &BTreeMap<String, String>) -> Value {
    Value::Map(map.iter().map(|(k, v)| (k.clone(), Value::Text(v.clone()))).collect())
}
