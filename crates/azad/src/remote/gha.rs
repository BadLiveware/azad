//! The GitHub Actions cache, through the cache service that `actions/cache` uses (version 2, Twirp with JSON).
//!
//! GitHub gives the service's address and token only to JavaScript actions, in `ACTIONS_RESULTS_URL` and
//! `ACTIONS_RUNTIME_TOKEN`; the Azad action passes them on. An entry is uploaded to and downloaded from Azure Blob
//! Storage with a signed URL. Entries do not change, and are scoped by branch: a run reads the entries of its own
//! branch and of the default branch.

use std::env;
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::remote::{Provider, RemoteError};

const RESULTS_URL_ENV: &str = "ACTIONS_RESULTS_URL";
const RUNTIME_TOKEN_ENV: &str = "ACTIONS_RUNTIME_TOKEN";
const CACHE_MODE_ENV: &str = "ACTIONS_CACHE_MODE";
/// The variables that give the cache service's address and token, which commands must not read.
pub const SECRET_VARIABLES: [&str; 2] = [RESULTS_URL_ENV, RUNTIME_TOKEN_ENV];
const SERVICE: &str = "github.actions.results.api.v1.CacheService";
/// The service scopes entries by a version as well as a key; `actions/cache` hashes its paths and compression into it.
const VERSION_SALT: &str = "azad-objects-1";
/// Azure takes one request up to this size; larger objects go in blocks of this size.
const BLOCK_SIZE: usize = 64 * 1024 * 1024;
/// Downloads of large objects: ranges of this size, this many at a time.
const RANGE_SIZE: u64 = 32 * 1024 * 1024;
const DOWNLOADS_AT_ONCE: usize = 8;
const COPY_BUFFER_SIZE: usize = 1024 * 1024;
const RANGE_HEADER: &str = "Range";
const CONTENT_RANGE_HEADER: &str = "Content-Range";
const HTTP_PARTIAL_CONTENT: u16 = 206;
const HTTP_CONFLICT: u16 = 409;
const HTTP_NOT_FOUND: u16 = 404;
const WRITE_DENIED: &str = "cache write denied";

/// What `ACTIONS_CACHE_MODE` allows: (read, write). An unknown mode allows both, as `actions/cache` does.
pub fn access_from_environment() -> (bool, bool) {
    let mode = env::var(CACHE_MODE_ENV).unwrap_or_default();
    match mode.trim().to_lowercase().as_str() {
        "none" => (false, false),
        "read" => (true, false),
        "write-only" => (false, true),
        _ => (true, true),
    }
}

pub struct GhaCache {
    agent: ureq::Agent,
    base: String,
    token: String,
    version: String,
}

#[derive(Serialize)]
struct CreateRequest<'a> {
    key: &'a str,
    version: &'a str,
}

#[derive(Deserialize)]
struct CreateResponse {
    ok: bool,
    #[serde(alias = "signedUploadUrl", default)]
    signed_upload_url: String,
    #[serde(default)]
    message: String,
}

#[derive(Serialize)]
struct FinalizeRequest<'a> {
    key: &'a str,
    version: &'a str,
    /// An int64, which protobuf's JSON writes as a string.
    size_bytes: String,
}

#[derive(Deserialize)]
struct FinalizeResponse {
    ok: bool,
    #[serde(default)]
    message: String,
}

#[derive(Serialize)]
struct DownloadRequest<'a> {
    key: &'a str,
    restore_keys: Vec<&'a str>,
    version: &'a str,
}

#[derive(Deserialize)]
struct DownloadResponse {
    ok: bool,
    #[serde(alias = "signedDownloadUrl", default)]
    signed_download_url: String,
    #[serde(alias = "matchedKey", default)]
    matched_key: String,
}

/// A Twirp call that the service refused, with its HTTP status and message.
struct Refused {
    status: u16,
    message: String,
}

impl GhaCache {
    pub fn from_environment() -> Result<Self, RemoteError> {
        let (Ok(base), Ok(token)) = (env::var(RESULTS_URL_ENV), env::var(RUNTIME_TOKEN_ENV)) else {
            return Err(RemoteError::Http(format!(
                "the GHA cache needs {RESULTS_URL_ENV} and {RUNTIME_TOKEN_ENV}, which GitHub gives only to JavaScript \
                 actions: run Azad with its GitHub Action"
            )));
        };
        let agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
        let version: String = Sha256::digest(VERSION_SALT.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
        Ok(GhaCache { agent, base: base.trim_end_matches('/').to_owned(), token, version })
    }

    fn call<Q: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        request: &Q,
    ) -> Result<Result<R, Refused>, RemoteError> {
        let url = format!("{}/twirp/{SERVICE}/{method}", self.base);
        let body = serde_json::to_vec(request).expect("a request serializes");
        let mut response = self
            .agent
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Content-Type", "application/json")
            .send(&body[..])
            .map_err(|e| RemoteError::Http(format!("{method}: {e}")))?;
        let status = response.status().as_u16();
        let text = response.body_mut().read_to_string().map_err(|e| RemoteError::Http(format!("{method}: {e}")))?;
        if !response.status().is_success() {
            return Ok(Err(Refused { status, message: text }));
        }
        serde_json::from_str(&text).map(Ok).map_err(|e| RemoteError::Http(format!("{method}: {e}: {text}")))
    }

    fn download_url(&self, key: &str) -> Result<Option<String>, RemoteError> {
        let request = DownloadRequest { key, restore_keys: Vec::new(), version: &self.version };
        match self.call::<_, DownloadResponse>("GetCacheEntryDownloadURL", &request)? {
            Ok(found) if found.ok && found.matched_key == key => Ok(Some(found.signed_download_url)),
            Ok(_) => Ok(None),
            Err(refused) if refused.status == HTTP_NOT_FOUND => Ok(None),
            Err(refused) => Err(RemoteError::Http(format!("GetCacheEntryDownloadURL: {}", refused.message))),
        }
    }

    /// The download URL of the newest entry whose key starts with `prefix`; the service gives the newest entry that a
    /// restore key matches, as `actions/cache` uses it.
    fn newest_download_url(&self, prefix: &str) -> Result<Option<String>, RemoteError> {
        let request = DownloadRequest { key: prefix, restore_keys: vec![prefix], version: &self.version };
        match self.call::<_, DownloadResponse>("GetCacheEntryDownloadURL", &request)? {
            Ok(found) if found.ok && found.matched_key.starts_with(prefix) => Ok(Some(found.signed_download_url)),
            Ok(_) => Ok(None),
            Err(refused) if refused.status == HTTP_NOT_FOUND => Ok(None),
            Err(refused) => Err(RemoteError::Http(format!("GetCacheEntryDownloadURL: {}", refused.message))),
        }
    }

    /// Uploads a file to a signed Azure URL: in one request when it is small, else in blocks and a block list.
    fn upload(&self, url: &str, source: &Path) -> Result<u64, RemoteError> {
        let mut file = File::open(source).map_err(|e| RemoteError::io(source, e))?;
        let size = file.metadata().map_err(|e| RemoteError::io(source, e))?.len();
        if size <= BLOCK_SIZE as u64 {
            let mut bytes = Vec::with_capacity(size as usize);
            file.read_to_end(&mut bytes).map_err(|e| RemoteError::io(source, e))?;
            self.azure_put(url, "x-ms-blob-type", "BlockBlob", &bytes)?;
            return Ok(size);
        }
        let mut ids = Vec::new();
        let mut buffer = vec![0u8; BLOCK_SIZE];
        loop {
            let n = read_full(&mut file, &mut buffer).map_err(|e| RemoteError::io(source, e))?;
            if n == 0 {
                break;
            }
            let id = STANDARD.encode(format!("azad-block-{:08}", ids.len()));
            let block_url = format!("{url}&comp=block&blockid={}", percent_encode(&id));
            self.azure_put(&block_url, "Content-Type", "application/octet-stream", &buffer[..n])?;
            ids.push(id);
        }
        let list: String = ids.iter().map(|id| format!("<Latest>{id}</Latest>")).collect();
        let body = format!("<?xml version=\"1.0\" encoding=\"utf-8\"?><BlockList>{list}</BlockList>");
        self.azure_put(&format!("{url}&comp=blocklist"), "Content-Type", "application/xml", body.as_bytes())?;
        Ok(size)
    }

    fn azure_put(&self, url: &str, header: &str, value: &str, body: &[u8]) -> Result<(), RemoteError> {
        let mut response = self
            .agent
            .put(url)
            .header(header, value)
            .send(body)
            .map_err(|e| RemoteError::Http(format!("upload: {e}")))?;
        if response.status().is_success() {
            return Ok(());
        }
        let text = response.body_mut().read_to_string().unwrap_or_default();
        Err(RemoteError::Http(format!("upload: {}: {text}", response.status())))
    }
}

impl Provider for GhaCache {
    fn name(&self) -> String {
        "gha".to_owned()
    }

    fn contains(&self, key: &str) -> Result<bool, RemoteError> {
        Ok(self.download_url(key)?.is_some())
    }

    fn get(&self, key: &str, dest: &Path) -> Result<bool, RemoteError> {
        let Some(url) = self.download_url(key)? else { return Ok(false) };
        self.download(&url, key, dest)
    }

    fn get_newest(&self, prefix: &str, dest: &Path) -> Result<bool, RemoteError> {
        let Some(url) = self.newest_download_url(prefix)? else { return Ok(false) };
        self.download(&url, prefix, dest)
    }

    fn put(&self, key: &str, source: &Path) -> Result<(), RemoteError> {
        self.create(key, source)
    }
}

impl GhaCache {
    /// Downloads an object: a large one in ranges at the same time, as `actions/cache` does, because one stream from the
    /// storage service is much slower than the runner's network.
    fn download(&self, url: &str, key: &str, dest: &Path) -> Result<bool, RemoteError> {
        let file = File::create(dest).map_err(|e| RemoteError::io(dest, e))?;
        let size = match self.size_of(url, key)? {
            Some(size) if size > RANGE_SIZE => size,
            _ => {
                self.download_range(url, key, None, &file, dest)?;
                return Ok(true);
            }
        };
        file.set_len(size).map_err(|e| RemoteError::io(dest, e))?;
        let ranges = ranges(size, RANGE_SIZE);
        let next = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            let workers: Vec<_> = (0..DOWNLOADS_AT_ONCE)
                .map(|_| {
                    scope.spawn(|| -> Result<(), RemoteError> {
                        while let Some(range) = ranges.get(next.fetch_add(1, Ordering::Relaxed)) {
                            self.download_range(url, key, Some(*range), &file, dest)?;
                        }
                        Ok(())
                    })
                })
                .collect();
            workers.into_iter().try_for_each(|worker| worker.join().expect("a download thread does not panic"))
        })?;
        Ok(true)
    }

    /// The size of an object, from the answer to a request for its first byte; `None` when the service does not give
    /// ranges.
    fn size_of(&self, url: &str, key: &str) -> Result<Option<u64>, RemoteError> {
        let response = self
            .agent
            .get(url)
            .header(RANGE_HEADER, "bytes=0-0")
            .call()
            .map_err(|e| RemoteError::Http(format!("download of {key}: {e}")))?;
        if response.status().as_u16() != HTTP_PARTIAL_CONTENT {
            return Ok(None);
        }
        let total = response.headers().get(CONTENT_RANGE_HEADER).and_then(|value| value.to_str().ok());
        Ok(total.and_then(|text| text.rsplit_once('/')).and_then(|(_, size)| size.parse().ok()))
    }

    /// Writes a range of an object, or all of it, at its place in `file`.
    fn download_range(
        &self,
        url: &str,
        key: &str,
        range: Option<(u64, u64)>,
        file: &File,
        dest: &Path,
    ) -> Result<(), RemoteError> {
        let mut request = self.agent.get(url);
        if let Some((start, end)) = range {
            request = request.header(RANGE_HEADER, format!("bytes={start}-{end}"));
        }
        let mut response = request.call().map_err(|e| RemoteError::Http(format!("download of {key}: {e}")))?;
        if !response.status().is_success() {
            return Err(RemoteError::Http(format!("download of {key}: {}", response.status())));
        }
        let mut offset = range.map(|(start, _)| start).unwrap_or_default();
        let mut reader = response.body_mut().with_config().limit(u64::MAX).reader();
        let mut buffer = vec![0; COPY_BUFFER_SIZE];
        loop {
            let read = reader.read(&mut buffer).map_err(|e| RemoteError::io(dest, e))?;
            if read == 0 {
                return Ok(());
            }
            file.write_all_at(&buffer[..read], offset).map_err(|e| RemoteError::io(dest, e))?;
            offset += read as u64;
        }
    }

    fn create(&self, key: &str, source: &Path) -> Result<(), RemoteError> {
        let request = CreateRequest { key, version: &self.version };
        let created = match self.call::<_, CreateResponse>("CreateCacheEntry", &request)? {
            Ok(created) => created,
            Err(refused) if refused.status == HTTP_CONFLICT => return Ok(()),
            Err(refused) if refused.message.contains(WRITE_DENIED) => return Err(RemoteError::Denied(refused.message)),
            Err(refused) => return Err(RemoteError::Http(format!("CreateCacheEntry: {}", refused.message))),
        };
        if !created.ok {
            // Another run created the entry first; entries do not change, so it holds the same object.
            return match created.message.contains("already exists") || created.message.is_empty() {
                true => Ok(()),
                false => Err(RemoteError::Http(format!("CreateCacheEntry: {}", created.message))),
            };
        }
        let size = self.upload(&created.signed_upload_url, source)?;
        let request = FinalizeRequest { key, version: &self.version, size_bytes: size.to_string() };
        match self.call::<_, FinalizeResponse>("FinalizeCacheEntryUpload", &request)? {
            Ok(done) if done.ok => Ok(()),
            Ok(done) => Err(RemoteError::Http(format!("FinalizeCacheEntryUpload: {}", done.message))),
            Err(refused) => Err(RemoteError::Http(format!("FinalizeCacheEntryUpload: {}", refused.message))),
        }
    }
}

/// Reads until the buffer is full or the file ends.
fn read_full(file: &mut File, buffer: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        let n = file.read(&mut buffer[filled..])?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    Ok(filled)
}

fn percent_encode(text: &str) -> String {
    text.replace('+', "%2B").replace('/', "%2F").replace('=', "%3D")
}

impl std::fmt::Debug for GhaCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GhaCache").field("base", &self.base).finish_non_exhaustive()
    }
}

/// The ranges, first and last byte, that cover an object of `size` bytes in parts of `part` bytes.
fn ranges(size: u64, part: u64) -> Vec<(u64, u64)> {
    (0..size.div_ceil(part)).map(|index| (index * part, ((index + 1) * part).min(size) - 1)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    /// An HTTP server for one object that answers range requests, as the storage service does, and counts them.
    fn serve(object: Vec<u8>) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
        let url = format!("http://{}/object", listener.local_addr().expect("an address"));
        let requests = Arc::new(AtomicUsize::new(0));
        let counted = requests.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                counted.fetch_add(1, Ordering::SeqCst);
                let mut reader = BufReader::new(stream.try_clone().expect("a stream"));
                let mut range = None;
                let mut line = String::new();
                while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                    if let Some(value) =
                        line.trim().strip_prefix("range: bytes=").or(line.trim().strip_prefix("Range: bytes="))
                    {
                        let (start, end) = value.split_once('-').expect("a range");
                        range = Some((start.parse::<usize>().expect("a start"), end.parse::<usize>().expect("an end")));
                    }
                    line.clear();
                }
                let mut stream = stream;
                let (status, body, extra) = match range {
                    Some((start, end)) => (
                        "206 Partial Content",
                        &object[start..=end],
                        format!("Content-Range: bytes {start}-{end}/{}\r\n", object.len()),
                    ),
                    None => ("200 OK", &object[..], String::new()),
                };
                let head =
                    format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n", body.len());
                stream.write_all(head.as_bytes()).and_then(|()| stream.write_all(body)).ok();
            }
        });
        (url, requests)
    }

    #[test]
    fn a_large_object_is_downloaded_in_ranges_and_put_together() {
        let object: Vec<u8> = (0..RANGE_SIZE * 3 + 12_345).map(|i| (i % 251) as u8).collect();
        let (url, requests) = serve(object.clone());
        let agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
        let cache = GhaCache { agent, base: String::new(), token: String::new(), version: String::new() };
        let dir = tempfile::TempDir::new().expect("a directory");
        let dest = dir.path().join("object");
        assert!(cache.download(&url, "object", &dest).expect("the download works"));
        assert!(std::fs::read(&dest).expect("the file") == object, "the parts are put together in order");
        assert_eq!(requests.load(Ordering::SeqCst), 1 + 4, "one request for the size, and one per range");
    }

    #[test]
    fn ranges_cover_every_byte_once() {
        assert_eq!(ranges(10, 4), [(0, 3), (4, 7), (8, 9)]);
        assert_eq!(ranges(8, 4), [(0, 3), (4, 7)]);
        assert_eq!(ranges(1, 4), [(0, 0)]);
    }
}
