//! The client side of the plan: a backend of the run loop that sends actions to an executor service. It opens one
//! sandbox group for the actions without network and one for those with network, at the first action of each.

use std::collections::{BTreeMap, HashMap};
use std::io::{self, BufReader};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::executor::protocol::{Event, Request, Shape, receive, send};
use crate::ir::{Action, ActionResult};
use crate::local::cached_result;
use crate::log::LogSink;
use crate::scheduler::{Backend, BackendError, BackendExecution};
use crate::store::{ActionCache, Store, StoreError};
use crate::trust::Trust;

#[derive(Debug, thiserror::Error)]
pub enum ExecutorError {
    #[error("the executor at {socket}: {error}")]
    Connect { socket: PathBuf, error: io::Error },
    #[error("the executor refused the group: {0}")]
    Refused(String),
    #[error("the executor closed the group")]
    Closed,
    #[error("the executor could not run the action: {0}")]
    Failed(String),
    #[error(transparent)]
    Store(#[from] StoreError),
}

#[derive(Debug)]
pub struct ExecutorClient {
    socket: PathBuf,
    store: Store,
    cache: ActionCache,
    trust: Trust,
    groups: Mutex<BTreeMap<Shape, Arc<Group>>>,
    next_run: AtomicU64,
}

impl ExecutorClient {
    /// A client of the service at `socket`, which shares `store`, for runs of a trust level.
    pub fn new(socket: PathBuf, store: Store, trust: Trust) -> Self {
        let cache = store.state().action_cache(trust);
        ExecutorClient { socket, store, cache, trust, groups: Mutex::default(), next_run: AtomicU64::new(1) }
    }

    fn group(&self, shape: Shape) -> Result<Arc<Group>, ExecutorError> {
        let mut groups = self.groups.lock().map_err(|_| ExecutorError::Closed)?;
        if let Some(group) = groups.get(&shape).filter(|group| group.is_open()) {
            return Ok(group.clone());
        }
        let group = Arc::new(Group::open(&self.socket, shape, self.trust)?);
        groups.insert(shape, group.clone());
        Ok(group)
    }
}

impl Backend for ExecutorClient {
    fn cached(&self, action: &Action) -> Result<Option<ActionResult>, BackendError> {
        Ok(cached_result(&self.store, &self.cache, action)?)
    }

    fn execute(&self, action: &Action, logs: &dyn LogSink) -> Result<BackendExecution, BackendError> {
        let digest = self.store.put(action).map_err(ExecutorError::from)?;
        let group = self.group(Shape { network: action.kind.has_network() })?;
        let id = self.next_run.fetch_add(1, Ordering::SeqCst);
        let (result, discarded) = group.run(id, digest, logs)?;
        let result: ActionResult = self.store.get(&result).map_err(ExecutorError::from)?;
        if result.succeeded() && action.kind.is_cacheable() {
            self.cache.put_result(&digest, &result).map_err(ExecutorError::from)?;
        }
        Ok(BackendExecution { result, discarded })
    }
}

/// Where the reader of a group sends the events of each run; none when the group closed.
type Runs = Mutex<Option<HashMap<u64, Sender<Event>>>>;

/// One open sandbox group: a connection, and a reader that gives each run its events.
#[derive(Debug)]
struct Group {
    writer: Mutex<UnixStream>,
    runs: Arc<Runs>,
}

impl Group {
    fn open(socket: &PathBuf, shape: Shape, trust: Trust) -> Result<Self, ExecutorError> {
        let connect_error = |error| ExecutorError::Connect { socket: socket.clone(), error };
        let mut stream = UnixStream::connect(socket).map_err(connect_error)?;
        send(&mut stream, &Request::Open { shape, trust }).map_err(connect_error)?;
        let mut reader = BufReader::new(stream.try_clone().map_err(connect_error)?);
        match receive(&mut reader).map_err(connect_error)? {
            Some(Event::Opened) => {}
            Some(Event::Refused { reason }) => return Err(ExecutorError::Refused(reason)),
            _ => return Err(ExecutorError::Closed),
        }
        let runs = Arc::new(Mutex::new(Some(HashMap::new())));
        let routed = runs.clone();
        thread::spawn(move || route_events(reader, &routed));
        Ok(Group { writer: Mutex::new(stream), runs })
    }

    fn is_open(&self) -> bool {
        self.runs.lock().is_ok_and(|runs| runs.is_some())
    }

    fn run(
        &self,
        id: u64,
        action: crate::ir::Digest,
        logs: &dyn LogSink,
    ) -> Result<(crate::ir::Digest, Vec<String>), ExecutorError> {
        let events = self.register(id)?;
        {
            let mut writer = self.writer.lock().map_err(|_| ExecutorError::Closed)?;
            send(&mut *writer, &Request::Run { id, action }).map_err(|_| ExecutorError::Closed)?;
        }
        for event in events {
            match event {
                Event::Log { stream, line, .. } => logs.line(stream.into(), &line),
                Event::Done { result, discarded, .. } => return Ok((result, discarded)),
                Event::Failed { reason, .. } => return Err(ExecutorError::Failed(reason)),
                Event::Opened | Event::Refused { .. } => {}
            }
        }
        Err(ExecutorError::Closed)
    }

    fn register(&self, id: u64) -> Result<Receiver<Event>, ExecutorError> {
        let (sender, receiver) = mpsc::channel();
        let mut runs = self.runs.lock().map_err(|_| ExecutorError::Closed)?;
        runs.as_mut().ok_or(ExecutorError::Closed)?.insert(id, sender);
        Ok(receiver)
    }
}

/// Gives each event to its run. When the connection ends, every run that waits learns that the group closed.
fn route_events(mut reader: BufReader<UnixStream>, runs: &Runs) {
    while let Ok(Some(event)) = receive::<Event>(&mut reader) {
        let id = match &event {
            Event::Log { id, .. } | Event::Done { id, .. } | Event::Failed { id, .. } => *id,
            Event::Opened | Event::Refused { .. } => continue,
        };
        let ended = matches!(event, Event::Done { .. } | Event::Failed { .. });
        let Ok(mut runs) = runs.lock() else { return };
        let Some(open) = runs.as_mut() else { return };
        if let Some(sender) = open.get(&id) {
            sender.send(event).ok();
        }
        if ended {
            open.remove(&id);
        }
    }
    if let Ok(mut runs) = runs.lock() {
        runs.take();
    }
}
