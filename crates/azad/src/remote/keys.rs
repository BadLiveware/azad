//! Signing keys for result entries, written `name:base64` as Nix writes its keys. The name says which key signed an
//! entry, so keys can be rotated.

use std::fmt;
use std::str::FromStr;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::{Signature, Signer, Verifier};

use crate::remote::RemoteError;

const KEY_LEN: usize = 32;

/// A private key, which signs result entries. It is a secret.
pub struct SigningKey {
    name: String,
    key: ed25519_dalek::SigningKey,
}

/// A public key, which verifies the entries that its signing key signed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicKey {
    name: String,
    key: ed25519_dalek::VerifyingKey,
}

impl SigningKey {
    /// A new key with random bytes from the operating system.
    pub fn generate(name: &str) -> Result<Self, RemoteError> {
        check_name(name)?;
        let mut seed = [0u8; KEY_LEN];
        getrandom::fill(&mut seed).map_err(|e| RemoteError::Key(format!("no random bytes: {e}")))?;
        Ok(SigningKey { name: name.to_owned(), key: ed25519_dalek::SigningKey::from_bytes(&seed) })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn public(&self) -> PublicKey {
        PublicKey { name: self.name.clone(), key: self.key.verifying_key() }
    }

    pub(crate) fn sign(&self, message: &[u8]) -> Vec<u8> {
        self.key.sign(message).to_bytes().to_vec()
    }

    /// The text form, `name:base64`, to keep as a secret.
    pub fn to_secret_string(&self) -> String {
        format!("{}:{}", self.name, STANDARD.encode(self.key.to_bytes()))
    }
}

impl fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SigningKey({}, public {})", self.name, self.public())
    }
}

impl FromStr for SigningKey {
    type Err = RemoteError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let (name, bytes) = split(text)?;
        Ok(SigningKey { name, key: ed25519_dalek::SigningKey::from_bytes(&bytes) })
    }
}

impl PublicKey {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn verify(&self, message: &[u8], signature: &[u8]) -> bool {
        let Ok(signature) = Signature::from_slice(signature) else { return false };
        self.key.verify(message, &signature).is_ok()
    }
}

impl fmt::Display for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.name, STANDARD.encode(self.key.to_bytes()))
    }
}

impl FromStr for PublicKey {
    type Err = RemoteError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let (name, bytes) = split(text)?;
        let key = ed25519_dalek::VerifyingKey::from_bytes(&bytes)
            .map_err(|e| RemoteError::Key(format!("{name}: not a public key: {e}")))?;
        Ok(PublicKey { name, key })
    }
}

/// The public keys in a list: one per line; empty lines and lines that start with `#` are skipped.
pub fn parse_public_keys(text: &str) -> Result<Vec<PublicKey>, RemoteError> {
    text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')).map(str::parse).collect()
}

fn split(text: &str) -> Result<(String, [u8; KEY_LEN]), RemoteError> {
    let Some((name, encoded)) = text.trim().split_once(':') else {
        return Err(RemoteError::Key("a key is written name:base64".to_owned()));
    };
    check_name(name)?;
    let bytes = STANDARD.decode(encoded).map_err(|e| RemoteError::Key(format!("{name}: not base64: {e}")))?;
    let bytes = <[u8; KEY_LEN]>::try_from(bytes.as_slice())
        .map_err(|_| RemoteError::Key(format!("{name}: a key has {KEY_LEN} bytes")))?;
    Ok((name.to_owned(), bytes))
}

fn check_name(name: &str) -> Result<(), RemoteError> {
    let valid = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    match valid {
        true => Ok(()),
        false => Err(RemoteError::Key(format!("{name:?}: a key name has letters, digits, '-', '_' and '.'"))),
    }
}
