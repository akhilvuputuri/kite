//! How arborist writes lockfile data: `Shrinkwrap.metaFromNode` for each
//! entry and json-stringify-nice for key order.

use std::cmp::Ordering;

use serde_json::{Map, Value};

use crate::collate;
use crate::json::{self, Style};

/// Shrinkwrap's preferred top keys (`swKeyOrder`).
pub const SW_KEY_ORDER: [&str; 8] = [
    "name",
    "version",
    "lockfileVersion",
    "resolved",
    "integrity",
    "requires",
    "packages",
    "dependencies",
];

const PKG_META_KEYS: [&str; 18] = [
    "version",
    "dependencies",
    "peerDependencies",
    "peerDependenciesMeta",
    "optionalDependencies",
    "bundleDependencies",
    "acceptDependencies",
    "funding",
    "engines",
    "os",
    "cpu",
    "_integrity",
    "license",
    "_hasShrinkwrap",
    "hasInstallScript",
    "bin",
    "deprecated",
    "workspaces",
];

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// `metaFieldFromPkg`: the license type of a license object, nothing for
/// falsy values and empty objects/arrays.
fn meta_field(pkg: &Map<String, Value>, key: &str) -> Option<Value> {
    let val = pkg.get(key)?;
    if !truthy(val) {
        return None;
    }
    if key == "license"
        && let Some(t) = val.get("type").filter(|t| truthy(t))
    {
        return Some(t.clone());
    }
    match val {
        Value::Object(m) if m.is_empty() => None,
        Value::Array(a) if a.is_empty() => None,
        _ => Some(val.clone()),
    }
}

/// What metaFromNode needs to know about a node.
pub struct NodeInfo<'a> {
    /// The manifest arborist holds for the node: the lockfile entry for
    /// registry packages, package.json for link targets and the root.
    pub package: &'a Map<String, Value>,
    /// The package name implied by the location.
    pub node_name: &'a str,
    pub is_top: bool,
    pub integrity: Option<&'a str>,
    pub resolved: Option<&'a str>,
    pub in_bundle: bool,
    pub has_shrinkwrap: bool,
    pub has_install_script: bool,
    pub extraneous: bool,
    pub dev: bool,
    pub optional: bool,
    pub dev_optional: bool,
    pub peer: bool,
}

pub fn meta_from_node(n: &NodeInfo) -> Map<String, Value> {
    let mut meta = Map::new();
    for key in PKG_META_KEYS {
        if let Some(v) = meta_field(n.package, key) {
            meta.insert(key.trim_start_matches('_').to_string(), v);
        }
    }
    if let Some(Value::String(pname)) = n.package.get("name")
        && !pname.is_empty()
        && pname != n.node_name
    {
        meta.insert("name".into(), Value::String(pname.clone()));
    }
    if n.is_top
        && let Some(dd) = n.package.get("devDependencies").filter(|v| truthy(v))
    {
        meta.insert("devDependencies".into(), dd.clone());
    }
    if let Some(i) = n.integrity.filter(|s| !s.is_empty()) {
        meta.insert("integrity".into(), Value::String(i.to_string()));
    }
    if n.in_bundle {
        meta.insert("inBundle".into(), Value::Bool(true));
    }
    if n.has_shrinkwrap {
        meta.insert("hasShrinkwrap".into(), Value::Bool(true));
    }
    if n.has_install_script {
        meta.insert("hasInstallScript".into(), Value::Bool(true));
    }
    if let Some(r) = n.resolved.filter(|s| !s.is_empty()) {
        meta.insert("resolved".into(), Value::String(r.to_string()));
    }
    if n.extraneous {
        meta.insert("extraneous".into(), Value::Bool(true));
    } else {
        if n.peer {
            meta.insert("peer".into(), Value::Bool(true));
        }
        if n.dev {
            meta.insert("dev".into(), Value::Bool(true));
        }
        if n.optional {
            meta.insert("optional".into(), Value::Bool(true));
        }
        if n.dev_optional && !n.dev && !n.optional {
            meta.insert("devOptional".into(), Value::Bool(true));
        }
    }
    meta
}

fn key_order(ak: &str, bk: &str, pref: &[&str]) -> Ordering {
    let ai = pref.iter().position(|k| *k == ak);
    let bi = pref.iter().position(|k| *k == bk);
    match (ai, bi) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => collate::compare(ak, bk),
    }
}

fn is_obj(v: &Value) -> bool {
    matches!(v, Value::Object(_))
}

/// json-stringify-nice key order applied recursively: non-objects first,
/// preferred keys first within each group, then ICU 'en' order. Keys that
/// are array indexes come first in ascending order, as JS objects do.
pub fn sort_nice(value: &Value, pref: &[&str]) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|(ak, av), (bk, bv)| match (is_obj(av), is_obj(bv)) {
                (a, b) if a == b => key_order(ak, bk, pref),
                (true, false) => Ordering::Greater,
                _ => Ordering::Less,
            });
            let mut index_keys: Vec<(u32, &String, &Value)> = Vec::new();
            let mut other: Vec<(&String, &Value)> = Vec::new();
            for (k, v) in entries {
                match array_index(k) {
                    Some(i) => index_keys.push((i, k, v)),
                    None => other.push((k, v)),
                }
            }
            index_keys.sort_by_key(|(i, _, _)| *i);
            let mut out = Map::new();
            for (_, k, v) in index_keys {
                out.insert(k.clone(), sort_nice(v, pref));
            }
            for (k, v) in other {
                out.insert(k.clone(), sort_nice(v, pref));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(|v| sort_nice(v, pref)).collect()),
        other => other.clone(),
    }
}

fn array_index(k: &str) -> Option<u32> {
    if k.is_empty() || (k.len() > 1 && k.starts_with('0')) || !k.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    k.parse::<u32>().ok().filter(|n| *n < u32::MAX)
}

/// Shrinkwrap#toString: stringify-nice with the lockfile key order, the
/// detected indent (2 spaces when none) and newline, plus a final newline.
pub fn lockfile_text(value: &Value, style: &Style) -> String {
    let indent = if style.indent.is_empty() {
        "  ".to_string()
    } else {
        style.indent.clone()
    };
    let newline = if style.newline.is_empty() {
        "\n".to_string()
    } else {
        style.newline.clone()
    };
    json::stringify(&sort_nice(value, &SW_KEY_ORDER), &Style { indent, newline })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn nice_order() {
        let v = json!({"packages": {"b": {}, "a": {}}, "requires": true, "lockfileVersion": 3, "name": "x", "zeta": 1, "alpha": [1]});
        let s = serde_json::to_string(&sort_nice(&v, &SW_KEY_ORDER)).unwrap();
        assert_eq!(
            s,
            r#"{"name":"x","lockfileVersion":3,"requires":true,"alpha":[1],"zeta":1,"packages":{"a":{},"b":{}}}"#
        );
    }

    #[test]
    fn meta_drops_unknown_and_libc() {
        let pkg = json!({"version": "1.0.0", "libc": ["glibc"], "license": {"type": "MIT"}, "engines": {}, "bin": {"x": "y"}, "custom": 1});
        let meta = meta_from_node(&NodeInfo {
            package: pkg.as_object().unwrap(),
            node_name: "a",
            is_top: false,
            integrity: Some("sha512-x"),
            resolved: Some("https://registry.npmjs.org/a/-/a-1.0.0.tgz"),
            in_bundle: false,
            has_shrinkwrap: false,
            has_install_script: false,
            extraneous: false,
            dev: true,
            optional: true,
            dev_optional: true,
            peer: false,
        });
        let keys: Vec<&str> = meta.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            ["version", "license", "bin", "integrity", "resolved", "dev", "optional"]
        );
    }
}
