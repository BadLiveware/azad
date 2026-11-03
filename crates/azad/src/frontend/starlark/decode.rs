//! `toml`, `yaml` and `xml`: the text of a file as values, as Starlark's `json.decode` gives them (decision 0031).
//!
//! Each format is made into a JSON value first, so that the three give the same kinds of values: dicts with text
//! keys, lists, strings, ints, floats, bools and None.

use serde_json::{Map, Number, Value as Json};
use starlark::environment::GlobalsBuilder;
use starlark::starlark_module;
use starlark::values::{Heap, Value};
use yaml_rust2::{Yaml, YamlLoader};

const XML_TAG: &str = "tag";
const XML_ATTRS: &str = "attrs";
const XML_CHILDREN: &str = "children";
const XML_TEXT: &str = "text";

fn toml_to_json(value: toml::Value) -> anyhow::Result<Json> {
    Ok(match value {
        toml::Value::String(text) => Json::String(text),
        toml::Value::Integer(int) => Json::from(int),
        toml::Value::Float(float) => float_to_json(float)?,
        toml::Value::Boolean(bool) => Json::Bool(bool),
        toml::Value::Datetime(datetime) => Json::String(datetime.to_string()),
        toml::Value::Array(items) => Json::Array(items.into_iter().map(toml_to_json).collect::<anyhow::Result<_>>()?),
        toml::Value::Table(table) => toml_table_to_json(table)?,
    })
}

fn toml_table_to_json(table: toml::Table) -> anyhow::Result<Json> {
    let entries = table.into_iter().map(|(key, value)| Ok((key, toml_to_json(value)?)));
    Ok(Json::Object(entries.collect::<anyhow::Result<Map<_, _>>>()?))
}

fn float_to_json(float: f64) -> anyhow::Result<Json> {
    Number::from_f64(float)
        .map(Json::Number)
        .ok_or_else(|| anyhow::anyhow!("the float {float} has no value in Starlark that json.decode() would give"))
}

pub(crate) fn decode_toml(text: &str) -> anyhow::Result<Json> {
    let table = toml::from_str::<toml::Table>(text).map_err(|e| anyhow::anyhow!("toml.decode(): {e}"))?;
    toml_table_to_json(table)
}

fn yaml_to_json(value: Yaml) -> anyhow::Result<Json> {
    Ok(match value {
        Yaml::String(text) => Json::String(text),
        Yaml::Integer(int) => Json::from(int),
        Yaml::Real(_) => {
            let float = value.as_f64().ok_or_else(|| anyhow::anyhow!("{value:?} is not a float"))?;
            float_to_json(float)?
        }
        Yaml::Boolean(bool) => Json::Bool(bool),
        Yaml::Null => Json::Null,
        Yaml::Array(items) => Json::Array(items.into_iter().map(yaml_to_json).collect::<anyhow::Result<_>>()?),
        Yaml::Hash(hash) => {
            let entries = hash.into_iter().map(|(key, value)| Ok((yaml_key(key)?, yaml_to_json(value)?)));
            Json::Object(entries.collect::<anyhow::Result<Map<_, _>>>()?)
        }
        Yaml::Alias(_) | Yaml::BadValue => anyhow::bail!("{value:?} is not a value"),
    })
}

/// A key of a mapping as text, as JSON has it: a number or a bool is its text.
fn yaml_key(key: Yaml) -> anyhow::Result<String> {
    match key {
        Yaml::String(text) | Yaml::Real(text) => Ok(text),
        Yaml::Integer(int) => Ok(int.to_string()),
        Yaml::Boolean(bool) => Ok(bool.to_string()),
        other => anyhow::bail!("the key {other:?} is not a string, a number or a bool"),
    }
}

pub(crate) fn decode_yaml(text: &str) -> anyhow::Result<Json> {
    let documents = YamlLoader::load_from_str(text).map_err(|e| anyhow::anyhow!("yaml.decode(): {e}"))?;
    let mut documents = documents.into_iter();
    let (first, more) = (documents.next(), documents.next());
    match (first, more) {
        (None, _) => Ok(Json::Null),
        (Some(document), None) => yaml_to_json(document).map_err(|e| anyhow::anyhow!("yaml.decode(): {e}")),
        (Some(_), Some(_)) => anyhow::bail!("yaml.decode(): the text holds more than one document"),
    }
}

fn xml_element(node: roxmltree::Node<'_, '_>) -> anyhow::Result<Json> {
    let tag = node.tag_name().name();
    let mut attrs = Map::new();
    for attribute in node.attributes() {
        if attrs.insert(attribute.name().to_owned(), Json::String(attribute.value().to_owned())).is_some() {
            anyhow::bail!("<{tag}> has two attributes named {:?} in different namespaces", attribute.name());
        }
    }
    let children = node.children().filter(roxmltree::Node::is_element).map(xml_element);
    let text: String = node.children().filter(roxmltree::Node::is_text).filter_map(|child| child.text()).collect();
    let mut element = Map::new();
    element.insert(XML_TAG.to_owned(), Json::String(tag.to_owned()));
    element.insert(XML_ATTRS.to_owned(), Json::Object(attrs));
    element.insert(XML_CHILDREN.to_owned(), Json::Array(children.collect::<anyhow::Result<_>>()?));
    element.insert(XML_TEXT.to_owned(), Json::String(text.trim().to_owned()));
    Ok(Json::Object(element))
}

pub(crate) fn decode_xml(text: &str) -> anyhow::Result<Json> {
    let document = roxmltree::Document::parse(text).map_err(|e| anyhow::anyhow!("xml.decode(): {e}"))?;
    xml_element(document.root_element()).map_err(|e| anyhow::anyhow!("xml.decode(): {e}"))
}

/// The functions of the namespace `toml`.
#[starlark_module]
pub(crate) fn toml_functions(builder: &mut GlobalsBuilder) {
    /// The value of a TOML (Tom's Obvious Minimal Language) document, such as a `pyproject.toml`, as a dict.
    ///
    /// Tables are dicts, arrays are lists, and strings, integers, floats and booleans are themselves.
    /// A date or a time is its text, such as `"1979-05-27T07:32:00Z"`.
    ///
    /// # Arguments
    /// * `text`: The document, such as `read("pyproject.toml")`.
    ///
    /// Examples:
    /// ```python
    /// name = toml.decode(read("pyproject.toml"))["project"]["name"]
    /// ```
    fn decode<'v>(#[starlark(require = pos)] text: &str, heap: Heap<'v>) -> anyhow::Result<Value<'v>> {
        Ok(heap.alloc(decode_toml(text)?))
    }
}

/// The functions of the namespace `yaml`.
#[starlark_module]
pub(crate) fn yaml_functions(builder: &mut GlobalsBuilder) {
    /// The value of a YAML document, such as a `pnpm-workspace.yaml`.
    ///
    /// Mappings are dicts, sequences are lists, and scalars are strings, ints, floats, bools or None, as YAML 1.2
    /// reads them. The keys of a dict are text: a key that is a number or a bool is its text. Anchors and aliases are
    /// resolved. The text must hold one document, or none, which gives None.
    ///
    /// # Arguments
    /// * `text`: The document, such as `read("pnpm-workspace.yaml")`.
    ///
    /// Examples:
    /// ```python
    /// globs = yaml.decode(read("//pnpm-workspace.yaml"))["packages"]
    /// ```
    fn decode<'v>(#[starlark(require = pos)] text: &str, heap: Heap<'v>) -> anyhow::Result<Value<'v>> {
        Ok(heap.alloc(decode_yaml(text)?))
    }
}

/// The functions of the namespace `xml`.
#[starlark_module]
pub(crate) fn xml_functions(builder: &mut GlobalsBuilder) {
    /// The root element of an XML document, such as a `.csproj` file, as a dict.
    ///
    /// An element is `{"tag": <name>, "attrs": {<name>: <value>}, "children": [<element>], "text": <text>}`.
    /// Names are local names: a namespace is dropped, so `<Project xmlns="...">` and `<Project>` are the same.
    /// `children` holds the child elements in their order; `text` is the text directly in the element, with the
    /// white space at its ends removed, and CDATA sections are text. Comments and processing instructions are left
    /// out. A document type declaration (`<!DOCTYPE>`) is refused.
    ///
    /// # Arguments
    /// * `text`: The document, such as `read("App.csproj")`.
    ///
    /// Examples:
    /// ```python
    /// project = xml.decode(read("App.csproj"))
    /// references = [item["attrs"]["Include"] for group in project["children"] if group["tag"] == "ItemGroup"
    ///               for item in group["children"] if item["tag"] == "ProjectReference"]
    /// ```
    fn decode<'v>(#[starlark(require = pos)] text: &str, heap: Heap<'v>) -> anyhow::Result<Value<'v>> {
        Ok(heap.alloc(decode_xml(text)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn toml_tables_arrays_and_scalars_are_values() {
        let text = r#"
            [project]
            name = "app"
            version = 3
            ratio = 0.5
            released = 1979-05-27T07:32:00Z

            [tool.uv.sources]
            core = { path = "../core", editable = true }
            [[tool.uv.index]]
            url = "https://example.org"
        "#;
        assert_eq!(
            decode_toml(text).unwrap(),
            json!({
                "project": {"name": "app", "version": 3, "ratio": 0.5, "released": "1979-05-27T07:32:00Z"},
                "tool": {"uv": {
                    "sources": {"core": {"path": "../core", "editable": true}},
                    "index": [{"url": "https://example.org"}],
                }},
            })
        );
        assert!(decode_toml("name = ").unwrap_err().to_string().starts_with("toml.decode(): "));
    }

    #[test]
    fn yaml_mappings_sequences_and_scalars_are_values() {
        let text = "packages:\n  - 'apps/*'\n  - '!**/test/**'\nlimits: {jobs: 4, ratio: 1.5, strict: true, none: ~}\n1: one\nbase: &base {a: 1}\nderived: *base\n";
        assert_eq!(
            decode_yaml(text).unwrap(),
            json!({
                "packages": ["apps/*", "!**/test/**"],
                "limits": {"jobs": 4, "ratio": 1.5, "strict": true, "none": null},
                "1": "one",
                "base": {"a": 1},
                "derived": {"a": 1},
            })
        );
        assert_eq!(decode_yaml("").unwrap(), Json::Null);
        assert!(decode_yaml("a: 1\n---\nb: 2\n").unwrap_err().to_string().contains("more than one document"));
        assert!(decode_yaml("[a, b]: 1\n").unwrap_err().to_string().contains("is not a string, a number or a bool"));
    }

    #[test]
    fn xml_elements_have_local_names_attributes_children_and_text() {
        let text = r#"<?xml version="1.0" encoding="utf-8"?>
<Project ToolsVersion="15.0" xmlns="http://schemas.microsoft.com/developer/msbuild/2003" xmlns:x="urn:x">
  <!-- a comment -->
  <PropertyGroup Condition=" '$(Configuration)' == 'Debug' ">
    <OutputType> Exe </OutputType>
    <x:Note x:Kind="extra">a <![CDATA[<b>]]> c</x:Note>
  </PropertyGroup>
  <ItemGroup><ProjectReference Include="..\Lib\Lib.csproj" /></ItemGroup>
</Project>"#;
        let element = |tag: &str, attrs: Json, children: Json, text: &str| json!({"tag": tag, "attrs": attrs, "children": children, "text": text});
        let expected = element(
            "Project",
            json!({"ToolsVersion": "15.0"}),
            json!([
                element(
                    "PropertyGroup",
                    json!({"Condition": " '$(Configuration)' == 'Debug' "}),
                    json!([
                        element("OutputType", json!({}), json!([]), "Exe"),
                        element("Note", json!({"Kind": "extra"}), json!([]), "a <b> c"),
                    ]),
                    "",
                ),
                element(
                    "ItemGroup",
                    json!({}),
                    json!([element("ProjectReference", json!({"Include": "..\\Lib\\Lib.csproj"}), json!([]), "")]),
                    "",
                ),
            ]),
            "",
        );
        assert_eq!(decode_xml(text).unwrap(), expected);
        let sdk_style =
            r#"<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><OutputType>Exe</OutputType></PropertyGroup></Project>"#;
        let old_style = r#"<Project xmlns="http://schemas.microsoft.com/developer/msbuild/2003" Sdk="Microsoft.NET.Sdk"><PropertyGroup><OutputType>Exe</OutputType></PropertyGroup></Project>"#;
        assert_eq!(decode_xml(sdk_style).unwrap(), decode_xml(old_style).unwrap());
    }

    #[test]
    fn xml_that_is_not_well_formed_or_ambiguous_is_an_error() {
        assert!(decode_xml("<Project>").unwrap_err().to_string().starts_with("xml.decode(): "));
        let ambiguous = r#"<P xmlns:a="urn:a" xmlns:b="urn:b" a:x="1" b:x="2"/>"#;
        assert!(decode_xml(ambiguous).unwrap_err().to_string().contains("two attributes named \"x\""));
        assert!(decode_xml("<!DOCTYPE p [<!ENTITY e \"x\">]><p>&e;</p>").is_err());
    }
}
