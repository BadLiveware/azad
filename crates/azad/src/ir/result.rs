//! The result of an action, which the action cache stores under the action digest.

use std::collections::BTreeMap;

use crate::ir::canonical::{Canonical, CanonicalError, Fields, digest_from_value, digest_to_value};
use crate::ir::cbor::Value;
use crate::ir::digest::Digest;

const KEY_EXIT: &str = "x";
const KEY_OUTPUTS: &str = "o";
const KEY_STDOUT: &str = "1";
const KEY_STDERR: &str = "2";
const KEY_LOG: &str = "l";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionResult {
    pub exit_code: i64,
    /// The tree digest of each declared output.
    pub outputs: BTreeMap<String, Digest>,
    pub stdout: Digest,
    pub stderr: Digest,
    /// Both outputs, in the order in which their lines arrived. Results from before it was kept have none.
    pub log: Option<Digest>,
}

impl ActionResult {
    pub fn succeeded(&self) -> bool {
        self.exit_code == 0
    }

    /// The blobs of the result's output: the log, standard output and standard error.
    pub fn logs(&self) -> impl Iterator<Item = &Digest> {
        self.log.iter().chain([&self.stdout, &self.stderr])
    }

    /// Whether the action wrote any output, from the digests alone.
    pub fn printed(&self) -> bool {
        let empty = Digest::of_bytes(&[]);
        self.logs().any(|blob| *blob != empty)
    }
}

impl Canonical for ActionResult {
    const TYPE: &'static str = "result";

    fn fields(&self) -> BTreeMap<String, Value> {
        let mut fields = BTreeMap::from([
            (KEY_EXIT.to_owned(), int_to_value(self.exit_code)),
            (
                KEY_OUTPUTS.to_owned(),
                Value::Map(self.outputs.iter().map(|(k, d)| (k.clone(), digest_to_value(d))).collect()),
            ),
            (KEY_STDOUT.to_owned(), digest_to_value(&self.stdout)),
            (KEY_STDERR.to_owned(), digest_to_value(&self.stderr)),
        ]);
        if let Some(log) = &self.log {
            fields.insert(KEY_LOG.to_owned(), digest_to_value(log));
        }
        fields
    }

    fn from_fields(fields: &mut Fields) -> Result<Self, CanonicalError> {
        let exit_code = int_from_value(fields.take(KEY_EXIT)?).ok_or(CanonicalError::Field(KEY_EXIT))?;
        let outputs = fields
            .map(KEY_OUTPUTS)?
            .into_iter()
            .map(|(k, v)| digest_from_value(&v).map(|d| (k, d)).ok_or(CanonicalError::Field(KEY_OUTPUTS)))
            .collect::<Result<_, _>>()?;
        let log = match fields.take_optional(KEY_LOG) {
            Some(value) => Some(digest_from_value(&value).ok_or(CanonicalError::Field(KEY_LOG))?),
            None => None,
        };
        Ok(ActionResult {
            exit_code,
            outputs,
            stdout: fields.digest(KEY_STDOUT)?,
            stderr: fields.digest(KEY_STDERR)?,
            log,
        })
    }
}

fn int_to_value(n: i64) -> Value {
    match u64::try_from(n) {
        Ok(positive) => Value::Uint(positive),
        Err(_) => Value::Nint(n.unsigned_abs() - 1),
    }
}

fn int_from_value(value: Value) -> Option<i64> {
    match value {
        Value::Uint(n) => i64::try_from(n).ok(),
        Value::Nint(n) => i64::try_from(n).ok().map(|n| -1 - n),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_with_a_negative_exit_code() {
        let result = ActionResult {
            exit_code: -9,
            outputs: BTreeMap::from([("out".to_owned(), Digest::of_bytes(b"tree"))]),
            stdout: Digest::of_bytes(b"out"),
            stderr: Digest::of_bytes(b"err"),
            log: Some(Digest::of_bytes(b"log")),
        };
        assert_eq!(ActionResult::decode(&result.encode()), Ok(result));
    }
}
