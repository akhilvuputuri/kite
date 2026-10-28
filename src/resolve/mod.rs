//! `kite install`, `uninstall` and `update`: resolve package.json into
//! a package-lock.json the way npm 10.9 does, then install it.

mod build;
mod flags;
mod npa;
mod place;
mod registry;
mod save;
mod tree;

use std::fs;

use serde_json::Value;

pub use build::SaveType;
use build::{AddRequest, Builder, Requests, normalize_root_package};
pub use registry::NPM_VERSION;
use registry::{PickSpec, Registry};

use crate::install::{self, CiOptions};
use crate::lockfile::Lockfile;
use crate::semver::Version;

pub struct ResolveOptions {
    pub ci: CiOptions,
    /// `name[@spec]` arguments to add.
    pub add: Vec<String>,
    pub save_type: Option<SaveType>,
    pub save_exact: bool,
    pub rm: Vec<String>,
    pub update: Option<Vec<String>>,
    pub lockfile_only: bool,
    pub before: Option<String>,
}

/// The version of `node` on PATH, asked once per run and only when an
/// engines check needs it.
pub fn node_version() -> Option<Version> {
    static NODE: std::sync::OnceLock<Option<Version>> = std::sync::OnceLock::new();
    NODE.get_or_init(|| {
        let out = std::process::Command::new("node").arg("--version").output().ok()?;
        Version::parse(String::from_utf8_lossy(&out.stdout).trim())
    })
    .clone()
}

/// Normalises `--before` to the registry's `time` format so the strings
/// compare chronologically.
fn normalize_before(before: &str) -> Result<String, String> {
    let b = before.trim();
    if !b.is_ascii() {
        return Err(format!("--before {before:?}: use an ISO date such as 2026-10-01"));
    }
    let ok = |s: &str| s.len() >= 10 && s.as_bytes()[4] == b'-' && s.as_bytes()[7] == b'-';
    if !ok(b) {
        return Err(format!(
            "--before {before:?}: use an ISO date such as 2026-10-01 or 2026-10-01T12:00:00Z"
        ));
    }
    if b.len() == 10 {
        return Ok(format!("{b}T00:00:00.000Z"));
    }
    let core = b.trim_end_matches('Z');
    let (date_time, frac) = match core.split_once('.') {
        Some((dt, f)) => (dt.to_string(), f.to_string()),
        None => (core.to_string(), "000".to_string()),
    };
    let date_time = if date_time.len() == 16 {
        format!("{date_time}:00")
    } else {
        date_time
    };
    Ok(format!("{date_time}.{:0<3}Z", &frac[..frac.len().min(3)]))
}

/// arborist `assertNoNewer`: the hidden lockfile describes node_modules
/// only if no folder it walks (node_modules, scopes, packages, nested
/// node_modules and symlinked targets) is newer than the hidden lockfile,
/// every package folder is listed in it, and every listed package was found.
/// Names starting with `.` and plain files are ignored.
pub fn hidden_lockfile_current(project: &std::path::Path, hidden: &std::path::Path) -> bool {
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};
    let Ok(lock_time) = fs::metadata(hidden).and_then(|m| m.modified()) else {
        return false;
    };
    let Ok(data) = Lockfile::read(hidden) else { return false };
    let listed: HashSet<String> = data.entries.iter().map(|e| e.path.clone()).collect();
    let root = match fs::canonicalize(project) {
        Ok(r) => r,
        Err(_) => return false,
    };
    let rel = |p: &Path| -> String {
        p.strip_prefix(&root)
            .map(|r| r.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    fn walk(
        dir: &Path,
        root: &Path,
        lock_time: std::time::SystemTime,
        listed: &HashSet<String>,
        seen: &mut HashSet<String>,
        rel: &dyn Fn(&Path) -> String,
        depth: usize,
    ) -> bool {
        if depth > 64 {
            return false;
        }
        let base = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let is_nm = base == "node_modules";
        let is_scope = base.starts_with('@');
        let here = rel(dir);
        seen.insert(here.clone());
        match fs::metadata(dir).and_then(|m| m.modified()) {
            Ok(t) if t <= lock_time => {}
            _ => return false,
        }
        if !is_scope && !is_nm && !listed.contains(&here) {
            return false;
        }
        let parent: PathBuf = if is_nm || is_scope {
            dir.to_path_buf()
        } else {
            dir.join("node_modules")
        };
        let Ok(entries) = fs::read_dir(&parent) else {
            return true;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(ft) = entry.file_type() else { continue };
            let child = parent.join(&name);
            if ft.is_dir() && !name.starts_with('.') {
                if !walk(&child, root, lock_time, listed, seen, rel, depth + 1) {
                    return false;
                }
            } else if ft.is_symlink() {
                seen.insert(rel(&child));
                if let Ok(target) = fs::canonicalize(&child)
                    && target.is_dir()
                    && target.starts_with(root)
                    && !seen.contains(&rel(&target))
                    && !walk(&target, root, lock_time, listed, seen, rel, depth + 1)
                {
                    return false;
                }
            }
        }
        true
    }
    let mut seen = HashSet::new();
    if !walk(
        &root.join("node_modules"),
        &root,
        lock_time,
        &listed,
        &mut seen,
        &rel,
        0,
    ) {
        return false;
    }
    listed.iter().all(|loc| seen.contains(loc))
}

pub fn run(opts: ResolveOptions) -> Result<(), String> {
    // Fail on configuration errors before package.json or the lockfile change.
    install::network_concurrency()?;
    let dir = opts.ci.dir.clone();
    let project = dir.to_string_lossy().trim_end_matches('/').to_string();
    let pkg_path = dir.join("package.json");
    let pkg_text = fs::read_to_string(&pkg_path).map_err(|e| format!("cannot read {}: {e}", pkg_path.display()))?;
    let raw_pkg: serde_json::Map<String, Value> = match serde_json::from_str(&pkg_text) {
        Ok(Value::Object(m)) => m,
        _ => return Err(format!("{} is not a JSON object", pkg_path.display())),
    };
    for unsupported in ["overrides", "workspaces", "bundleDependencies", "bundledDependencies"] {
        if raw_pkg.contains_key(unsupported) {
            return Err(format!("package.json \"{unsupported}\" is not supported by kite"));
        }
    }
    if dir.join("npm-shrinkwrap.json").exists() {
        return Err("npm-shrinkwrap.json is not supported".into());
    }
    let lock_path = dir.join("package-lock.json");
    let lock = if lock_path.exists() {
        Some(Lockfile::read(&lock_path)?)
    } else {
        // Without a lockfile npm starts from what is installed (loadActual).
        // node_modules/.package-lock.json records exactly that; without it
        // kite refuses rather than resolve differently.
        let hidden = dir.join("node_modules/.package-lock.json");
        if hidden.exists() {
            // Arborist trusts the hidden lockfile only when nothing in
            // node_modules changed after it was written.
            if !hidden_lockfile_current(&dir, &hidden) {
                return Err(
                    "there is no package-lock.json, and node_modules changed after it was last installed; \
                            remove node_modules or restore package-lock.json"
                        .into(),
                );
            }
            Some(Lockfile::read(&hidden)?)
        } else if fs::read_dir(dir.join("node_modules"))
            .map(|d| d.flatten().any(|e| !e.file_name().to_string_lossy().starts_with('.')))
            .unwrap_or(false)
        {
            return Err(
                "there is no package-lock.json, and node_modules was not installed by npm or kite; \
                        remove node_modules or restore package-lock.json"
                    .into(),
            );
        } else {
            None
        }
    };
    let before = opts.before.as_deref().map(normalize_before).transpose()?;
    let reg = Registry::new(opts.ci.store.join("packuments"), before);

    // `name@tag` is pinned to the tag's version before resolution (npm #add).
    let mut add: Vec<AddRequest> = Vec::new();
    for arg in &opts.add {
        let (name, spec) = npa::split_name_spec(arg);
        npa::check_name(name)?;
        let mut raw_spec = spec.unwrap_or("*").to_string();
        if let Ok(npa::Spec::Tag { tag }) = npa::parse(name, &raw_spec) {
            let doc = reg.packument(name)?;
            let m = reg.pick(&doc, &PickSpec::Tag(tag))?;
            raw_spec = m.get("version").and_then(Value::as_str).unwrap_or("*").to_string();
        }
        add.push(AddRequest {
            name: name.to_string(),
            raw_spec,
        });
    }
    let update_all = matches!(&opts.update, Some(names) if names.is_empty());
    let requests = Requests {
        add: add.clone(),
        save_type: opts.save_type,
        rm: opts.rm.clone(),
        update_all,
        update_names: opts.update.clone().unwrap_or_default(),
    };
    let root_pkg = normalize_root_package(raw_pkg);
    let mut b = Builder::new(&project, root_pkg, lock.as_ref(), &reg, update_all)?;
    b.apply_requests(&requests)?;
    b.build_deps()?;
    b.finish()?;

    let raw_specs: Vec<(String, String)> = add.iter().map(|a| (a.name.clone(), a.raw_spec.clone())).collect();
    save::update_root_specs(&mut b, &raw_specs, opts.save_exact);
    if !opts.add.is_empty() || !opts.rm.is_empty() {
        let root = b.t.root;
        save::save_package_json(&dir, &b.t.nodes[root].package)?;
    }
    let style = crate::json::Style::detect(&fs::read_to_string(&pkg_path).unwrap_or_default());
    let text = save::lockfile_text(&b.t, lock.as_ref().map(|l| &l.raw), &style);
    let changed = fs::read_to_string(&lock_path).map(|old| old != text).unwrap_or(true);
    if changed {
        crate::json::write_atomic(&lock_path, text.as_bytes()).map_err(|e| format!("{}: {e}", lock_path.display()))?;
    }
    if !opts.ci.quiet {
        let count = b.t.inventory().len().saturating_sub(1);
        eprintln!(
            "kite: {} package-lock.json ({count} entries)",
            if changed { "wrote" } else { "kept" }
        );
    }
    if opts.lockfile_only {
        return Ok(());
    }
    // npm install reifies incrementally and runs the root lifecycle only
    // when no packages were named.
    let root_scripts = opts.add.is_empty() && opts.rm.is_empty() && opts.update.is_none();
    let command = if !opts.rm.is_empty() {
        "uninstall"
    } else if opts.update.is_some() {
        "update"
    } else {
        "install"
    };
    install::reify(
        &opts.ci,
        install::Mode::Incremental { root_scripts, command },
        lock.as_ref(),
    )
}

/// Dependency flags of one lockfile location, as npm ci computes them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NodeFlags {
    pub dev: bool,
    pub optional: bool,
    pub dev_optional: bool,
    pub peer: bool,
}

/// What `npm ci` does before installing: build the ideal tree from the
/// lockfile and package.json, refuse if it disagrees with the lockfile
/// (validate-lockfile.js), and use its recomputed dependency flags.
pub fn ci_ideal_flags(
    dir: &std::path::Path,
    lock: &Lockfile,
    store: &std::path::Path,
) -> Result<std::collections::HashMap<String, NodeFlags>, String> {
    let project = dir.to_string_lossy().trim_end_matches('/').to_string();
    let pkg_path = dir.join("package.json");
    let text = fs::read_to_string(&pkg_path).map_err(|e| format!("cannot read {}: {e}", pkg_path.display()))?;
    let raw: serde_json::Map<String, Value> = match serde_json::from_str(&text) {
        Ok(Value::Object(m)) => m,
        _ => return Err(format!("{} is not a JSON object", pkg_path.display())),
    };
    let reg = Registry::new(store.join("packuments"), None);
    let mut b = Builder::new(&project, normalize_root_package(raw), Some(lock), &reg, false)?;
    b.apply_requests(&Requests::default())?;
    b.build_deps()?;
    b.finish()?;
    let mut locked: std::collections::HashMap<&str, Option<&str>> = std::collections::HashMap::new();
    for e in &lock.entries {
        locked.insert(e.path.as_str(), e.version.as_deref());
    }
    let mut errors = Vec::new();
    let mut flags = std::collections::HashMap::new();
    let root = b.t.root;
    for n in b.t.inventory() {
        if n == root {
            continue;
        }
        let loc = tree::relpath(&b.t.nodes[root].realpath, &b.t.nodes[n].path);
        let node = &b.t.nodes[n];
        let name = b.t.package_name(n).unwrap_or(&node.name).to_string();
        let version = if node.is_link { None } else { b.t.version(n) };
        match locked.get(loc.as_str()) {
            None => errors.push(format!("Missing: {name}@{} from lock file", version.unwrap_or(""))),
            Some(lv) => {
                if !node.is_link && version.is_some() && *lv != version {
                    errors.push(format!(
                        "Invalid: lock file's {name}@{} does not satisfy {name}@{}",
                        lv.unwrap_or(""),
                        version.unwrap_or("")
                    ));
                }
            }
        }
        flags.insert(
            loc,
            NodeFlags {
                dev: node.dev,
                optional: node.optional,
                dev_optional: node.dev_optional,
                peer: node.peer,
            },
        );
    }
    if !errors.is_empty() {
        return Err(format!(
            "`kite ci` can only install packages when package.json and package-lock.json are in sync. \
             Update the lock file with `kite install` before continuing.\n\n{}",
            errors.join("\n")
        ));
    }
    Ok(flags)
}

#[cfg(test)]
mod tests {
    use super::hidden_lockfile_current;
    use std::fs;
    use std::time::{Duration, SystemTime};

    #[test]
    fn freshness_ignores_dot_folders_but_not_unlisted_packages() {
        let dir = std::env::temp_dir().join(format!("kite-fresh-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let nm = dir.join("node_modules");
        fs::create_dir_all(nm.join("a")).unwrap();
        fs::create_dir_all(nm.join(".vite/deps")).unwrap();
        let hidden = nm.join(".package-lock.json");
        fs::write(
            &hidden,
            r#"{"name":"x","lockfileVersion":3,"requires":true,"packages":{"node_modules/a":{"version":"1.0.0"}}}"#,
        )
        .unwrap();
        let later = SystemTime::now() + Duration::from_secs(60);
        fs::File::options()
            .write(true)
            .open(&hidden)
            .unwrap()
            .set_modified(later)
            .unwrap();
        // A tool cache changing inside a dot folder keeps the tree current.
        fs::write(nm.join(".vite/deps/x"), "").unwrap();
        assert!(hidden_lockfile_current(&dir, &hidden));
        // A package the hidden lockfile does not list makes it stale.
        fs::create_dir_all(nm.join("b")).unwrap();
        assert!(!hidden_lockfile_current(&dir, &hidden));
        let _ = fs::remove_dir_all(&dir);
    }
}
