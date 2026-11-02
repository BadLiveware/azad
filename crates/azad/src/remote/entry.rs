//! Signed entries: "the action with this digest has this result", and "the image with this name has this record",
//! with the signature of a key. The content that an entry names checks itself by its digests; only the claim needs a
//! signature.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};

use crate::ir::{ActionResult, Canonical, Digest, Image};
use crate::remote::RemoteError;
use crate::remote::keys::{PublicKey, SigningKey};

/// Bound into every signature, so that a signature for another purpose or format cannot be taken for an entry.
const CONTEXT: &[u8] = b"azad result entry v1\0";

#[derive(Serialize, Deserialize)]
struct Wire {
    action: String,
    /// The canonical encoding of the result, in base64.
    result: String,
    key: String,
    signature: String,
}

fn message(action: &Digest, encoded_result: &[u8]) -> Vec<u8> {
    let action = action.to_string();
    let mut message = Vec::with_capacity(CONTEXT.len() + action.len() + 1 + encoded_result.len());
    message.extend_from_slice(CONTEXT);
    message.extend_from_slice(action.as_bytes());
    message.push(0);
    message.extend_from_slice(encoded_result);
    message
}

/// The bytes of an entry for `action`, signed with `key`.
pub(crate) fn sign(action: &Digest, result: &ActionResult, key: &SigningKey) -> Vec<u8> {
    let encoded = result.encode();
    let signature = key.sign(&message(action, &encoded));
    let wire = Wire {
        action: action.to_string(),
        result: STANDARD.encode(&encoded),
        key: key.name().to_owned(),
        signature: STANDARD.encode(signature),
    };
    serde_json::to_vec(&wire).expect("an entry serializes")
}

/// The result in an entry, when the entry is for `action` and a trusted key signed it.
pub(crate) fn verify(bytes: &[u8], action: &Digest, trusted: &[PublicKey]) -> Result<ActionResult, RemoteError> {
    let wire: Wire = serde_json::from_slice(bytes).map_err(|e| RemoteError::Entry(format!("not an entry: {e}")))?;
    if wire.action != action.to_string() {
        return Err(RemoteError::Entry(format!("the entry is for {}, not {action}", wire.action)));
    }
    let Some(key) = trusted.iter().find(|key| key.name() == wire.key) else {
        return Err(RemoteError::Untrusted(wire.key));
    };
    let encoded = STANDARD.decode(&wire.result).map_err(|e| RemoteError::Entry(format!("result: {e}")))?;
    let signature = STANDARD.decode(&wire.signature).map_err(|e| RemoteError::Entry(format!("signature: {e}")))?;
    if !key.verify(&message(action, &encoded), &signature) {
        return Err(RemoteError::Entry(format!("the signature of {} does not match", wire.key)));
    }
    ActionResult::decode(&encoded).map_err(|e| RemoteError::Entry(format!("result: {e}")))
}

/// Bound into the signature of an image entry: "the image with this name has this record", where the name holds the
/// reference and the platform. An image entry lets a machine know the tree and the environment of an image without
/// pulling it (decision 0016). It replaces the alias entry of decision 0012, which held only the tree.
const IMAGE_CONTEXT: &[u8] = b"azad image entry v1\0";

#[derive(Serialize, Deserialize)]
struct ImageWire {
    name: String,
    /// The canonical encoding of the image record, in base64.
    image: String,
    key: String,
    signature: String,
}

fn image_message(name: &str, encoded_image: &[u8]) -> Vec<u8> {
    [IMAGE_CONTEXT, name.as_bytes(), b"\0", encoded_image].concat()
}

pub(crate) fn sign_image(name: &str, image: &Image, key: &SigningKey) -> Vec<u8> {
    let encoded = image.encode();
    let wire = ImageWire {
        name: name.to_owned(),
        image: STANDARD.encode(&encoded),
        key: key.name().to_owned(),
        signature: STANDARD.encode(key.sign(&image_message(name, &encoded))),
    };
    serde_json::to_vec(&wire).expect("an image entry serializes")
}

/// The record in an image entry, when the entry is for `name` and a trusted key signed it.
pub(crate) fn verify_image(bytes: &[u8], name: &str, trusted: &[PublicKey]) -> Result<Image, RemoteError> {
    let wire: ImageWire =
        serde_json::from_slice(bytes).map_err(|e| RemoteError::Entry(format!("not an image entry: {e}")))?;
    if wire.name != name {
        return Err(RemoteError::Entry(format!("the image entry is for {}, not {name}", wire.name)));
    }
    let Some(key) = trusted.iter().find(|key| key.name() == wire.key) else {
        return Err(RemoteError::Untrusted(wire.key));
    };
    let encoded = STANDARD.decode(&wire.image).map_err(|e| RemoteError::Entry(format!("image: {e}")))?;
    let signature = STANDARD.decode(&wire.signature).map_err(|e| RemoteError::Entry(format!("signature: {e}")))?;
    if !key.verify(&image_message(name, &encoded), &signature) {
        return Err(RemoteError::Entry(format!("the signature of {} does not match", wire.key)));
    }
    Image::decode(&encoded).map_err(|e| RemoteError::Entry(format!("image: {e}")))
}
