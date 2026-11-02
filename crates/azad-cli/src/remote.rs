//! The remote cache from the command line and the environment (decision 0012).

use std::env;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use azad::remote::gha::{self, GhaCache};
use azad::remote::{DirectoryProvider, Provider, PublicKey, RemoteConfig, SigningKey, parse_public_keys};

const CACHE_ENV: &str = "AZAD_CACHE";
pub(crate) const SIGNING_KEY_ENV: &str = "AZAD_SIGNING_KEY";
const TRUSTED_KEYS_ENV: &str = "AZAD_TRUSTED_KEYS";
/// The public keys that a workspace trusts, relative to its root.
const TRUSTED_KEYS_FILE: &str = ".azad/trusted-keys";
const NONE: &str = "none";
const GHA: &str = "gha";
const DIRECTORY_PREFIX: &str = "dir:";

/// The remote cache that `--cache` or `$AZAD_CACHE` names: `gha`, `dir:<path>`, or `none`.
pub(crate) fn config(spec: Option<String>, root: &Path) -> Result<Option<RemoteConfig>> {
    let Some(spec) = spec.or_else(|| env::var(CACHE_ENV).ok()).filter(|s| !s.is_empty() && s != NONE) else {
        return Ok(None);
    };
    let (provider, (read, write)): (Box<dyn Provider>, _) = match spec.as_str() {
        GHA => (Box::new(GhaCache::from_environment()?), gha::access_from_environment()),
        dir if dir.starts_with(DIRECTORY_PREFIX) => {
            let path = PathBuf::from(&dir[DIRECTORY_PREFIX.len()..]);
            (Box::new(DirectoryProvider::new(std::path::absolute(path)?)), (true, true))
        }
        other => bail!("--cache {other:?}: use gha, dir:<path> or none"),
    };
    let signing_key = match env::var(SIGNING_KEY_ENV) {
        Ok(text) if !text.trim().is_empty() => Some(text.parse::<SigningKey>().context(SIGNING_KEY_ENV)?),
        _ => None,
    };
    Ok(Some(RemoteConfig { provider, read, write, signing_key, trusted: trusted_keys(root)? }))
}

/// The keys in the workspace's `.azad/trusted-keys` and in `$AZAD_TRUSTED_KEYS` (separated by new lines or commas).
fn trusted_keys(root: &Path) -> Result<Vec<PublicKey>> {
    let file = root.join(TRUSTED_KEYS_FILE);
    let mut keys = match fs::read_to_string(&file) {
        Ok(text) => parse_public_keys(&text).with_context(|| file.display().to_string())?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e).with_context(|| file.display().to_string()),
    };
    if let Ok(text) = env::var(TRUSTED_KEYS_ENV) {
        keys.extend(parse_public_keys(&text.replace(',', "\n")).context(TRUSTED_KEYS_ENV)?);
    }
    Ok(keys)
}

/// Prints a new signing key on standard output, and how to use it on standard error.
pub(crate) fn generate(name: &str) -> Result<ExitCode> {
    let key = SigningKey::generate(name)?;
    println!("{}", key.to_secret_string());
    eprintln!("This is a secret: keep it as {SIGNING_KEY_ENV} where Azad may write the cache, such as a CI secret.");
    eprintln!("Readers trust its public key when it is a line of {TRUSTED_KEYS_FILE}:");
    eprintln!("{}", key.public());
    Ok(ExitCode::SUCCESS)
}

/// Prints the public key of the signing key on standard input.
pub(crate) fn public() -> Result<ExitCode> {
    let mut text = String::new();
    io::stdin().read_to_string(&mut text)?;
    println!("{}", text.parse::<SigningKey>().context("standard input")?.public());
    Ok(ExitCode::SUCCESS)
}
