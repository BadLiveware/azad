//! Image references: `[registry/]repository[:tag]@sha256:<hex>`. The tag is ignored; the digest names the image.
//! A reference without a digest, `[registry/]repository[:tag]`, is a [`Tagged`] reference: it names what the tag
//! points to now, so it is only a way to find the digest.

const DOCKER_HUB: &str = "docker.io";
const DOCKER_HUB_API: &str = "registry-1.docker.io";
const DOCKER_OFFICIAL_PREFIX: &str = "library/";
const DIGEST_SEPARATOR: &str = "@sha256:";
const SHA256_HEX_LEN: usize = 64;
const TAG_SEPARATOR: char = ':';
const DEFAULT_TAG: &str = "latest";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reference {
    pub registry: String,
    pub repository: String,
    /// The hex sha256 of the manifest or index.
    pub digest: String,
}

impl Reference {
    pub fn parse(text: &str) -> Result<Self, String> {
        let (name, digest) = text
            .split_once(DIGEST_SEPARATOR)
            .ok_or_else(|| format!("image {text:?} must name a digest: <name>@sha256:<hex>"))?;
        let valid_digest =
            digest.len() == SHA256_HEX_LEN && digest.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if !valid_digest {
            return Err(format!("image {text:?}: the digest must be {SHA256_HEX_LEN} lower-case hex digits"));
        }
        let (name, _) = split_tag(name);
        let (registry, repository) = registry_and_repository(text, name)?;
        Ok(Reference { registry, repository, digest: digest.to_owned() })
    }

    /// The base URL of the registry API for this repository.
    pub fn api_base(&self) -> String {
        api_base(&self.registry, &self.repository)
    }
}

/// A reference by tag, such as `python:3.13-slim`: what the tag points to now. Without a tag, the tag is `latest`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tagged {
    /// The name as it was written, without the tag, such as `python` or `ghcr.io/owner/tool`.
    pub name: String,
    pub tag: String,
    pub registry: String,
    pub repository: String,
}

impl Tagged {
    pub fn parse(text: &str) -> Result<Self, String> {
        if has_digest(text) {
            return Err(format!("image {text:?} names a digest, not only a tag"));
        }
        let (name, tag) = split_tag(text);
        let tag = tag.unwrap_or(DEFAULT_TAG);
        let valid_tag =
            !tag.is_empty() && tag.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'));
        if !valid_tag {
            return Err(format!("image {text:?}: the tag {tag:?} may hold only letters, digits, '_', '.' and '-'"));
        }
        let (registry, repository) = registry_and_repository(text, name)?;
        Ok(Tagged { name: name.to_owned(), tag: tag.to_owned(), registry, repository })
    }

    /// The base URL of the registry API for this repository.
    pub fn api_base(&self) -> String {
        api_base(&self.registry, &self.repository)
    }

    /// The reference that names the image that the tag points to, by the digest of its manifest or index:
    /// `python:3.13-slim@sha256:<hex>`.
    pub fn with_digest(&self, digest_hex: &str) -> String {
        format!("{}{TAG_SEPARATOR}{}{DIGEST_SEPARATOR}{digest_hex}", self.name, self.tag)
    }
}

/// Whether a reference names a digest, such as `python@sha256:<hex>`, and not only a tag.
pub fn has_digest(text: &str) -> bool {
    text.contains(DIGEST_SEPARATOR)
}

/// The registry and the repository of a name without tag or digest. Docker Hub is the registry when the first part of
/// the name is not a host, and its official images are in `library/`.
fn registry_and_repository(text: &str, name: &str) -> Result<(String, String), String> {
    let (registry, repository) = match name.split_once('/') {
        Some((first, rest)) if is_registry(first) => (first.to_owned(), rest.to_owned()),
        _ => (DOCKER_HUB.to_owned(), name.to_owned()),
    };
    let repository = match registry == DOCKER_HUB && !repository.contains('/') {
        true => format!("{DOCKER_OFFICIAL_PREFIX}{repository}"),
        false => repository,
    };
    if repository.is_empty() || repository.ends_with('/') {
        return Err(format!("image {text:?} has no repository"));
    }
    Ok((registry, repository))
}

fn api_base(registry: &str, repository: &str) -> String {
    let host = match registry {
        DOCKER_HUB => DOCKER_HUB_API,
        other => other,
    };
    format!("https://{host}/v2/{repository}")
}

fn is_registry(first: &str) -> bool {
    first.contains(['.', ':']) || first == "localhost"
}

/// `name:tag` as the name and the tag; a `:` before the last `/` belongs to a registry port.
fn split_tag(name: &str) -> (&str, Option<&str>) {
    let last_part = name.rfind('/').map_or(0, |i| i + 1);
    match name[last_part..].find(TAG_SEPARATOR) {
        Some(colon) => (&name[..last_part + colon], Some(&name[last_part + colon + 1..])),
        None => (name, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn references_are_parsed() {
        let r = Reference::parse(&format!("golang:1.25-alpine@sha256:{HEX}")).unwrap();
        assert_eq!((r.registry.as_str(), r.repository.as_str()), ("docker.io", "library/golang"));
        assert_eq!(r.api_base(), "https://registry-1.docker.io/v2/library/golang");

        let r = Reference::parse(&format!("ghcr.io/owner/tool@sha256:{HEX}")).unwrap();
        assert_eq!((r.registry.as_str(), r.repository.as_str()), ("ghcr.io", "owner/tool"));

        let r = Reference::parse(&format!("localhost:5000/team/app:v1@sha256:{HEX}")).unwrap();
        assert_eq!((r.registry.as_str(), r.repository.as_str()), ("localhost:5000", "team/app"));

        let r = Reference::parse(&format!("someone/thing@sha256:{HEX}")).unwrap();
        assert_eq!((r.registry.as_str(), r.repository.as_str()), ("docker.io", "someone/thing"));
    }

    #[test]
    fn tagged_references_are_parsed() {
        let r = Tagged::parse("python:3.13-slim").unwrap();
        assert_eq!(
            (r.registry.as_str(), r.repository.as_str(), r.tag.as_str()),
            ("docker.io", "library/python", "3.13-slim")
        );
        assert_eq!(r.api_base(), "https://registry-1.docker.io/v2/library/python");
        assert_eq!(r.with_digest(HEX), format!("python:3.13-slim@sha256:{HEX}"));

        let r = Tagged::parse("localhost:5000/team/app").unwrap();
        assert_eq!(
            (r.registry.as_str(), r.repository.as_str(), r.tag.as_str()),
            ("localhost:5000", "team/app", "latest")
        );
        assert_eq!(r.with_digest(HEX), format!("localhost:5000/team/app:latest@sha256:{HEX}"));

        let r = Tagged::parse("ghcr.io/owner/tool:v1.2").unwrap();
        assert_eq!((r.registry.as_str(), r.repository.as_str(), r.tag.as_str()), ("ghcr.io", "owner/tool", "v1.2"));
        assert_eq!(Reference::parse(&r.with_digest(HEX)).unwrap().repository, r.repository);
    }

    #[test]
    fn a_tagged_reference_must_have_a_name_and_a_valid_tag() {
        assert!(Tagged::parse(&format!("python@sha256:{HEX}")).is_err());
        assert!(Tagged::parse("python:").is_err());
        assert!(Tagged::parse("python:3 13").is_err());
        assert!(Tagged::parse("ghcr.io/").is_err());
    }

    #[test]
    fn a_reference_without_a_valid_digest_is_refused() {
        assert!(Reference::parse("golang:1.25").is_err());
        assert!(Reference::parse("golang@sha256:abc").is_err());
        assert!(Reference::parse(&format!("golang@sha256:{}", HEX.to_uppercase())).is_err());
    }
}
