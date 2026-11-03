//! The remote cache that a command line names, such as `azad --cache gha`, with its keys from the environment and the
//! workspace.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::remote::gha::{self, GhaCache};
use crate::remote::{DirectoryProvider, Provider, PublicKey, RemoteConfig, RemoteError, SigningKey, parse_public_keys};

/// The secret signing key, from `azad key generate`, with which Azad may write the cache.
pub const SIGNING_KEY_ENV: &str = "AZAD_SIGNING_KEY";
const TRUSTED_KEYS_ENV: &str = "AZAD_TRUSTED_KEYS";
/// The public keys that a workspace trusts, relative to its root.
pub const TRUSTED_KEYS_FILE: &str = ".azad/trusted-keys";
const NONE: &str = "none";
const GHA: &str = "gha";
const DIRECTORY_PREFIX: &str = "dir:";

impl RemoteConfig {
    /// The remote cache that `spec` names: `gha`, `dir:<path>`, or `none` (or empty), which is no remote cache. It
    /// signs with `$AZAD_SIGNING_KEY`, and trusts the keys of the workspace at `root` and of `$AZAD_TRUSTED_KEYS`.
    pub fn from_spec(spec: &str, root: &Path) -> Result<Option<RemoteConfig>, RemoteError> {
        let (provider, (read, write)): (Box<dyn Provider>, _) = match spec {
            "" | NONE => return Ok(None),
            GHA => (Box::new(GhaCache::from_environment()?), gha::access_from_environment()),
            dir if dir.starts_with(DIRECTORY_PREFIX) => {
                let path = PathBuf::from(&dir[DIRECTORY_PREFIX.len()..]);
                let path = std::path::absolute(&path).map_err(|e| RemoteError::io(&path, e))?;
                (Box::new(DirectoryProvider::new(path)), (true, true))
            }
            other => return Err(RemoteError::Key(format!("--cache {other:?}: use gha, dir:<path> or none"))),
        };
        let signing_key = match env::var(SIGNING_KEY_ENV) {
            Ok(text) if !text.trim().is_empty() => {
                Some(text.parse::<SigningKey>().map_err(|e| RemoteError::Key(format!("{SIGNING_KEY_ENV}: {e}")))?)
            }
            _ => None,
        };
        Ok(Some(RemoteConfig { provider, read, write, signing_key, trusted: trusted_keys(root)? }))
    }
}

/// The keys in the workspace's `.azad/trusted-keys` and in `$AZAD_TRUSTED_KEYS` (separated by new lines or commas).
fn trusted_keys(root: &Path) -> Result<Vec<PublicKey>, RemoteError> {
    let file = root.join(TRUSTED_KEYS_FILE);
    let mut keys = match fs::read_to_string(&file) {
        Ok(text) => parse_public_keys(&text).map_err(|e| RemoteError::Key(format!("{}: {e}", file.display())))?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(RemoteError::io(&file, e)),
    };
    if let Ok(text) = env::var(TRUSTED_KEYS_ENV) {
        let more = parse_public_keys(&text.replace(',', "\n"))
            .map_err(|e| RemoteError::Key(format!("{TRUSTED_KEYS_ENV}: {e}")))?;
        keys.extend(more);
    }
    Ok(keys)
}
