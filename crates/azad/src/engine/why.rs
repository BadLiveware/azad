//! What `azad why` needs beyond the record of a run: the actions themselves, kept in the store when the run loop looks
//! them up, and the earlier run of a goal to compare an action with.

use crate::engine::EngineError;
use crate::ir::{Action, ActionResult, Canonical, Digest, Kind};
use crate::log::LogSink;
use crate::scheduler::{Backend, BackendError, BackendExecution, Cause, Changes, Outcome, Status};
use crate::store::{Store, StoreError};

/// A backend that stores each action that it looks up, by its digest, so that a later `azad why` can compare it with
/// another version of the same action.
pub(crate) struct KeepActions {
    pub(crate) inner: Box<dyn Backend>,
    pub(crate) store: Store,
}

impl Backend for KeepActions {
    fn cached(&self, action: &Action) -> Result<Option<ActionResult>, BackendError> {
        let stored = self.store.put(action)?;
        debug_assert_eq!(stored, action.digest(), "an action is stored by its digest");
        self.inner.cached(action)
    }

    fn execute(&self, action: &Action, logs: &dyn LogSink) -> Result<BackendExecution, BackendError> {
        self.inner.execute(action, logs)
    }
}

/// Why the cache had no result for the action `label` with the digest `action`: a comparison with the newest run of
/// the goal's history `history`, before run `before`, that has the action.
pub(crate) fn cause(
    store: &Store,
    history: &str,
    label: &str,
    action: &Digest,
    before: u64,
) -> Result<Cause, EngineError> {
    let Some(earlier) = earlier_run(store, history, label, before)? else {
        return Ok(Cause::FirstRun);
    };
    let run = earlier.number;
    if earlier.action == *action {
        return same_action(store, &earlier, action);
    }
    let (Some(old), Some(new)) = (stored_action(store, &earlier.action)?, stored_action(store, action)?) else {
        return Ok(Cause::NotStored { run });
    };
    let changes = Changes::between(store, &old, &new).map_err(EngineError::store("the trees of the action"))?;
    Ok(Cause::Changed { run, changes })
}

/// An action in an earlier run of a goal.
struct Earlier {
    number: u64,
    action: Digest,
    failed: bool,
}

/// Why an action that is the same as in an earlier run has no result.
fn same_action(store: &Store, earlier: &Earlier, action: &Digest) -> Result<Cause, EngineError> {
    let run = earlier.number;
    if earlier.failed {
        return Ok(Cause::FailedBefore { run });
    }
    let host_outputs = match stored_action(store, action)? {
        Some(Action { kind: Kind::Host { .. }, outputs, .. }) => outputs.into_iter().collect(),
        _ => Vec::new(),
    };
    Ok(Cause::SameAsBefore { run, host_outputs })
}

/// The action `label` in the newest run before `before` that resolved it.
fn earlier_run(store: &Store, history: &str, label: &str, before: u64) -> Result<Option<Earlier>, EngineError> {
    let state = store.state();
    for number in (1..before).rev() {
        let Some(record) = state.run(history, number).map_err(EngineError::store("the run records"))? else {
            continue;
        };
        let outcome = Outcome::decode(&record)?;
        let Some(status) = outcome.statuses.get(label) else { continue };
        if let Some(action) = status.action() {
            let failed = matches!(status, Status::Failed { .. });
            return Ok(Some(Earlier { number, action: *action, failed }));
        }
    }
    Ok(None)
}

/// An action that the store kept, or `None` when it did not keep it, as for a run of an older version of Azad.
fn stored_action(store: &Store, digest: &Digest) -> Result<Option<Action>, EngineError> {
    match store.get::<Action>(digest) {
        Ok(action) => Ok(Some(action)),
        Err(StoreError::Missing(_)) => Ok(None),
        Err(e) => Err(EngineError::store("an action")(e)),
    }
}
