//! YAML to JSON: workflows and their values are read as `serde_json::Value`, the type that expressions use.

use std::collections::BTreeMap;

use serde_json::{Map, Number, Value};
use yaml_rust2::{Yaml, YamlLoader};

/// The order of the keys of each mapping in the file, by the path of the mapping. `serde_json::Map` sorts its keys,
/// but GitHub reads some mappings in the order of the file: the keys of a matrix give the order of its jobs.
#[derive(Debug, Default)]
pub(crate) struct KeyOrder {
    keys: BTreeMap<Vec<String>, Vec<String>>,
}

impl KeyOrder {
    /// The keys of the mapping at `path`, in the order of the file; the keys of `map` that the file does not order
    /// follow in their own order.
    pub(crate) fn keys<'m>(&self, path: &[String], map: &'m Map<String, Value>) -> Vec<&'m String> {
        let ordered = self.keys.get(path).map(Vec::as_slice).unwrap_or_default();
        let known = ordered.iter().filter_map(|key| map.get_key_value(key).map(|(key, _)| key));
        let rest = map.keys().filter(|key| !ordered.contains(key));
        known.chain(rest).collect()
    }
}

pub(crate) fn parse(text: &str) -> Result<(Value, KeyOrder), String> {
    let documents = YamlLoader::load_from_str(text).map_err(|e| e.to_string())?;
    let [document] = documents.as_slice() else {
        return Err(format!("a workflow is one YAML document, not {}", documents.len()));
    };
    let mut order = KeyOrder::default();
    let value = to_json(document, &mut Vec::new(), &mut order)?;
    Ok((value, order))
}

fn to_json(yaml: &Yaml, path: &mut Vec<String>, order: &mut KeyOrder) -> Result<Value, String> {
    let value = match yaml {
        Yaml::Null => Value::Null,
        Yaml::Boolean(b) => Value::Bool(*b),
        Yaml::Integer(n) => Value::Number(Number::from(*n)),
        Yaml::Real(text) => {
            let parsed: f64 = text.parse().map_err(|_| format!("{text:?} is not a number"))?;
            Number::from_f64(parsed).map_or(Value::String(text.clone()), Value::Number)
        }
        Yaml::String(s) => Value::String(s.clone()),
        Yaml::Array(items) => {
            let mut values = Vec::with_capacity(items.len());
            for (index, item) in items.iter().enumerate() {
                values.push(nested(item, index.to_string(), path, order)?);
            }
            Value::Array(values)
        }
        Yaml::Hash(hash) => {
            let mut map = Map::new();
            let mut keys = Vec::with_capacity(hash.len());
            for (key, item) in hash {
                let key = key_text(key)?;
                map.insert(key.clone(), nested(item, key.clone(), path, order)?);
                keys.push(key);
            }
            order.keys.insert(path.clone(), keys);
            Value::Object(map)
        }
        Yaml::Alias(_) | Yaml::BadValue => return Err("a YAML alias or a bad value".to_owned()),
    };
    Ok(value)
}

fn nested(yaml: &Yaml, step: String, path: &mut Vec<String>, order: &mut KeyOrder) -> Result<Value, String> {
    path.push(step);
    let value = to_json(yaml, path, order);
    path.pop();
    value
}

fn key_text(key: &Yaml) -> Result<String, String> {
    match key {
        Yaml::String(s) => Ok(s.clone()),
        Yaml::Integer(n) => Ok(n.to_string()),
        Yaml::Boolean(b) => Ok(b.to_string()),
        Yaml::Real(text) => Ok(text.clone()),
        other => Err(format!("a mapping key must be a scalar, not {other:?}")),
    }
}
