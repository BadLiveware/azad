//! The record of a finished run: its facts and the status of each action, so that `azad why` can explain what
//! happened, not only what would happen now. See decision 0006.

use std::collections::{BTreeMap, BTreeSet};

use crate::ir::canonical::{Canonical, CanonicalError, Fields, digest_from_value, digest_to_value, text_list, texts};
use crate::ir::cbor::Value;
use crate::ir::{ActionResult, Digest};

use crate::scheduler::rules::{Facts, Id};
use crate::scheduler::runner::{Outcome, Status};

const KEY_GOAL: &str = "g";
const KEY_NAMES: &str = "n";
const KEY_STATUSES: &str = "s";
const KEY_REQUESTED: &str = "fr";
const KEY_DEPENDS_ON: &str = "fd";
const KEY_SUCCEEDED: &str = "fs";
const KEY_FAILED: &str = "ff";
const KEY_STARTED: &str = "fb";
const KEY_NEVER_CACHED: &str = "i";
const KEY_KIND: &str = "k";
const KEY_ACTION: &str = "a";
const KEY_RESULT: &str = "res";
const KEY_REASON: &str = "r";
const KEY_DISCARDED: &str = "d";

const NOT_NEEDED: &str = "not-needed";
const RAN: &str = "ran";
const CACHED: &str = "cached";
const FAILED: &str = "failed";
const SKIPPED: &str = "skipped";
const WILL_RUN: &str = "will-run";
const WAITING: &str = "waiting";

impl Canonical for Outcome {
    const TYPE: &'static str = "run";

    fn fields(&self) -> BTreeMap<String, Value> {
        let statuses = self.statuses.iter().map(|(name, status)| (name.clone(), status_value(status))).collect();
        let pairs = self
            .facts
            .depends_on
            .iter()
            .map(|&(a, d)| Value::Array(vec![Value::Uint(a.into()), Value::Uint(d.into())]));
        BTreeMap::from([
            (KEY_GOAL.to_owned(), Value::Text(self.goal.clone())),
            (KEY_NAMES.to_owned(), texts(&self.names)),
            (KEY_STATUSES.to_owned(), Value::Map(statuses)),
            (KEY_REQUESTED.to_owned(), ids(&self.facts.requested)),
            (KEY_DEPENDS_ON.to_owned(), Value::Array(pairs.collect())),
            (KEY_SUCCEEDED.to_owned(), ids(&self.facts.succeeded)),
            (KEY_FAILED.to_owned(), ids(&self.facts.failed)),
            (KEY_STARTED.to_owned(), ids(&self.facts.started)),
            (KEY_NEVER_CACHED.to_owned(), Value::Array(self.never_cached.iter().cloned().map(Value::Text).collect())),
        ])
    }

    fn from_fields(fields: &mut Fields) -> Result<Self, CanonicalError> {
        let goal = fields.text(KEY_GOAL)?;
        let names = text_list(fields.array(KEY_NAMES)?, KEY_NAMES)?;
        let statuses = fields
            .map(KEY_STATUSES)?
            .into_iter()
            .map(|(name, value)| status_from_value(value).map(|s| (name, s)))
            .collect::<Result<_, _>>()?;
        let depends_on = fields
            .array(KEY_DEPENDS_ON)?
            .into_iter()
            .map(|pair| match pair {
                Value::Array(items) => match items.as_slice() {
                    [Value::Uint(a), Value::Uint(d)] => Ok((id(*a)?, id(*d)?)),
                    _ => Err(CanonicalError::Field(KEY_DEPENDS_ON)),
                },
                _ => Err(CanonicalError::Field(KEY_DEPENDS_ON)),
            })
            .collect::<Result<_, _>>()?;
        let facts = Facts {
            requested: id_set(fields.array(KEY_REQUESTED)?, KEY_REQUESTED)?,
            depends_on,
            succeeded: id_set(fields.array(KEY_SUCCEEDED)?, KEY_SUCCEEDED)?,
            failed: id_set(fields.array(KEY_FAILED)?, KEY_FAILED)?,
            started: id_set(fields.array(KEY_STARTED)?, KEY_STARTED)?,
        };
        let never_cached = match fields.take_optional(KEY_NEVER_CACHED) {
            Some(Value::Array(items)) => text_list(items, KEY_NEVER_CACHED)?.into_iter().collect(),
            Some(_) => return Err(CanonicalError::Field(KEY_NEVER_CACHED)),
            None => BTreeSet::new(),
        };
        Ok(Outcome::from_parts(goal, statuses, names, facts, never_cached))
    }
}

fn ids(set: &BTreeSet<Id>) -> Value {
    Value::Array(set.iter().map(|&i| Value::Uint(i.into())).collect())
}

fn id(n: u64) -> Result<Id, CanonicalError> {
    Id::try_from(n).map_err(|_| CanonicalError::Invalid(format!("action number {n} is too large")))
}

fn id_set(values: Vec<Value>, key: &'static str) -> Result<BTreeSet<Id>, CanonicalError> {
    values
        .into_iter()
        .map(|v| match v {
            Value::Uint(n) => id(n),
            _ => Err(CanonicalError::Field(key)),
        })
        .collect()
}

fn status_value(status: &Status) -> Value {
    let mut map = BTreeMap::new();
    let kind = match status {
        Status::NotNeeded => NOT_NEEDED,
        Status::Skipped => SKIPPED,
        Status::Waiting => WAITING,
        Status::WillRun { action } => {
            map.insert(KEY_ACTION.to_owned(), digest_to_value(action));
            WILL_RUN
        }
        Status::Ran { action, result, discarded } => {
            map.insert(KEY_ACTION.to_owned(), digest_to_value(action));
            map.insert(KEY_RESULT.to_owned(), Value::Bytes(result.encode()));
            map.insert(KEY_DISCARDED.to_owned(), texts(discarded));
            RAN
        }
        Status::Cached { action, result } => {
            map.insert(KEY_ACTION.to_owned(), digest_to_value(action));
            map.insert(KEY_RESULT.to_owned(), Value::Bytes(result.encode()));
            CACHED
        }
        Status::Failed { action, reason, result } => {
            map.insert(KEY_ACTION.to_owned(), digest_to_value(action));
            map.insert(KEY_REASON.to_owned(), Value::Text(reason.clone()));
            if let Some(result) = result {
                map.insert(KEY_RESULT.to_owned(), Value::Bytes(result.encode()));
            }
            FAILED
        }
    };
    map.insert(KEY_KIND.to_owned(), Value::Text(kind.to_owned()));
    Value::Map(map)
}

fn status_from_value(value: Value) -> Result<Status, CanonicalError> {
    let Value::Map(map) = value else {
        return Err(CanonicalError::Field(KEY_STATUSES));
    };
    let mut fields = Fields::new(map);
    let action = |fields: &mut Fields| -> Result<Digest, CanonicalError> {
        digest_from_value(&fields.take(KEY_ACTION)?).ok_or(CanonicalError::Field(KEY_ACTION))
    };
    let result = |value: Value| -> Result<ActionResult, CanonicalError> {
        match value {
            Value::Bytes(bytes) => ActionResult::decode(&bytes),
            _ => Err(CanonicalError::Field(KEY_RESULT)),
        }
    };
    let status = match fields.text(KEY_KIND)?.as_str() {
        NOT_NEEDED => Status::NotNeeded,
        SKIPPED => Status::Skipped,
        WAITING => Status::Waiting,
        WILL_RUN => Status::WillRun { action: action(&mut fields)? },
        RAN => Status::Ran {
            action: action(&mut fields)?,
            result: result(fields.take(KEY_RESULT)?)?,
            discarded: text_list(fields.array(KEY_DISCARDED)?, KEY_DISCARDED)?,
        },
        CACHED => Status::Cached { action: action(&mut fields)?, result: result(fields.take(KEY_RESULT)?)? },
        FAILED => Status::Failed {
            action: action(&mut fields)?,
            reason: fields.text(KEY_REASON)?,
            result: fields.take_optional(KEY_RESULT).map(result).transpose()?,
        },
        other => return Err(CanonicalError::Invalid(format!("unknown status {other:?}"))),
    };
    fields.finish()?;
    Ok(status)
}
