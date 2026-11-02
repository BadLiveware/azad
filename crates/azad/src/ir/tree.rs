//! Directory nodes: the Merkle tree of a file system. A directory lists its entries in the order of their name bytes,
//! and each entry names a file, a directory or a symbolic link by digest.

use std::collections::BTreeMap;

use crate::ir::canonical::{Canonical, CanonicalError, Fields, digest_from_value, digest_to_value};
use crate::ir::cbor::Value;
use crate::ir::digest::Digest;

const KEY_ENTRIES: &str = "e";
const KEY_NAME: &str = "n";
const KEY_KIND: &str = "k";
const KEY_DIGEST: &str = "d";
const KEY_EXECUTABLE: &str = "x";
const KEY_TARGET: &str = "l";

const KIND_FILE: &str = "file";
const KIND_DIRECTORY: &str = "dir";
const KIND_SYMLINK: &str = "link";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    File { digest: Digest, executable: bool },
    Directory { digest: Digest },
    Symlink { target: String },
}

/// One directory. Construct it with `Directory::new`, which checks the names.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Directory {
    entries: BTreeMap<String, Node>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum NameError {
    #[error("an entry name must not be empty, \".\" or \"..\", and must not contain '/' or NUL: {0:?}")]
    Invalid(String),
}

pub fn check_name(name: &str) -> Result<(), NameError> {
    let invalid = name.is_empty() || name == "." || name == ".." || name.contains(['/', '\0']);
    if invalid {
        return Err(NameError::Invalid(name.to_owned()));
    }
    Ok(())
}

impl Directory {
    pub fn new(entries: BTreeMap<String, Node>) -> Result<Self, NameError> {
        for name in entries.keys() {
            check_name(name)?;
        }
        Ok(Directory { entries })
    }

    pub fn entries(&self) -> &BTreeMap<String, Node> {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Canonical for Directory {
    const TYPE: &'static str = "directory";

    fn fields(&self) -> BTreeMap<String, Value> {
        let entries = self.entries.iter().map(|(name, node)| Value::Map(entry_fields(name, node))).collect();
        BTreeMap::from([(KEY_ENTRIES.to_owned(), Value::Array(entries))])
    }

    fn from_fields(fields: &mut Fields) -> Result<Self, CanonicalError> {
        let mut entries = BTreeMap::new();
        let mut previous: Option<String> = None;
        for value in fields.array(KEY_ENTRIES)? {
            let Value::Map(map) = value else {
                return Err(CanonicalError::Field(KEY_ENTRIES));
            };
            let (name, node) = entry_from_fields(Fields::new(map))?;
            check_name(&name).map_err(|e| CanonicalError::Invalid(e.to_string()))?;
            if previous.as_deref().is_some_and(|p| p.as_bytes() >= name.as_bytes()) {
                return Err(CanonicalError::Invalid(format!("entry {name:?} is out of order or repeated")));
            }
            previous = Some(name.clone());
            entries.insert(name, node);
        }
        Ok(Directory { entries })
    }
}

fn entry_fields(name: &str, node: &Node) -> BTreeMap<String, Value> {
    let mut map = BTreeMap::from([(KEY_NAME.to_owned(), Value::Text(name.to_owned()))]);
    let kind = match node {
        Node::File { digest, executable } => {
            map.insert(KEY_DIGEST.to_owned(), digest_to_value(digest));
            map.insert(KEY_EXECUTABLE.to_owned(), Value::Bool(*executable));
            KIND_FILE
        }
        Node::Directory { digest } => {
            map.insert(KEY_DIGEST.to_owned(), digest_to_value(digest));
            KIND_DIRECTORY
        }
        Node::Symlink { target } => {
            map.insert(KEY_TARGET.to_owned(), Value::Text(target.clone()));
            KIND_SYMLINK
        }
    };
    map.insert(KEY_KIND.to_owned(), Value::Text(kind.to_owned()));
    map
}

fn entry_from_fields(mut fields: Fields) -> Result<(String, Node), CanonicalError> {
    let name = fields.text(KEY_NAME)?;
    let node = match fields.text(KEY_KIND)?.as_str() {
        KIND_FILE => {
            let digest = digest_from_value(&fields.take(KEY_DIGEST)?).ok_or(CanonicalError::Field(KEY_DIGEST))?;
            Node::File { digest, executable: fields.bool(KEY_EXECUTABLE)? }
        }
        KIND_DIRECTORY => {
            let digest = digest_from_value(&fields.take(KEY_DIGEST)?).ok_or(CanonicalError::Field(KEY_DIGEST))?;
            Node::Directory { digest }
        }
        KIND_SYMLINK => Node::Symlink { target: fields.text(KEY_TARGET)? },
        other => return Err(CanonicalError::Invalid(format!("unknown entry kind {other:?}"))),
    };
    fields.finish()?;
    Ok((name, node))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Directory {
        Directory::new(BTreeMap::from([
            ("b.txt".to_owned(), Node::File { digest: Digest::of_bytes(b"b"), executable: false }),
            ("a.sh".to_owned(), Node::File { digest: Digest::of_bytes(b"a"), executable: true }),
            ("sub".to_owned(), Node::Directory { digest: Directory::default().digest() }),
            ("link".to_owned(), Node::Symlink { target: "a.sh".to_owned() }),
        ]))
        .unwrap()
    }

    #[test]
    fn round_trips() {
        let dir = sample();
        assert_eq!(Directory::decode(&dir.encode()), Ok(dir));
    }

    #[test]
    fn the_empty_directory_has_the_expected_bytes() {
        let mut expected = vec![0xa3, 0x61, b'e', 0x80, 0x61, b't', 0x69];
        expected.extend_from_slice(b"directory");
        expected.extend_from_slice(&[0x61, b'v', 0x01]);
        assert_eq!(Directory::default().encode(), expected);
    }

    #[test]
    fn the_empty_directory_has_a_fixed_digest() {
        assert_eq!(
            Directory::default().digest().to_string(),
            "sha256:76a40fb768ec1ccbb0c762081b3557ebaddfb870ad9a58f5349ddf7deb22fe72/19"
        );
    }

    #[test]
    fn a_sample_directory_has_a_fixed_digest() {
        assert_eq!(
            sample().digest().to_string(),
            "sha256:797c9da6fbba9a34b2a70aff75546eaa7568f8b5ea5effd8c8d968bd1fbdd2fe/244"
        );
    }

    #[test]
    fn invalid_names_are_refused() {
        for name in ["", ".", "..", "a/b", "a\0b"] {
            let entries = BTreeMap::from([(name.to_owned(), Node::Symlink { target: "x".to_owned() })]);
            assert!(Directory::new(entries).is_err(), "{name:?}");
        }
    }

    #[test]
    fn decode_refuses_another_type() {
        let err = Directory::decode(&crate::ir::cbor::encode(&Value::Map(BTreeMap::from([
            ("t".to_owned(), Value::Text("action".to_owned())),
            ("v".to_owned(), Value::Uint(1)),
        ]))));
        assert!(matches!(err, Err(CanonicalError::Type { .. })));
    }

    #[test]
    fn decode_refuses_unknown_fields() {
        let mut map = Directory::default().fields();
        map.insert("t".to_owned(), Value::Text("directory".to_owned()));
        map.insert("v".to_owned(), Value::Uint(1));
        map.insert("z".to_owned(), Value::Null);
        let err = Directory::decode(&crate::ir::cbor::encode(&Value::Map(map)));
        assert_eq!(err, Err(CanonicalError::UnknownField("z".to_owned())));
    }
}
