//! The service side of the plan: each connection is a sandbox group with a backend of its own, whose sandbox serves
//! the group's runs; the group's VM stops when the connection closes.

use std::io::{self, BufReader};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::Mutex;
use std::thread;

use crate::executor::protocol::{Event, Request, receive, send};
use crate::ir::Action;
use crate::local::LocalBackend;
use crate::log::{LogSink, Stream};
use crate::sandbox::{Bubblewrap, Limits, MicroVm, Sandbox, VmConfig};
use crate::store::Store;
use crate::timings::Timings;

/// The sandbox in which the service runs its groups' actions.
#[derive(Clone, Debug)]
pub enum ServiceSandbox {
    Bubblewrap,
    Vm(VmConfig),
}

#[derive(Debug)]
pub struct ExecutorService {
    store: Store,
    sandbox: ServiceSandbox,
    limits: Limits,
}

impl ExecutorService {
    pub fn new(store: Store, sandbox: ServiceSandbox, limits: Limits) -> Self {
        ExecutorService { store, sandbox, limits }
    }

    /// Serves groups until the listener fails. Each connection gets a thread of its own.
    pub fn serve(&self, listener: &UnixListener) -> io::Result<()> {
        thread::scope(|scope| {
            for connection in listener.incoming() {
                let connection = connection?;
                scope.spawn(move || {
                    if let Err(e) = self.serve_group(connection) {
                        eprintln!("azad executor: a group ended with an error: {e}");
                    }
                });
            }
            Ok(())
        })
    }

    fn serve_group(&self, connection: UnixStream) -> io::Result<()> {
        let writer = Mutex::new(connection.try_clone()?);
        let mut reader = BufReader::new(connection);
        let Some(Request::Open { shape, trust }) = receive(&mut reader)? else {
            return send_to(&writer, &Event::Refused { reason: "a group starts with an open request".to_owned() });
        };
        let sandbox: Box<dyn Sandbox> = match &self.sandbox {
            ServiceSandbox::Bubblewrap => Box::new(Bubblewrap::default()),
            ServiceSandbox::Vm(config) => Box::new(MicroVm::new(config.clone()).with_timings(Timings::default())),
        };
        let backend = match LocalBackend::new(self.store.clone(), sandbox, self.limits.clone()) {
            Ok(backend) => backend.with_trust(trust),
            Err(e) => return send_to(&writer, &Event::Refused { reason: e.to_string() }),
        };
        send_to(&writer, &Event::Opened)?;
        thread::scope(|scope| {
            while let Some(request) = receive(&mut reader)? {
                let Request::Run { id, action } = request else {
                    send_to(&writer, &Event::Failed { id: 0, reason: "the group is open already".to_owned() })?;
                    continue;
                };
                let (backend, writer, store) = (&backend, &writer, &self.store);
                scope.spawn(move || {
                    let event = match store.get::<Action>(&action) {
                        Ok(action) if action.kind.has_network() != shape.network => Event::Failed {
                            id,
                            reason: format!("the action's network does not match the group's shape: {shape:?}"),
                        },
                        Ok(action) => run(backend, store, id, &action, writer),
                        Err(e) => Event::Failed { id, reason: e.to_string() },
                    };
                    send_to(writer, &event).ok();
                });
            }
            Ok(())
        })
    }
}

fn run(backend: &LocalBackend, store: &Store, id: u64, action: &Action, writer: &Mutex<UnixStream>) -> Event {
    let logs = RunLogs { id, writer };
    let execution = match backend.run_uncached(action, &logs) {
        Ok(execution) => execution,
        Err(e) => return Event::Failed { id, reason: e.to_string() },
    };
    match store.put(&execution.result) {
        Ok(result) => Event::Done { id, result, discarded: execution.discarded },
        Err(e) => Event::Failed { id, reason: e.to_string() },
    }
}

fn send_to(writer: &Mutex<UnixStream>, event: &Event) -> io::Result<()> {
    let mut stream = writer.lock().map_err(|_| io::Error::other("the group's writer lock is poisoned"))?;
    send(&mut *stream, event)
}

/// Sends each line of a run's output to the client.
struct RunLogs<'a> {
    id: u64,
    writer: &'a Mutex<UnixStream>,
}

impl LogSink for RunLogs<'_> {
    fn line(&self, stream: Stream, line: &str) {
        let event = Event::Log { id: self.id, stream: stream.into(), line: line.to_owned() };
        send_to(self.writer, &event).ok();
    }
}
