//! The executions that the engines of one process share, such as the orchestrator's runs that run at the same time: at
//! most a number of actions run at once, and an action that runs already at the same trust level is not run again, but
//! waits for that execution and gets its result.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};

use crate::ir::{Action, ActionResult, Canonical, Digest};
use crate::log::{LogSink, Stream};
use crate::scheduler::{Backend, BackendError, BackendExecution};
use crate::trust::Trust;

const JOINED: &str = "Azad runs the same step for another goal or run now, and waits for its result.";

/// A shared limit of executions, and the executions that run now. Clones share them.
#[derive(Clone, Debug)]
pub struct Pool {
    shared: Arc<Shared>,
}

#[derive(Debug)]
struct Shared {
    slots: usize,
    state: Mutex<State>,
    freed: Condvar,
}

#[derive(Debug, Default)]
struct State {
    running: usize,
    flights: HashMap<(Trust, Digest), Arc<Flight>>,
}

/// One execution that runs now, and the result that it gives to the actions that wait for it.
#[derive(Debug, Default)]
struct Flight {
    result: Mutex<Option<Result<BackendExecution, String>>>,
    ended: Condvar,
}

impl Pool {
    /// A pool in which at most `slots` actions run at the same time.
    pub fn new(slots: usize) -> Self {
        let shared = Shared { slots: slots.max(1), state: Mutex::default(), freed: Condvar::new() };
        Pool { shared: Arc::new(shared) }
    }

    fn take_slot(&self) {
        let mut state = self.shared.state.lock().expect("the pool's lock");
        while state.running >= self.shared.slots {
            state = self.shared.freed.wait(state).expect("the pool's lock");
        }
        state.running += 1;
    }

    fn give_slot(&self, flight: Option<&(Trust, Digest)>) {
        let mut state = self.shared.state.lock().expect("the pool's lock");
        state.running -= 1;
        if let Some(key) = flight {
            state.flights.remove(key);
        }
        self.shared.freed.notify_all();
    }

    /// The execution of `key` that runs now, or none, and then the caller's execution runs for the others.
    fn join_or_lead(&self, key: (Trust, Digest)) -> Result<Arc<Flight>, Arc<Flight>> {
        let mut state = self.shared.state.lock().expect("the pool's lock");
        if let Some(flight) = state.flights.get(&key) {
            return Ok(flight.clone());
        }
        let flight = Arc::new(Flight::default());
        state.flights.insert(key, flight.clone());
        Err(flight)
    }
}

impl Flight {
    fn wait(&self) -> Result<BackendExecution, String> {
        let mut result = self.result.lock().expect("the flight's lock");
        loop {
            if let Some(result) = result.as_ref() {
                return result.clone();
            }
            result = self.ended.wait(result).expect("the flight's lock");
        }
    }

    fn end(&self, result: &Result<BackendExecution, BackendError>) {
        let shared = match result {
            Ok(execution) => Ok(execution.clone()),
            Err(e) => Err(e.to_string()),
        };
        *self.result.lock().expect("the flight's lock") = Some(shared);
        self.ended.notify_all();
    }
}

/// A backend whose executions go through a pool, at the trust level of its engine.
pub(crate) struct Pooled {
    pub(crate) inner: Box<dyn Backend>,
    pub(crate) pool: Pool,
    pub(crate) trust: Trust,
}

impl Backend for Pooled {
    fn cached(&self, action: &Action) -> Result<Option<ActionResult>, BackendError> {
        self.inner.cached(action)
    }

    fn execute(&self, action: &Action, logs: &dyn LogSink) -> Result<BackendExecution, BackendError> {
        if !action.kind.is_cacheable() {
            self.pool.take_slot();
            let execution = self.inner.execute(action, logs);
            self.pool.give_slot(None);
            return execution;
        }
        let key = (self.trust, action.digest());
        let flight = match self.pool.join_or_lead(key) {
            Ok(running) => {
                logs.line(Stream::Stderr, JOINED);
                return running.wait().map_err(BackendError::from);
            }
            Err(led) => led,
        };
        self.pool.take_slot();
        let execution = self.inner.execute(action, logs);
        flight.end(&execution);
        self.pool.give_slot(Some(&key));
        execution
    }
}
