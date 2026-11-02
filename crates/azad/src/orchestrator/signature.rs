//! The signature of a webhook delivery: an HMAC (hash-based message authentication code) with SHA-256 of the payload,
//! under a secret that GitHub and the orchestrator share, in the header `X-Hub-Signature-256` as `sha256=<hex>`.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

pub const SIGNATURE_HEADER: &str = "x-hub-signature-256";
const PREFIX: &str = "sha256=";

pub fn sign(secret: &[u8], payload: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC takes a key of any length");
    mac.update(payload);
    let hex: String = mac.finalize().into_bytes().iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{PREFIX}{hex}")
}

/// Whether `signature` signs `payload` under `secret`, compared in constant time.
pub(super) fn verify(secret: &[u8], payload: &[u8], signature: &str) -> bool {
    let Some(bytes) = signature.strip_prefix(PREFIX).and_then(from_hex) else { return false };
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC takes a key of any length");
    mac.update(payload);
    mac.verify_slice(&bytes).is_ok()
}

fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The example of GitHub's documentation on validating webhook deliveries.
    #[test]
    fn a_signature_matches_githubs_example() {
        let signature = "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17";
        assert_eq!(sign(b"It's a Secret to Everybody", b"Hello, World!"), signature);
        assert!(verify(b"It's a Secret to Everybody", b"Hello, World!", signature));
        assert!(!verify(b"another secret", b"Hello, World!", signature));
        assert!(!verify(b"It's a Secret to Everybody", b"Hello, World?", signature));
        assert!(!verify(b"It's a Secret to Everybody", b"Hello, World!", "sha256=zz"));
    }
}
