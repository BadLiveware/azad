//! `semver`: semantic versions as values, with Cargo's rules (decision 0021).
//!
//! The derive macros of the `starlark` crate (`ProvidesStaticType`) generate `unsafe impl` blocks, so this module allows
//! `unsafe_code`; it has no `unsafe` code of its own.
#![allow(unsafe_code)]

use std::cmp::Ordering;
use std::fmt;
use std::hash::Hash;

use allocative::Allocative;
use semver::{BuildMetadata, Prerelease, Version, VersionReq};
use starlark::any::ProvidesStaticType;
use starlark::collections::StarlarkHasher;
use starlark::environment::{GlobalsBuilder, Methods, MethodsBuilder};
use starlark::values::{NoSerialize, StarlarkValue, Value, ValueLike, starlark_value};
use starlark::{StarlarkPagablePanic, starlark_module, starlark_simple_value};

/// A semantic version.
#[derive(Debug, Clone, PartialEq, Eq, ProvidesStaticType, NoSerialize, Allocative, StarlarkPagablePanic)]
pub(crate) struct SemverValue {
    #[allocative(skip)]
    version: Version,
}

starlark_simple_value!(SemverValue);

impl fmt::Display for SemverValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.version)
    }
}

#[starlark_value(type = "semver")]
impl<'v> StarlarkValue<'v> for SemverValue {
    fn get_methods() -> Option<&'static Methods> {
        Some(SEMVER_METHODS.methods())
    }

    fn equals(&self, other: Value<'v>) -> starlark::Result<bool> {
        Ok(other.downcast_ref::<SemverValue>().is_some_and(|other| other.version == self.version))
    }

    fn compare(&self, other: Value<'v>) -> starlark::Result<Ordering> {
        match other.downcast_ref::<SemverValue>() {
            Some(other) => Ok(self.version.cmp(&other.version)),
            None => Err(starlark::Error::new_other(anyhow::anyhow!(
                "a semver compares only with a semver, not with {}",
                other.get_type()
            ))),
        }
    }

    fn write_hash(&self, hasher: &mut StarlarkHasher) -> starlark::Result<()> {
        self.version.hash(hasher);
        Ok(())
    }
}

/// The part of a version that `bump` raises.
#[derive(Debug, Clone, Copy)]
enum Part {
    Major,
    Minor,
    Patch,
}

impl Part {
    fn parse(text: &str) -> anyhow::Result<Self> {
        match text {
            "major" => Ok(Part::Major),
            "minor" => Ok(Part::Minor),
            "patch" => Ok(Part::Patch),
            other => anyhow::bail!("bump({other:?}): the part is \"major\", \"minor\" or \"patch\""),
        }
    }
}

/// The next release that raises `part`. A pre-release of the version that a bump would give is released as it is:
/// `1.3.0-rc.1` bumped at `minor` is `1.3.0`, because it comes before `1.3.0`.
fn bump(version: &Version, part: Part) -> Version {
    let releases_itself = !version.pre.is_empty()
        && match part {
            Part::Major => version.minor == 0 && version.patch == 0,
            Part::Minor => version.patch == 0,
            Part::Patch => true,
        };
    let (major, minor, patch) = match (releases_itself, part) {
        (true, _) => (version.major, version.minor, version.patch),
        (false, Part::Major) => (version.major + 1, 0, 0),
        (false, Part::Minor) => (version.major, version.minor + 1, 0),
        (false, Part::Patch) => (version.major, version.minor, version.patch + 1),
    };
    Version { major, minor, patch, pre: Prerelease::EMPTY, build: BuildMetadata::EMPTY }
}

fn parse(text: &str) -> anyhow::Result<Version> {
    Version::parse(text).map_err(|e| anyhow::anyhow!("{text:?} is not a semantic version: {e}"))
}

starlark::methods_static!(SEMVER_METHODS = semver_methods);

#[starlark_module]
fn semver_methods(builder: &mut MethodsBuilder) {
    /// The major version: `1` of `1.4.2`.
    #[starlark(attribute)]
    fn major(this: &SemverValue) -> anyhow::Result<u64> {
        Ok(this.version.major)
    }

    /// The minor version: `4` of `1.4.2`.
    #[starlark(attribute)]
    fn minor(this: &SemverValue) -> anyhow::Result<u64> {
        Ok(this.version.minor)
    }

    /// The patch version: `2` of `1.4.2`.
    #[starlark(attribute)]
    fn patch(this: &SemverValue) -> anyhow::Result<u64> {
        Ok(this.version.patch)
    }

    /// The pre-release, such as `rc.1` of `1.4.2-rc.1`; empty for a release.
    #[starlark(attribute)]
    fn pre(this: &SemverValue) -> anyhow::Result<String> {
        Ok(this.version.pre.to_string())
    }

    /// The build metadata, such as `abc123` of `1.4.2+abc123`; empty when there is none.
    #[starlark(attribute)]
    fn build(this: &SemverValue) -> anyhow::Result<String> {
        Ok(this.version.build.to_string())
    }

    /// The next release that raises `part`, without pre-release or build metadata.
    ///
    /// A pre-release of the release that the bump gives is released as it is, because it comes before it:
    /// `1.3.0-rc.1` bumped at `minor` is `1.3.0`, and at `patch` too; at `major` it is `2.0.0`.
    ///
    /// # Arguments
    /// * `part`: `"major"`, `"minor"` or `"patch"`.
    fn bump(this: &SemverValue, part: &str) -> anyhow::Result<SemverValue> {
        Ok(SemverValue { version: bump(&this.version, Part::parse(part)?) })
    }
}

/// The functions of the namespace `semver`.
#[starlark_module]
pub(crate) fn semver_functions(builder: &mut GlobalsBuilder) {
    /// A semantic version (semver.org), parsed with Cargo's rules.
    ///
    /// Versions compare by precedence, so `max()` and `sorted()` work on them: `1.4.0-rc.1 < 1.4.0 < 1.10.0`.
    /// Two versions that differ only in build metadata are not equal, and compare by that metadata.
    /// `str()` gives the version as text.
    ///
    /// # Arguments
    /// * `text`: The version, such as `1.4.2`, `1.4.2-rc.1` or `1.4.2+build.7`; no `v` before it.
    ///
    /// Examples:
    /// ```python
    /// version = semver.parse("1.4.2")
    /// next = version.bump("minor")    # 1.5.0
    /// ```
    fn parse(text: &str) -> anyhow::Result<SemverValue> {
        Ok(SemverValue { version: parse(text)? })
    }

    /// Whether `text` is a semantic version that `semver.parse()` takes.
    ///
    /// # Arguments
    /// * `text`: The text to check, such as the part of a tag after its `v`.
    fn valid(text: &str) -> anyhow::Result<bool> {
        Ok(Version::parse(text).is_ok())
    }

    /// Whether `version` meets `requirement`, with Cargo's rules: `^1.4` holds `1.4.0` up to, and not with, `2.0.0`.
    ///
    /// A pre-release meets a requirement only when the requirement names a pre-release of the same version.
    ///
    /// # Arguments
    /// * `requirement`: Such as `^1.4`, `~1.4.2`, `>=1.2, <1.5` or `=1.4.2`.
    /// * `version`: A version from `semver.parse()`, or its text.
    fn matches<'v>(requirement: &str, version: Value<'v>) -> anyhow::Result<bool> {
        let requirement = VersionReq::parse(requirement)
            .map_err(|e| anyhow::anyhow!("{requirement:?} is not a version requirement: {e}"))?;
        let version = match (version.downcast_ref::<SemverValue>(), version.unpack_str()) {
            (Some(value), _) => value.version.clone(),
            (None, Some(text)) => parse(text)?,
            (None, None) => anyhow::bail!("matches(): version must be a semver or text, not {}", version.get_type()),
        };
        Ok(requirement.matches(&version))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bumped(text: &str, part: Part) -> String {
        bump(&parse(text).unwrap(), part).to_string()
    }

    #[test]
    fn a_bump_raises_its_part_and_resets_the_parts_below() {
        assert_eq!(bumped("1.4.2", Part::Major), "2.0.0");
        assert_eq!(bumped("1.4.2", Part::Minor), "1.5.0");
        assert_eq!(bumped("1.4.2+build.7", Part::Patch), "1.4.3");
    }

    #[test]
    fn a_pre_release_of_the_bumped_release_is_released_as_it_is() {
        assert_eq!(bumped("1.3.0-rc.1", Part::Minor), "1.3.0");
        assert_eq!(bumped("1.3.0-rc.1", Part::Patch), "1.3.0");
        assert_eq!(bumped("1.3.0-rc.1", Part::Major), "2.0.0");
        assert_eq!(bumped("2.0.0-alpha", Part::Major), "2.0.0");
        assert_eq!(bumped("1.3.1-rc.1", Part::Minor), "1.4.0");
    }
}
