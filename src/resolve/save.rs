//! Writing the results: package-lock.json from the ideal tree
//! (Shrinkwrap.commit) and package.json after add/remove
//! (@npmcli/package-json updateDependencies + save).

use std::fs;
use std::path::Path;

use serde_json::{Map, Value};

use super::build::{Builder, SaveType};
use super::npa::{self, Spec};
use super::tree::{Id, Tree, relpath};
use crate::collate;
use crate::json::Style;
use crate::lockmeta::{self, NodeInfo};
use crate::semver;

fn has_install_script(pkg: &Map<String, Value>) -> bool {
    if pkg.get("hasInstallScript").and_then(Value::as_bool) == Some(true) {
        return true;
    }
    let scripts = pkg.get("scripts").and_then(Value::as_object);
    ["install", "preinstall", "postinstall"].iter().any(|k| {
        scripts
            .and_then(|s| s.get(*k))
            .and_then(Value::as_str)
            .is_some_and(|v| !v.is_empty())
    })
}

fn meta(t: &Tree, n: Id) -> Map<String, Value> {
    let node = &t.nodes[n];
    if node.is_link {
        let root_path = &t.nodes[t.root].realpath;
        let mut m = Map::new();
        m.insert("resolved".into(), Value::String(relpath(root_path, &node.realpath)));
        m.insert("link".into(), Value::Bool(true));
        return m;
    }
    let is_root = n == t.root;
    let node_name = if node.parent.is_some() {
        node.name.clone()
    } else {
        node.realpath.rsplit('/').next().unwrap_or("").to_string()
    };
    let mut m = lockmeta::meta_from_node(&NodeInfo {
        package: &node.package,
        node_name: &node_name,
        is_top: t.is_top(n),
        integrity: node.integrity.as_deref(),
        resolved: node.resolved.as_deref(),
        in_bundle: node.in_bundle,
        has_shrinkwrap: node.has_shrinkwrap,
        has_install_script: has_install_script(&node.package),
        extraneous: node.extraneous && !is_root,
        dev: node.dev,
        optional: node.optional,
        dev_optional: node.dev_optional,
        peer: node.peer,
    });
    if is_root
        && let Some(name) = node
            .package
            .get("name")
            .filter(|v| v.as_str().is_some_and(|s| !s.is_empty()))
    {
        m.insert("name".into(), name.clone());
    }
    m
}

/// Shrinkwrap.commit + toString.
pub fn lockfile_text(t: &Tree, old_top: Option<&Map<String, Value>>, style: &Style) -> String {
    let mut data = Map::new();
    if let Some(old) = old_top {
        for (k, v) in old {
            if k != "packages" && k != "dependencies" {
                data.insert(k.clone(), v.clone());
            }
        }
    }
    let root = t.root;
    let root_pkg = &t.nodes[root].package;
    let name = root_pkg
        .get("name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| t.nodes[root].realpath.rsplit('/').next().unwrap_or("").to_string());
    data.insert("name".into(), Value::String(name));
    match root_pkg.get("version").and_then(Value::as_str) {
        Some(v) if !v.is_empty() => {
            data.insert("version".into(), Value::String(v.to_string()));
        }
        _ => {
            data.remove("version");
        }
    }
    data.insert("lockfileVersion".into(), Value::from(3));
    data.insert("requires".into(), Value::Bool(true));
    let mut packages = Map::new();
    let root_meta = meta(t, root);
    if !root_meta.is_empty() {
        packages.insert(String::new(), Value::Object(root_meta));
    }
    for n in t.inventory() {
        if n == root {
            continue;
        }
        let loc = relpath(&t.nodes[root].realpath, &t.nodes[n].path);
        packages.insert(loc, Value::Object(meta(t, n)));
    }
    data.insert("packages".into(), Value::Object(packages));
    lockmeta::lockfile_text(&Value::Object(data), style)
}

/// The spec saved for a dependency the user added (reify updateNodes).
pub fn saved_spec(t: &mut Tree, name: &str, raw: &str, save_exact: bool) -> Option<String> {
    let root = t.root;
    let edge = t.edge_out(root, name)?;
    let child = t.edge_to(edge)?;
    let spec = npa::parse(name, raw).ok()?;
    match &spec {
        Spec::Directory { path } => {
            let target = npa::resolve_path(&t.nodes[root].realpath, path);
            Some(format!("file:{}", relpath(&t.nodes[root].realpath, &target)))
        }
        _ => {
            let version = t.version(child).map(str::to_string);
            let prefix = if save_exact { "" } else { "^" };
            let prefix_range = match &version {
                Some(v) => format!("{prefix}{v}"),
                None => "*".to_string(),
            };
            let (range_spec, alias) = match &spec {
                Spec::Alias { name: real, spec } => (spec.as_ref().clone(), Some(real.clone())),
                other => (other.clone(), None),
            };
            let range = match &range_spec {
                Spec::Range { range } => {
                    if range.trim() == "*" || semver::subset(&prefix_range, range) {
                        prefix_range
                    } else {
                        range.clone()
                    }
                }
                _ => prefix_range,
            };
            Some(match alias {
                Some(real) => format!("npm:{real}@{range}"),
                None => range,
            })
        }
    }
}

const DEP_TYPES: [&str; 4] = [
    "dependencies",
    "optionalDependencies",
    "devDependencies",
    "peerDependencies",
];

/// json-parse-even-better-errors formatting, including the minified case
/// where both indent and newline are empty.
pub fn detect_raw(text: &str) -> (String, String) {
    let trimmed = text.trim_start();
    let Some(rest) = trimmed.strip_prefix('{').or_else(|| trimmed.strip_prefix('[')) else {
        return (String::new(), String::new());
    };
    let nl_end = rest.find(|c| c != '\r' && c != '\n').unwrap_or(rest.len());
    if nl_end == 0 {
        return (String::new(), String::new());
    }
    let s = Style::detect(text);
    (s.indent, s.newline)
}

/// Writes package.json with the root's dependency sections, the way
/// npm saves after install/uninstall. Returns whether the file changed.
pub fn save_package_json(dir: &Path, root_pkg: &Map<String, Value>) -> Result<bool, String> {
    let path = dir.join("package.json");
    let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let original: Map<String, Value> = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let (indent, newline) = detect_raw(&text);
    let mut pkg: Map<String, Value> = Map::new();
    for ty in DEP_TYPES {
        let mut entries: Vec<(String, Value)> = root_pkg
            .get(ty)
            .and_then(Value::as_object)
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        entries.sort_by(|a, b| collate::compare(&a.0, &b.0));
        pkg.insert(ty.to_string(), Value::Object(entries.into_iter().collect()));
    }
    let optional: Vec<String> = pkg["optionalDependencies"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    if let Some(Value::Object(deps)) = pkg.get_mut("dependencies") {
        for n in &optional {
            deps.remove(n);
        }
    }
    let mut result = original.clone();
    for ty in DEP_TYPES {
        let value = pkg[ty].clone();
        if value.as_object().is_some_and(|m| m.is_empty()) {
            result.remove(ty);
        } else {
            result.insert(ty.to_string(), value);
        }
    }
    if let (Some(Value::Object(orig_prod)), Some(Value::Object(orig_peer))) =
        (original.get("dependencies"), original.get("peerDependencies"))
        && let Some(Value::Object(new_peer)) = result.get("peerDependencies").cloned()
    {
        for name in orig_peer.keys() {
            if orig_prod.contains_key(name)
                && let Some(v) = new_peer.get(name)
            {
                let deps = result
                    .entry("dependencies")
                    .or_insert_with(|| Value::Object(Map::new()));
                if let Value::Object(d) = deps {
                    d.insert(name.clone(), v.clone());
                }
            }
        }
    }
    if let Some(b) = root_pkg.get("bundleDependencies")
        && original.contains_key("bundleDependencies")
    {
        result.insert("bundleDependencies".into(), b.clone());
    }
    let body = crate::json::stringify(
        &Value::Object(result),
        &Style {
            indent: indent.clone(),
            newline: "\n".into(),
        },
    );
    let body = if indent.is_empty() {
        // JSON.stringify with no indent, plus "\n" turned into the newline.
        let compact = body.trim_end_matches('\n').to_string();
        format!("{compact}{newline}")
    } else {
        body.replace('\n', if newline.is_empty() { "\n" } else { &newline })
    };
    if body.trim() == text.trim() {
        return Ok(false);
    }
    crate::json::write_atomic(&path, body.as_bytes()).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(true)
}

/// Applies the saved specs of added dependencies to the root package.
pub fn update_root_specs(b: &mut Builder, raw_specs: &[(String, String)], save_exact: bool) {
    let root = b.t.root;
    let added = b.added.clone();
    for (name, section) in added {
        let raw = raw_specs
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, s)| s.clone())
            .unwrap_or_default();
        let Some(spec) = saved_spec(&mut b.t, &name, &raw, save_exact) else {
            continue;
        };
        let field = section.field();
        let pkg = &mut b.t.nodes[root].package;
        let entry = pkg.entry(field).or_insert_with(|| Value::Object(Map::new()));
        if let Value::Object(m) = entry {
            m.insert(name.clone(), Value::String(spec));
        }
        if section == SaveType::Prod
            && let Some(Value::Object(o)) = pkg.get_mut("optionalDependencies")
        {
            o.remove(&name);
        }
    }
}
