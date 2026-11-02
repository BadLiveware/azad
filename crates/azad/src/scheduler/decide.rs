//! The life of one execution, as a pure function: `decide(state, event) -> (state, effects)`. The run loop performs the
//! effects and turns their results into events.

use crate::ir::ActionResult;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// The rules say the action must run; nothing is done yet.
    Queued,
    /// The cache is asked for the result.
    Checking,
    /// The action runs in a sandbox.
    Running,
    Succeeded {
        result: ActionResult,
        cached: bool,
    },
    /// A failed run keeps its result, so its log can be shown.
    Failed {
        reason: String,
        result: Option<ActionResult>,
    },
    Cancelled,
}

impl State {
    pub fn is_terminal(&self) -> bool {
        matches!(self, State::Succeeded { .. } | State::Failed { .. } | State::Cancelled)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Dispatch,
    CacheHit(ActionResult),
    CacheMiss,
    Finished(ActionResult),
    Errored(String),
    Cancel,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    LookUpCache,
    Run,
    Stop,
    /// Tell the rules the outcome: new facts for the actions that share this execution.
    Publish,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("event {event:?} is not valid in state {state:?}")]
pub struct InvalidTransition {
    pub state: String,
    pub event: String,
}

pub fn decide(state: &State, event: Event) -> Result<(State, Vec<Effect>), InvalidTransition> {
    use Effect::*;
    let next = match (state, event) {
        (State::Queued, Event::Dispatch) => (State::Checking, vec![LookUpCache]),
        (State::Checking, Event::CacheHit(result)) => (State::Succeeded { result, cached: true }, vec![Publish]),
        (State::Checking, Event::CacheMiss) => (State::Running, vec![Run]),
        (State::Running, Event::Finished(result)) if result.succeeded() => {
            (State::Succeeded { result, cached: false }, vec![Publish])
        }
        (State::Running, Event::Finished(result)) => {
            let reason = format!("exit code {}", result.exit_code);
            (State::Failed { reason, result: Some(result) }, vec![Publish])
        }
        (State::Checking | State::Running, Event::Errored(reason)) => {
            (State::Failed { reason, result: None }, vec![Publish])
        }
        (State::Queued | State::Checking, Event::Cancel) => (State::Cancelled, vec![Publish]),
        (State::Running, Event::Cancel) => (State::Cancelled, vec![Stop, Publish]),
        (state, event) => {
            return Err(InvalidTransition { state: format!("{state:?}"), event: format!("{event:?}") });
        }
    };
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::Digest;
    use std::collections::BTreeMap;

    fn result(exit_code: i64) -> ActionResult {
        let log = Digest::of_bytes(b"");
        ActionResult { exit_code, outputs: BTreeMap::new(), stdout: log, stderr: log, log: None }
    }

    fn states() -> Vec<State> {
        vec![
            State::Queued,
            State::Checking,
            State::Running,
            State::Succeeded { result: result(0), cached: false },
            State::Failed { reason: "x".to_owned(), result: None },
            State::Cancelled,
        ]
    }

    fn events() -> Vec<Event> {
        vec![
            Event::Dispatch,
            Event::CacheHit(result(0)),
            Event::CacheMiss,
            Event::Finished(result(0)),
            Event::Finished(result(1)),
            Event::Errored("e".to_owned()),
            Event::Cancel,
        ]
    }

    /// The expected state after each event, or `None` when the event is not valid.
    fn expected(state: &State, event: &Event) -> Option<State> {
        match (state, event) {
            (State::Queued, Event::Dispatch) => Some(State::Checking),
            (State::Checking, Event::CacheHit(r)) => Some(State::Succeeded { result: r.clone(), cached: true }),
            (State::Checking, Event::CacheMiss) => Some(State::Running),
            (State::Running, Event::Finished(r)) if r.exit_code == 0 => {
                Some(State::Succeeded { result: r.clone(), cached: false })
            }
            (State::Running, Event::Finished(r)) => {
                Some(State::Failed { reason: "exit code 1".to_owned(), result: Some(r.clone()) })
            }
            (State::Checking | State::Running, Event::Errored(e)) => {
                Some(State::Failed { reason: e.clone(), result: None })
            }
            (State::Queued | State::Checking | State::Running, Event::Cancel) => Some(State::Cancelled),
            _ => None,
        }
    }

    #[test]
    fn every_state_and_event_has_a_defined_outcome() {
        for state in states() {
            for event in events() {
                let outcome = decide(&state, event.clone());
                match expected(&state, &event) {
                    Some(next) => assert_eq!(outcome.map(|(s, _)| s), Ok(next), "{state:?} + {event:?}"),
                    None => assert!(outcome.is_err(), "{state:?} + {event:?} must be refused"),
                }
            }
        }
    }

    #[test]
    fn terminal_states_publish_and_a_running_action_is_stopped_on_cancel() {
        for state in states().iter().filter(|s| !s.is_terminal()) {
            for event in events() {
                if let Ok((next, effects)) = decide(state, event) {
                    assert_eq!(next.is_terminal(), effects.contains(&Effect::Publish), "{state:?} -> {next:?}");
                }
            }
        }
        let (_, effects) = decide(&State::Running, Event::Cancel).unwrap();
        assert_eq!(effects, vec![Effect::Stop, Effect::Publish]);
    }

    #[test]
    fn no_event_leaves_a_terminal_state() {
        for state in states().into_iter().filter(State::is_terminal) {
            for event in events() {
                assert!(decide(&state, event).is_err());
            }
        }
    }
}
