use std::path::Path;
use std::sync::Arc;

use crate::ir::{Action, ActionResult, Canonical, Kind};
use crate::log::LogSink;
use crate::remote::{Kept, RemoteCache};
use crate::scheduler::{Backend, BackendError, BackendExecution};

/// A backend with a remote cache in front: a local miss asks the remote cache, inputs that are missing here are
/// fetched before an action runs, and a successful result is uploaded.
pub struct RemoteBackend {
    inner: Box<dyn Backend>,
    remote: Arc<RemoteCache>,
}

impl RemoteBackend {
    pub fn new(inner: Box<dyn Backend>, remote: Arc<RemoteCache>) -> Self {
        RemoteBackend { inner, remote }
    }
}

impl Backend for RemoteBackend {
    fn cached(&self, action: &Action) -> Result<Option<ActionResult>, BackendError> {
        let local = self.inner.cached(action)?;
        if !action.kind.is_shareable() {
            return Ok(local);
        }
        if let Some(result) = local {
            // A result known here fills a remote cache that does not have it, such as a new one.
            self.remote.upload(&action.digest(), &result);
            return Ok(Some(result));
        }
        Ok(self.remote.result(&action.digest()))
    }

    fn execute(&self, action: &Action, logs: &dyn LogSink) -> Result<BackendExecution, BackendError> {
        let is_host = matches!(action.kind, Kind::Host { .. });
        if !is_host {
            for tree in std::iter::once(&action.root).chain(action.inputs.values()).chain(action.work.values()) {
                self.remote.ensure_tree(tree)?;
            }
        }
        let kept = kept_state(action);
        for state in &kept {
            self.remote.ensure_kept(state);
        }
        let execution = self.inner.execute(action, logs)?;
        if execution.result.succeeded() {
            for state in &kept {
                self.remote.kept_changed(state);
            }
        }
        if execution.result.succeeded() && action.kind.is_shareable() {
            self.remote.upload(&action.digest(), &execution.result);
        }
        Ok(execution)
    }
}

/// The state that an action keeps between runs (decision 0017): the cache volumes of a sandboxed action, or the
/// directories of the workspace that a host action names as its caches.
fn kept_state(action: &Action) -> Vec<Kept> {
    let platform = action.platform.clone();
    action
        .caches
        .iter()
        .map(|(path, name)| match &action.kind {
            Kind::Host { workspace, .. } => {
                let path = Path::new(workspace).join(path);
                Kept::Directory { name: name.clone(), platform: platform.clone(), path }
            }
            _ => Kept::Volume { name: name.clone(), platform: platform.clone() },
        })
        .collect()
}

impl std::fmt::Debug for RemoteBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteBackend").field("remote", &self.remote).finish_non_exhaustive()
    }
}
