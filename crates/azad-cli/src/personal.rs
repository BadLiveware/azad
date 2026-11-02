//! The personal defaults of the person who runs `azad`, from a file outside the repository (decision 0023): the tags to
//! include and exclude when the command line gives none, and where a local run runs tasks with an image (decision
//! 0024).

use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use azad::TagFilter;
use azad::frontend::LocalRun;
use serde::Deserialize;

/// The file of personal defaults; empty for none, as the Azad action sets it, so that CI never reads one.
const CONFIG_ENV: &str = "AZAD_CONFIG";
const CONFIG_HOME_ENV: &str = "XDG_CONFIG_HOME";
const HOME_ENV: &str = "HOME";
const CONFIG_DIR: &str = ".config";
const CONFIG_FILE: &str = "azad/config.toml";
const COMMAND_LINE: &str = "the command line";

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Personal {
    #[serde(default)]
    include: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
    local: Option<String>,
    /// The file that the defaults came from, for notes.
    #[serde(skip)]
    path: Option<PathBuf>,
}

/// Where the file of personal defaults is: `$AZAD_CONFIG`, or `azad/config.toml` in `$XDG_CONFIG_HOME` or `~/.config`.
fn path() -> Option<PathBuf> {
    if let Some(named) = env::var_os(CONFIG_ENV) {
        return (!named.is_empty()).then(|| PathBuf::from(named));
    }
    let config_home = env::var_os(CONFIG_HOME_ENV)
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| env::var_os(HOME_ENV).map(|home| PathBuf::from(home).join(CONFIG_DIR)))?;
    Some(config_home.join(CONFIG_FILE))
}

/// The personal defaults; none when there is no file of them.
pub(crate) fn read() -> Result<Personal> {
    let Some(path) = path() else { return Ok(Personal::default()) };
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Personal::default()),
        Err(error) => return Err(error).with_context(|| path.display().to_string()),
    };
    let personal: Personal = toml::from_str(&text).with_context(|| path.display().to_string())?;
    Ok(Personal { path: Some(path), ..personal })
}

impl Personal {
    /// The filter of the command line when it gives one; else the personal defaults.
    pub(crate) fn filter(&self, include: &[String], exclude: &[String]) -> TagFilter {
        let set = |tags: &[String]| tags.iter().cloned().collect::<BTreeSet<_>>();
        if !include.is_empty() || !exclude.is_empty() {
            return TagFilter { include: set(include), exclude: set(exclude), origin: COMMAND_LINE.to_owned() };
        }
        let origin = match &self.path {
            Some(path) => format!("the personal defaults in {}", path.display()),
            None => String::new(),
        };
        TagFilter { include: set(&self.include), exclude: set(&self.exclude), origin }
    }

    /// Where a local run runs every task with an image, when the defaults say.
    pub(crate) fn local(&self) -> Result<Option<LocalRun>> {
        let Some(local) = &self.local else { return Ok(None) };
        match LocalRun::parse(local) {
            Some(local) => Ok(Some(local)),
            None => bail!("`local` in the personal defaults is \"host\" or \"image\", not {local:?}"),
        }
    }
}
