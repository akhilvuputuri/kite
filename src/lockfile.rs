//! Reading `package-lock.json` (lockfileVersion 3 only).

use std::fs;
use std::path::Path;

use serde_json::{Map, Value};

use crate::json::Style;

#[derive(Clone, Debug, Default)]
pub struct Entry {
    /// Location relative to the project root, e.g. `node_modules/a/node_modules/b`.
    pub path: String,
    pub name: Option<String>,
    pub version: Option<String>,
    pub resolved: Option<String>,
    pub integrity: Option<String>,
    pub link: bool,
    pub dev: bool,
    pub optional: bool,
    pub dev_optional: bool,
    pub peer: bool,
    pub os: Vec<String>,
    pub cpu: Vec<String>,
    pub libc: Vec<String>,
    pub bin: Vec<(String, String)>,
    pub has_install_script: bool,
    pub in_bundle: bool,
    pub has_shrinkwrap: bool,
    pub dependencies: Vec<String>,
    pub optional_dependencies: Vec<String>,
    pub peer_dependencies: Vec<String>,
    /// The entry exactly as written, for the hidden lockfile.
    pub raw: Map<String, Value>,
}

impl Entry {
    pub fn is_root(&self) -> bool {
        self.path.is_empty()
    }

    /// Whether the entry lives in a `node_modules` folder (as opposed to the
    /// root or a linked local package such as `coding_runtime_pi`).
    pub fn in_node_modules(&self) -> bool {
        self.path.starts_with("node_modules/") || self.path.contains("/node_modules/")
    }

    /// The package name implied by the location, which is what npm uses.
    pub fn location_name(&self) -> &str {
        location_name(&self.path)
    }
}

pub fn location_name(path: &str) -> &str {
    match path.rfind("node_modules/") {
        Some(i) => &path[i + "node_modules/".len()..],
        None => path.rsplit('/').next().unwrap_or(path),
    }
}

#[derive(Clone, Debug)]
pub struct Lockfile {
    pub raw: Map<String, Value>,
    pub style: Style,
    pub entries: Vec<Entry>,
}

impl Lockfile {
    pub fn read(path: &Path) -> Result<Self, String> {
        let text = fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        Self::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let style = Style::detect(text);
        let value: Value = serde_json::from_str(text).map_err(|e| format!("invalid JSON: {e}"))?;
        let Value::Object(raw) = value else {
            return Err("lockfile is not a JSON object".into());
        };
        match raw.get("lockfileVersion").and_then(Value::as_u64) {
            Some(3) => {}
            Some(v) => {
                return Err(format!(
                    "lockfileVersion {v} is not supported; only lockfileVersion 3 is"
                ));
            }
            None => return Err("lockfileVersion is missing".into()),
        }
        let packages = raw
            .get("packages")
            .and_then(Value::as_object)
            .ok_or("lockfile has no \"packages\" object")?;
        let mut entries = Vec::with_capacity(packages.len());
        for (path, value) in packages {
            let obj = value
                .as_object()
                .ok_or_else(|| format!("packages[{path:?}] is not an object"))?;
            entries.push(parse_entry(path, obj)?);
        }
        Ok(Lockfile { raw, style, entries })
    }

    pub fn root(&self) -> Option<&Entry> {
        self.entries.iter().find(|e| e.is_root())
    }
}

fn strings(obj: &Map<String, Value>, key: &str) -> Vec<String> {
    match obj.get(key) {
        Some(Value::Array(items)) => items.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
        Some(Value::String(s)) => vec![s.clone()],
        _ => Vec::new(),
    }
}

fn keys(obj: &Map<String, Value>, key: &str) -> Vec<String> {
    obj.get(key)
        .and_then(Value::as_object)
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

fn flag(obj: &Map<String, Value>, key: &str) -> bool {
    obj.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn text(obj: &Map<String, Value>, key: &str) -> Option<String> {
    obj.get(key).and_then(Value::as_str).map(str::to_string)
}

fn parse_entry(path: &str, obj: &Map<String, Value>) -> Result<Entry, String> {
    if path.split('/').any(|part| part == ".." || part == ".") || path.starts_with('/') {
        return Err(format!("unsafe package location {path:?}"));
    }
    let bin = match obj.get("bin") {
        Some(Value::Object(map)) => map
            .iter()
            .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
            .collect(),
        _ => Vec::new(),
    };
    Ok(Entry {
        path: path.to_string(),
        name: text(obj, "name"),
        version: text(obj, "version"),
        resolved: text(obj, "resolved"),
        integrity: text(obj, "integrity"),
        link: flag(obj, "link"),
        dev: flag(obj, "dev"),
        optional: flag(obj, "optional"),
        dev_optional: flag(obj, "devOptional"),
        peer: flag(obj, "peer"),
        os: strings(obj, "os"),
        cpu: strings(obj, "cpu"),
        libc: strings(obj, "libc"),
        bin,
        has_install_script: flag(obj, "hasInstallScript"),
        in_bundle: flag(obj, "inBundle"),
        has_shrinkwrap: flag(obj, "hasShrinkwrap"),
        dependencies: keys(obj, "dependencies"),
        optional_dependencies: keys(obj, "optionalDependencies"),
        peer_dependencies: keys(obj, "peerDependencies"),
        raw: obj.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn location_names() {
        assert_eq!(location_name("node_modules/a"), "a");
        assert_eq!(location_name("node_modules/@s/a/node_modules/@t/b"), "@t/b");
        assert_eq!(location_name("coding_runtime_pi"), "coding_runtime_pi");
    }

    #[test]
    fn rejects_old_versions_and_unsafe_paths() {
        assert!(Lockfile::parse(r#"{"lockfileVersion":2,"packages":{}}"#).is_err());
        assert!(Lockfile::parse(r#"{"lockfileVersion":3,"packages":{"node_modules/../x":{}}}"#).is_err());
    }
}
