//! Pulling OCI (Open Container Initiative) images into the store, as decision 0008 describes: by digest, with every
//! download checked, and the layers applied into one tree. The image's configuration gives its environment (decision
//! 0016).

mod layers;
mod reference;

pub use reference::{Reference, Tagged, has_digest};

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::Mutex;

use crate::ir::{Digest, Hasher, Image};
use crate::store::{Selection, State, Store, StoreError};
use serde::Deserialize;
use tempfile::{NamedTempFile, TempDir};

const MEDIA_OCI_INDEX: &str = "application/vnd.oci.image.index.v1+json";
const MEDIA_DOCKER_LIST: &str = "application/vnd.docker.distribution.manifest.list.v2+json";
const MEDIA_OCI_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
const MEDIA_DOCKER_MANIFEST: &str = "application/vnd.docker.distribution.manifest.v2+json";
const SHA256_PREFIX: &str = "sha256:";
const HTTP_UNAUTHORIZED: u16 = 401;
const HTTP_OK: u16 = 200;
const COPY_BUFFER: usize = 256 * 1024;
/// The largest index, manifest or configuration that is read into memory.
const DOCUMENT_LIMIT_BYTES: u64 = 16 * 1024 * 1024;
/// The aliases of images name image records (decision 0016). The names of decision 0008 started with `image` and named
/// bare trees; a new prefix makes sure that they are never read as records.
const ALIAS_PREFIX: &str = "image-record";
const ACCEPT_ANY: &str = "*/*";

type HttpResponse = ureq::http::Response<ureq::Body>;

#[derive(Debug, thiserror::Error)]
pub enum PullError {
    #[error("{0}")]
    Reference(String),
    #[error("{url}: {message}")]
    Http { url: String, message: String },
    #[error("{what} has the digest sha256:{actual}, but sha256:{expected} was asked for")]
    DigestMismatch { what: String, expected: String, actual: String },
    #[error("the image has no manifest for platform {0}")]
    NoPlatform(String),
    #[error("{0}")]
    Format(String),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("{path}: {error}")]
    Io { path: std::path::PathBuf, error: io::Error },
}

#[derive(Debug, Deserialize)]
struct Index {
    manifests: Vec<IndexEntry>,
}

#[derive(Debug, Deserialize)]
struct IndexEntry {
    digest: String,
    platform: Option<Platform>,
}

#[derive(Debug, Deserialize)]
struct Platform {
    os: String,
    architecture: String,
}

#[derive(Debug, Deserialize)]
struct Manifest {
    config: Descriptor,
    layers: Vec<Descriptor>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Descriptor {
    digest: String,
    media_type: String,
}

/// The parts of an image configuration that Azad uses.
#[derive(Debug, Deserialize)]
struct ImageConfig {
    config: Option<ContainerConfig>,
}

#[derive(Debug, Deserialize)]
struct ContainerConfig {
    #[serde(rename = "Env")]
    env: Option<Vec<String>>,
}

/// Pulls images into a store, and remembers the tree of each image and platform.
pub struct Puller {
    store: Store,
    cache: State,
    agent: ureq::Agent,
    token: Mutex<Option<String>>,
}

impl std::fmt::Debug for Puller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Puller").field("store", &self.store).finish_non_exhaustive()
    }
}

impl Puller {
    pub fn new(store: Store) -> Result<Self, PullError> {
        let cache = store.state();
        let agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
        Ok(Puller { store, cache, agent, token: Mutex::new(None) })
    }

    /// The name under which the record of an image for a platform is known, here and in remote caches.
    pub fn alias_name(reference: &str, platform: &str) -> Result<String, PullError> {
        let reference = Reference::parse(reference).map_err(PullError::Reference)?;
        Ok(alias(&reference, platform))
    }

    /// The record of an image, when it was pulled before and its tree is still in the store.
    pub fn known(&self, reference: &str, platform: &str) -> Result<Option<Image>, PullError> {
        let reference = Reference::parse(reference).map_err(PullError::Reference)?;
        self.known_image(&reference, platform)
    }

    /// Remembers the record of an image for a platform, so that it is not pulled again. The tree must be in the store.
    pub fn remember(&self, reference: &str, platform: &str, image: &Image) -> Result<(), PullError> {
        let reference = Reference::parse(reference).map_err(PullError::Reference)?;
        self.remember_image(&reference, platform, image)
    }

    /// Stores an image that a container runtime built from a Dockerfile (decision 0024, amendment): the file system in
    /// `archive`, a tar archive of all of it, with the `Env` entries of its configuration, remembered by `reference`.
    pub fn import(&self, reference: &str, platform: &str, archive: &Path, env: &[String]) -> Result<Image, PullError> {
        let reference = Reference::parse(reference).map_err(PullError::Reference)?;
        let env = env.iter().map(|entry| env_entry(entry)).collect::<Result<_, _>>()?;
        let tmp_root = self.store.tmp_dir();
        let staging = TempDir::new_in(&tmp_root).map_err(|source| PullError::Io { path: tmp_root, error: source })?;
        let rootfs = staging.path().join("rootfs");
        std::fs::create_dir(&rootfs).map_err(|source| PullError::Io { path: rootfs.clone(), error: source })?;
        layers::apply(archive, WHOLE_FILE_SYSTEM, &rootfs)?;
        let image = Image { tree: self.store.ingest(&rootfs, &Selection::All)?, env };
        self.remember_image(&reference, platform, &image)?;
        Ok(image)
    }

    /// The record of an image for a platform such as `linux/amd64`. An image is pulled and unpacked once.
    pub fn pull(&self, reference: &str, platform: &str) -> Result<Image, PullError> {
        let reference = Reference::parse(reference).map_err(PullError::Reference)?;
        if let Some(image) = self.known_image(&reference, platform)? {
            return Ok(image);
        }
        let manifest = self.manifest(&reference, platform)?;
        let env = config_env(&self.config_bytes(&reference, &manifest.config)?)?;
        let tmp_root = self.store.tmp_dir();
        let staging = TempDir::new_in(&tmp_root).map_err(|source| PullError::Io { path: tmp_root, error: source })?;
        let rootfs = staging.path().join("rootfs");
        std::fs::create_dir(&rootfs).map_err(|source| PullError::Io { path: rootfs.clone(), error: source })?;
        let blobs = self.download_blobs(&reference, &manifest.layers, staging.path())?;
        for (layer, blob) in manifest.layers.iter().zip(&blobs) {
            layers::apply(blob.path(), &layer.media_type, &rootfs)?;
        }
        let image = Image { tree: self.store.ingest(&rootfs, &Selection::All)?, env };
        self.remember_image(&reference, platform, &image)?;
        Ok(image)
    }

    fn known_image(&self, reference: &Reference, platform: &str) -> Result<Option<Image>, PullError> {
        let Some(record) = self.cache.alias(&alias(reference, platform))? else {
            return Ok(None);
        };
        let image: Image = self.store.get(&record)?;
        Ok(self.store.contains(&image.tree).then_some(image))
    }

    fn remember_image(&self, reference: &Reference, platform: &str, image: &Image) -> Result<(), PullError> {
        let record = self.store.put(image)?;
        Ok(self.cache.set_alias(&alias(reference, platform), &record)?)
    }

    fn manifest(&self, reference: &Reference, platform: &str) -> Result<Manifest, PullError> {
        let (os, architecture) =
            platform.split_once('/').ok_or_else(|| PullError::Reference(format!("bad platform {platform:?}")))?;
        let top = self.manifest_bytes(reference, &reference.digest)?;
        let bytes = match serde_json::from_slice::<Index>(&top) {
            Ok(index) => {
                let entry = index
                    .manifests
                    .iter()
                    .find(|m| m.platform.as_ref().is_some_and(|p| p.os == os && p.architecture == architecture))
                    .ok_or_else(|| PullError::NoPlatform(platform.to_owned()))?;
                self.manifest_bytes(reference, strip_sha256(&entry.digest)?)?
            }
            Err(_) => top,
        };
        serde_json::from_slice(&bytes).map_err(|e| PullError::Format(format!("manifest: {e}")))
    }

    fn manifest_bytes(&self, reference: &Reference, digest: &str) -> Result<Vec<u8>, PullError> {
        let url = format!("{}/manifests/{SHA256_PREFIX}{digest}", reference.api_base());
        self.document(reference, &url, &manifest_accept(), digest)
    }

    /// The reference, by digest, of the image that a tag points to now: the digest of the index when the image has
    /// one, as `image()` takes it, not of the manifest of one platform. A reference with a digest is returned as it is.
    pub fn pin(&self, text: &str) -> Result<String, PullError> {
        if has_digest(text) {
            return Reference::parse(text).map(|_| text.to_owned()).map_err(PullError::Reference);
        }
        let tagged = Tagged::parse(text).map_err(PullError::Reference)?;
        let url = format!("{}/manifests/{}", tagged.api_base(), tagged.tag);
        let response = self.get(&tagged.repository, &url, &manifest_accept())?;
        let bytes = read_document(&url, response)?;
        Ok(tagged.with_digest(&Digest::of_bytes(&bytes).hex()))
    }

    fn config_bytes(&self, reference: &Reference, config: &Descriptor) -> Result<Vec<u8>, PullError> {
        let digest = strip_sha256(&config.digest)?;
        let url = format!("{}/blobs/{SHA256_PREFIX}{digest}", reference.api_base());
        self.document(reference, &url, ACCEPT_ANY, digest)
    }

    /// A small document, such as a manifest, read into memory and checked against its digest.
    fn document(&self, reference: &Reference, url: &str, accept: &str, digest: &str) -> Result<Vec<u8>, PullError> {
        let response = self.get(&reference.repository, url, accept)?;
        let bytes = read_document(url, response)?;
        let actual = Digest::of_bytes(&bytes).hex();
        if actual != digest {
            return Err(PullError::DigestMismatch { what: url.to_owned(), expected: digest.to_owned(), actual });
        }
        Ok(bytes)
    }

    /// Downloads the layers at the same time, because a pull waits mostly for the network; they are applied in order
    /// after.
    fn download_blobs(
        &self,
        reference: &Reference,
        layers: &[Descriptor],
        dir: &Path,
    ) -> Result<Vec<NamedTempFile>, PullError> {
        std::thread::scope(|scope| {
            let downloads: Vec<_> =
                layers.iter().map(|layer| scope.spawn(move || self.download_blob(reference, layer, dir))).collect();
            downloads.into_iter().map(|download| download.join().expect("a download thread does not panic")).collect()
        })
    }

    fn download_blob(&self, reference: &Reference, layer: &Descriptor, dir: &Path) -> Result<NamedTempFile, PullError> {
        let expected = strip_sha256(&layer.digest)?;
        let url = format!("{}/blobs/{SHA256_PREFIX}{expected}", reference.api_base());
        let response = self.get(&reference.repository, &url, ACCEPT_ANY)?;
        let mut reader = response.into_body().into_reader();
        let mut file =
            NamedTempFile::new_in(dir).map_err(|source| PullError::Io { path: dir.to_owned(), error: source })?;
        let mut hasher = Hasher::new();
        let mut buffer = vec![0u8; COPY_BUFFER];
        loop {
            let n =
                reader.read(&mut buffer).map_err(|e| PullError::Http { url: url.clone(), message: e.to_string() })?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
            file.write_all(&buffer[..n])
                .map_err(|source| PullError::Io { path: file.path().to_owned(), error: source })?;
        }
        let actual = hasher.finish().hex();
        if actual != expected {
            return Err(PullError::DigestMismatch { what: url, expected: expected.to_owned(), actual });
        }
        Ok(file)
    }

    /// A GET with the registry's bearer token, which is fetched when the registry asks for one.
    fn get(&self, repository: &str, url: &str, accept: &str) -> Result<HttpResponse, PullError> {
        let http_error = |e: ureq::Error| PullError::Http { url: url.to_owned(), message: e.to_string() };
        let token = self.token.lock().expect("the token lock is not poisoned").clone();
        let response = self.request(url, accept, token.as_deref()).map_err(http_error)?;
        if response.status().as_u16() != HTTP_UNAUTHORIZED {
            return check_status(url, response);
        }
        let challenge = response
            .headers()
            .get("www-authenticate")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| PullError::Http { url: url.to_owned(), message: "401 without a challenge".to_owned() })?
            .to_owned();
        let token = self.fetch_token(&challenge, repository)?;
        *self.token.lock().expect("the token lock is not poisoned") = Some(token.clone());
        let response = self.request(url, accept, Some(&token)).map_err(http_error)?;
        check_status(url, response)
    }

    fn request(&self, url: &str, accept: &str, token: Option<&str>) -> Result<HttpResponse, ureq::Error> {
        let mut request = self.agent.get(url).header("Accept", accept);
        if let Some(token) = token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        request.call()
    }

    fn fetch_token(&self, challenge: &str, repository: &str) -> Result<String, PullError> {
        #[derive(Deserialize)]
        struct TokenResponse {
            token: Option<String>,
            access_token: Option<String>,
        }
        let params = parse_challenge(challenge).ok_or_else(|| PullError::Http {
            url: challenge.to_owned(),
            message: "not a bearer challenge".to_owned(),
        })?;
        let param = |key: &str| params.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()).unwrap_or_default();
        let url = format!("{}?service={}&scope=repository:{repository}:pull", param("realm"), param("service"));
        let http_error = |e: ureq::Error| PullError::Http { url: url.clone(), message: e.to_string() };
        let mut response = self.agent.get(&url).call().map_err(http_error)?;
        if response.status().as_u16() != HTTP_OK {
            return Err(PullError::Http { url: url.clone(), message: format!("status {}", response.status()) });
        }
        let bytes = response.body_mut().read_to_vec().map_err(http_error)?;
        let body: TokenResponse =
            serde_json::from_slice(&bytes).map_err(|e| PullError::Format(format!("token response: {e}")))?;
        body.token.or(body.access_token).ok_or_else(|| PullError::Format("the token response has no token".to_owned()))
    }
}

/// The body of a small document, such as a manifest, up to the limit of what is read into memory.
fn read_document(url: &str, mut response: HttpResponse) -> Result<Vec<u8>, PullError> {
    response
        .body_mut()
        .with_config()
        .limit(DOCUMENT_LIMIT_BYTES)
        .read_to_vec()
        .map_err(|e| PullError::Http { url: url.to_owned(), message: e.to_string() })
}

/// The media types of an index and of a manifest, in OCI and Docker form.
fn manifest_accept() -> String {
    [MEDIA_OCI_INDEX, MEDIA_DOCKER_LIST, MEDIA_OCI_MANIFEST, MEDIA_DOCKER_MANIFEST].join(", ")
}

fn alias(reference: &Reference, platform: &str) -> String {
    format!("{ALIAS_PREFIX} {}@{SHA256_PREFIX}{} {platform}", reference.repository, reference.digest)
}

/// The `Env` of an image configuration, as variables. A later entry for a name replaces an earlier one.
/// The media type of a whole file system that a runtime exported: an uncompressed tar, as a layer of one.
const WHOLE_FILE_SYSTEM: &str = "application/vnd.oci.image.layer.v1.tar";

fn config_env(bytes: &[u8]) -> Result<BTreeMap<String, String>, PullError> {
    let config: ImageConfig =
        serde_json::from_slice(bytes).map_err(|e| PullError::Format(format!("image configuration: {e}")))?;
    let entries = config.config.and_then(|c| c.env).unwrap_or_default();
    entries.iter().map(|entry| env_entry(entry)).collect()
}

fn env_entry(entry: &str) -> Result<(String, String), PullError> {
    let Some((name, value)) = entry.split_once('=') else {
        return Err(PullError::Format(format!(
            "the image configuration has the Env entry {entry:?}, which is not NAME=value"
        )));
    };
    Ok((name.to_owned(), value.to_owned()))
}

fn check_status(url: &str, response: HttpResponse) -> Result<HttpResponse, PullError> {
    match response.status().as_u16() {
        HTTP_OK => Ok(response),
        status => Err(PullError::Http { url: url.to_owned(), message: format!("status {status}") }),
    }
}

fn strip_sha256(digest: &str) -> Result<&str, PullError> {
    digest
        .strip_prefix(SHA256_PREFIX)
        .ok_or_else(|| PullError::Format(format!("only sha256 digests are supported, not {digest:?}")))
}

/// The parameters of a `Bearer realm="...",service="...",scope="..."` challenge.
fn parse_challenge(challenge: &str) -> Option<Vec<(String, String)>> {
    let rest = challenge.strip_prefix("Bearer ")?;
    let mut params = Vec::new();
    for part in rest.split(',') {
        let (key, value) = part.trim().split_once('=')?;
        params.push((key.to_owned(), value.trim_matches('"').to_owned()));
    }
    Some(params)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bearer_challenge_is_parsed() {
        let params = parse_challenge(
            r#"Bearer realm="https://auth.docker.io/token",service="registry.docker.io",scope="repository:library/golang:pull""#,
        )
        .unwrap();
        assert_eq!(params[0], ("realm".to_owned(), "https://auth.docker.io/token".to_owned()));
        assert_eq!(params[1], ("service".to_owned(), "registry.docker.io".to_owned()));
        assert_eq!(parse_challenge("Basic realm=x"), None);
    }

    #[test]
    fn the_env_of_an_image_configuration_is_read_as_variables() {
        let python = br#"{"architecture":"amd64","config":{"Env":["PATH=/usr/local/bin:/usr/bin:/bin","LANG=C.UTF-8","EMPTY=","EQUALS=a=b"],"Cmd":["python3"]},"os":"linux"}"#;
        let env = config_env(python).unwrap();
        let expected =
            [("PATH", "/usr/local/bin:/usr/bin:/bin"), ("LANG", "C.UTF-8"), ("EMPTY", ""), ("EQUALS", "a=b")]
                .map(|(name, value)| (name.to_owned(), value.to_owned()));
        assert_eq!(env, BTreeMap::from(expected));
        assert_eq!(config_env(br#"{"config":{"Env":null}}"#).unwrap(), BTreeMap::new());
        assert_eq!(config_env(br#"{"architecture":"amd64"}"#).unwrap(), BTreeMap::new());
        let error = config_env(br#"{"config":{"Env":["NOVALUE"]}}"#).unwrap_err().to_string();
        assert!(error.contains("\"NOVALUE\", which is not NAME=value"), "{error}");
    }
}
