//! The HTTP API of Cloud Hypervisor on its Unix socket, for the disks of each run: they are attached to the running VM
//! when the run starts and detached when it ends.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::sandbox::SandboxError;

const API_PREFIX: &str = "/api/v1/";
const CONTENT_LENGTH: &str = "content-length";
const SUCCESS: std::ops::Range<u16> = 200..300;
const RAW_IMAGE: &str = "Raw";
const DEVICE_TREE: &str = "device_tree";

#[derive(Debug)]
pub(super) struct Api {
    socket: PathBuf,
}

impl Api {
    pub(super) fn new(socket: PathBuf) -> Self {
        Api { socket }
    }

    /// Attaches a read-only image as persistent memory; writes go nowhere. Returns the device's PCI address.
    pub(super) fn add_pmem(&self, file: &Path, id: &str) -> Result<String, SandboxError> {
        self.add("vm.add-pmem", json!({ "file": file, "discard_writes": true, "id": id }))
    }

    /// Attaches a writable raw disk image. Returns the device's PCI address.
    pub(super) fn add_disk(&self, path: &Path, id: &str) -> Result<String, SandboxError> {
        self.add("vm.add-disk", json!({ "path": path, "id": id, "image_type": RAW_IMAGE }))
    }

    /// Asks the guest to let go of a device; the VMM removes it when the guest has.
    pub(super) fn remove_device(&self, id: &str) -> Result<(), SandboxError> {
        self.put("vm.remove-device", &json!({ "id": id })).map(|_| ())
    }

    /// Whether the VM has a device with this ID.
    pub(super) fn has_device(&self, id: &str) -> Result<bool, SandboxError> {
        let response = self.request("GET", "vm.info", None)?;
        let info: Value = serde_json::from_str(&response)
            .map_err(|e| SandboxError::Setup(format!("the VMM's answer to vm.info: {e}")))?;
        Ok(info.get(DEVICE_TREE).and_then(|tree| tree.get(id)).is_some())
    }

    fn add(&self, endpoint: &str, body: Value) -> Result<String, SandboxError> {
        let response = self.put(endpoint, &body)?;
        let info: Value = serde_json::from_str(&response)
            .map_err(|e| SandboxError::Setup(format!("the VMM's answer to {endpoint}: {e}: {response}")))?;
        info.get("bdf").and_then(Value::as_str).map(str::to_owned).ok_or_else(|| {
            SandboxError::Setup(format!("the VMM's answer to {endpoint} has no PCI address: {response}"))
        })
    }

    fn put(&self, endpoint: &str, body: &Value) -> Result<String, SandboxError> {
        self.request("PUT", endpoint, Some(body))
    }

    fn request(&self, method: &str, endpoint: &str, body: Option<&Value>) -> Result<String, SandboxError> {
        let failed = |e: std::io::Error| SandboxError::Setup(format!("the VMM's API ({endpoint}): {e}"));
        let mut stream = UnixStream::connect(&self.socket).map_err(failed)?;
        let body = body.map(Value::to_string).unwrap_or_default();
        let request = format!(
            "{method} {API_PREFIX}{endpoint} HTTP/1.1\r\nHost: localhost\r\nAccept: application/json\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(request.as_bytes()).map_err(failed)?;
        let (status, text) = read_response(&mut BufReader::new(stream)).map_err(failed)?;
        match SUCCESS.contains(&status) {
            true => Ok(text),
            false => Err(SandboxError::Setup(format!("the VMM refused {endpoint} with {status}: {}", text.trim()))),
        }
    }
}

fn read_response(reader: &mut impl BufRead) -> std::io::Result<(u16, String)> {
    let mut status_line = String::new();
    reader.read_line(&mut status_line)?;
    let status =
        status_line.split_whitespace().nth(1).and_then(|code| code.parse().ok()).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("a bad status: {status_line}"))
        })?;
    let mut length = 0;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header)?;
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.trim().eq_ignore_ascii_case(CONTENT_LENGTH)
        {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}
