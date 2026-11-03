//! The remote cache from the command line and the environment (decision 0012).

use std::env;
use std::io::{self, Read};
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result};
use azad::remote::{RemoteConfig, SigningKey, TRUSTED_KEYS_FILE};

pub(crate) use azad::remote::SIGNING_KEY_ENV;

const CACHE_ENV: &str = "AZAD_CACHE";

/// The remote cache that `--cache` or `$AZAD_CACHE` names: `gha`, `dir:<path>`, or `none`.
pub(crate) fn config(spec: Option<String>, root: &Path) -> Result<Option<RemoteConfig>> {
    let spec = spec.or_else(|| env::var(CACHE_ENV).ok()).unwrap_or_default();
    Ok(RemoteConfig::from_spec(&spec, root)?)
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
