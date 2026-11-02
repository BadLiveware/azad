//! Expanding a static `strategy.matrix` into its combinations, with GitHub's `include` and `exclude` rules.
//!
//! GitHub makes the combinations with the first key of the matrix as the outer loop, removes the `exclude` entries,
//! and then applies the `include` entries in their order.

use serde_json::{Map, Value};

use crate::frontend::gha::yaml::KeyOrder;
use azad_gha_runner::expr::to_text;

const INCLUDE: &str = "include";
const EXCLUDE: &str = "exclude";
const TITLE_SEPARATOR: &str = ", ";

/// One combination of a matrix: its values, by key, in GitHub's order.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Combination {
    values: Vec<(String, Value)>,
}

impl Combination {
    fn get(&self, key: &str) -> Option<&Value> {
        self.values.iter().find(|(k, _)| k == key).map(|(_, value)| value)
    }

    /// Sets a value; a key that the combination has keeps its place.
    fn insert(&mut self, key: &str, value: &Value) {
        match self.values.iter_mut().find(|(k, _)| k == key) {
            Some((_, existing)) => *existing = value.clone(),
            None => self.values.push((key.to_owned(), value.clone())),
        }
    }

    /// The `matrix` context of the combination.
    pub(crate) fn to_map(&self) -> Map<String, Value> {
        self.values.iter().cloned().collect()
    }

    /// The values that GitHub adds to the name of a matrix job: `3.12, fast` in `test (3.12, fast)`.
    pub(crate) fn title(&self) -> String {
        let texts: Vec<String> = self.values.iter().map(|(_, value)| value_text(value)).collect();
        texts.join(TITLE_SEPARATOR)
    }
}

fn value_text(value: &Value) -> String {
    match value {
        Value::Object(_) | Value::Array(_) => value.to_string(),
        scalar => to_text(scalar),
    }
}

/// An entry of `include` or `exclude`: its pairs, in the order of the file.
type Entry<'a> = Vec<(&'a String, &'a Value)>;

/// The combinations of the matrix at `path` in the file, in GitHub's order. An error names what is not a static matrix.
pub(crate) fn expand(
    matrix: &Map<String, Value>,
    order: &KeyOrder,
    path: &[String],
) -> Result<Vec<Combination>, String> {
    let mut dimensions: Vec<(&String, &Vec<Value>)> = Vec::new();
    for key in order.keys(path, matrix).into_iter().filter(|k| *k != INCLUDE && *k != EXCLUDE) {
        let Value::Array(values) = &matrix[key] else {
            return Err(format!("matrix.{key} must be a list"));
        };
        dimensions.push((key, values));
    }
    let mut combinations = cartesian(&dimensions);
    if let Some(excludes) = matrix.get(EXCLUDE) {
        let excludes = entries(excludes, EXCLUDE, order, path)?;
        combinations.retain(|combination| !excludes.iter().any(|exclude| matches_all(combination, exclude)));
    }
    let originals: Vec<&String> = dimensions.iter().map(|(k, _)| *k).collect();
    let base_len = combinations.len();
    if let Some(includes) = matrix.get(INCLUDE) {
        for include in entries(includes, INCLUDE, order, path)? {
            apply_include(&mut combinations, base_len, &originals, &include);
        }
    }
    Ok(combinations)
}

/// The product of the lists; the first list is the outer loop.
fn cartesian(dimensions: &[(&String, &Vec<Value>)]) -> Vec<Combination> {
    if dimensions.is_empty() {
        return Vec::new();
    }
    let mut combinations = vec![Combination::default()];
    for (key, values) in dimensions {
        combinations = combinations
            .into_iter()
            .flat_map(|combination| {
                values.iter().map(move |value| {
                    let mut next = combination.clone();
                    next.insert(key, value);
                    next
                })
            })
            .collect();
    }
    combinations
}

fn entries<'a>(value: &'a Value, what: &str, order: &KeyOrder, path: &[String]) -> Result<Vec<Entry<'a>>, String> {
    let Value::Array(items) = value else {
        return Err(format!("matrix.{what} must be a list"));
    };
    let mut entries = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let map = item.as_object().ok_or_else(|| format!("each matrix.{what} entry must be a mapping"))?;
        let entry_path: Vec<String> = path.iter().cloned().chain([what.to_owned(), index.to_string()]).collect();
        entries.push(order.keys(&entry_path, map).into_iter().map(|key| (key, &map[key])).collect());
    }
    Ok(entries)
}

fn matches_all(combination: &Combination, pattern: &Entry<'_>) -> bool {
    pattern.iter().all(|(key, value)| combination.get(key) == Some(*value))
}

/// GitHub's rule: an include entry extends each original combination whose original values it does not change; when
/// it extends none, it is a new combination. Values that an earlier include added can change.
fn apply_include(combinations: &mut Vec<Combination>, base_len: usize, originals: &[&String], include: &Entry<'_>) {
    let changes_original = |combination: &Combination| {
        include.iter().any(|(key, value)| originals.contains(key) && combination.get(key) != Some(*value))
    };
    let mut extended = false;
    for combination in combinations.iter_mut().take(base_len) {
        if changes_original(combination) {
            continue;
        }
        for (key, value) in include {
            combination.insert(key, value);
        }
        extended = true;
    }
    if !extended {
        let mut added = Combination::default();
        for (key, value) in include {
            added.insert(key, value);
        }
        combinations.push(added);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::gha::yaml;
    use serde_json::json;

    /// The combinations of a matrix in YAML, each as its values in order.
    fn combos(text: &str) -> Vec<Vec<(String, Value)>> {
        let (value, order) = yaml::parse(text).unwrap();
        expand(value.as_object().unwrap(), &order, &[]).unwrap().into_iter().map(|c| c.values).collect()
    }

    fn pairs(values: &[(&str, Value)]) -> Vec<(String, Value)> {
        values.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect()
    }

    #[test]
    fn a_matrix_is_the_product_of_its_lists_with_the_first_key_outside() {
        let got = combos("v: [1, 2]\nos: [b, a]\n");
        let expected = [(1, "b"), (1, "a"), (2, "b"), (2, "a")]
            .map(|(v, os)| pairs(&[("v", json!(v)), ("os", json!(os))]))
            .to_vec();
        assert_eq!(got, expected);
    }

    #[test]
    fn exclude_removes_matching_combinations() {
        let got = combos("os: [a, b]\nv: [1, 2]\nexclude:\n  - {os: b, v: 2}\n");
        assert_eq!(got.len(), 3);
        assert!(!got.contains(&pairs(&[("os", json!("b")), ("v", json!(2))])));
    }

    /// The example in GitHub's documentation of `include`, with its result in the documented order.
    #[test]
    fn include_extends_or_adds_as_github_does() {
        let got = combos(
            r"
fruit: [apple, pear]
animal: [cat, dog]
include:
  - color: green
  - color: pink
    animal: cat
  - fruit: apple
    shape: circle
  - fruit: banana
  - fruit: banana
    animal: cat
",
        );
        let expected = vec![
            pairs(&[
                ("fruit", json!("apple")),
                ("animal", json!("cat")),
                ("color", json!("pink")),
                ("shape", json!("circle")),
            ]),
            pairs(&[
                ("fruit", json!("apple")),
                ("animal", json!("dog")),
                ("color", json!("green")),
                ("shape", json!("circle")),
            ]),
            pairs(&[("fruit", json!("pear")), ("animal", json!("cat")), ("color", json!("pink"))]),
            pairs(&[("fruit", json!("pear")), ("animal", json!("dog")), ("color", json!("green"))]),
            pairs(&[("fruit", json!("banana"))]),
            pairs(&[("fruit", json!("banana")), ("animal", json!("cat"))]),
        ];
        assert_eq!(got, expected);
    }

    /// GitHub applies `include` after `exclude`, so an include can add back what an exclude removed.
    #[test]
    fn include_adds_back_what_exclude_removed() {
        let got = combos("os: [a, b]\nexclude:\n  - os: b\ninclude:\n  - {os: b, extra: x}\n");
        assert_eq!(got, vec![pairs(&[("os", json!("a"))]), pairs(&[("os", json!("b")), ("extra", json!("x"))])]);
    }

    #[test]
    fn a_matrix_of_only_includes_is_its_includes() {
        let got = combos("include: [{a: 1}, {a: 2}]\n");
        assert_eq!(got, vec![pairs(&[("a", json!(1))]), pairs(&[("a", json!(2))])]);
    }

    #[test]
    fn the_title_is_the_values_in_order() {
        let (value, order) = yaml::parse("python: ['3.12']\nmode: [fast]\nos: [{name: x}]\n").unwrap();
        let combination = &expand(value.as_object().unwrap(), &order, &[]).unwrap()[0];
        assert_eq!(combination.title(), r#"3.12, fast, {"name":"x"}"#);
    }
}
