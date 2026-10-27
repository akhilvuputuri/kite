//! `lockstep ci`: install exactly what `package-lock.json` describes.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{Map, Value};

use crate::bins::{self, BinLinker};
use crate::collate;
use crate::graph::Graph;
use crate::integrity::Integrity;
use crate::json;
use crate::link;
use crate::lockfile::{Entry, Lockfile};
use crate::lockmeta;
use crate::platform::Platform;
use crate::pool;
use crate::scripts;
use crate::semver;
use crate::store::Store;

#[derive(Clone, Debug, Default)]
pub struct Omit {
    pub dev: bool,
    pub optional: bool,
    pub peer: bool,
}

impl Omit {
    pub fn excludes(&self, e: &Entry) -> bool {
        (e.dev && self.dev)
            || (e.optional && self.optional)
            || (e.dev_optional && self.dev && self.optional)
            || (e.peer && self.peer)
    }
}

pub struct CiOptions {
    pub dir: PathBuf,
    pub omit: Omit,
    pub ignore_scripts: bool,
    pub store: PathBuf,
    pub jobs: usize,
    pub quiet: bool,
}

/// The process umask, read once: reading it means setting it, which must
/// not race with threads creating files.
pub fn current_umask() -> u32 {
    static UMASK: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *UMASK.get_or_init(|| {
        // SAFETY: umask has no failure mode; it is set back immediately,
        // and the first call happens in main before any worker starts.
        unsafe {
            let old = libc::umask(0o022);
            libc::umask(old);
            old as u32
        }
    })
}

pub fn read_manifest(dir: &Path) -> Result<Map<String, Value>, String> {
    let path = dir.join("package.json");
    let text = fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    match serde_json::from_str(&text) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err(format!("{} is not a JSON object", path.display())),
        Err(e) => Err(format!("{}: invalid JSON: {e}", path.display())),
    }
}

struct Plan {
    /// Indexes of entries placed in a node_modules folder, in lockfile order.
    install: Vec<usize>,
    /// Entries that end up in the tree (installed plus link targets).
    present: HashSet<usize>,
    /// Entries in the lockfile that are not installed (omitted or skipped).
    skipped: Vec<usize>,
}

/// The node version that `node` on PATH reports, for engines checks.
fn node_version() -> Option<semver::Version> {
    let out = std::process::Command::new("node").arg("--version").output().ok()?;
    semver::Version::parse(String::from_utf8_lossy(&out.stdout).trim())
}

fn engines_ok(e: &Entry, node: &mut Option<Option<semver::Version>>) -> bool {
    // npm checks engines.npm against its own version too.
    if let Some(npm) = e.raw.get("engines").and_then(|v| v.get("npm")).and_then(Value::as_str) {
        let ok = match (
            semver::Range::parse(npm),
            semver::Version::parse(crate::resolve::NPM_VERSION),
        ) {
            (Some(r), Some(v)) => r.satisfies_including_prerelease(&v),
            _ => false,
        };
        if !ok {
            return false;
        }
    }
    let Some(range) = e.raw.get("engines").and_then(|v| v.get("node")).and_then(Value::as_str) else {
        return true;
    };
    let version = node.get_or_insert_with(node_version);
    match (version, semver::Range::parse(range)) {
        (Some(v), Some(r)) => r.satisfies_including_prerelease(v),
        // npm-install-checks treats an unparseable range as unsatisfied;
        // an unknown node version cannot be checked, so it passes.
        (Some(_), None) => false,
        (None, _) => true,
    }
}

fn plan(lock: &Lockfile, omit: &Omit, platform: &Platform) -> Result<Plan, String> {
    let entries = &lock.entries;
    let graph = Graph::build(entries);
    let mut skipped = vec![false; entries.len()];
    // reify.js #addOmitsToTrashList: only nodes whose top is the project
    // root are filtered, so a link target's own dependencies always stay.
    for (i, e) in entries.iter().enumerate() {
        if e.path.starts_with("node_modules/") && omit.excludes(e) {
            skipped[i] = true;
        }
    }
    let mut node: Option<Option<semver::Version>> = None;
    for (i, e) in entries.iter().enumerate() {
        if e.is_root() || skipped[i] || e.link || !e.in_node_modules() {
            continue;
        }
        let platform_ok = platform.accepts(&e.os, &e.cpu, &e.libc);
        if !e.optional {
            // build-ideal-tree #checkEngineAndPlatform throws for these.
            if !platform_ok {
                return Err(format!(
                    "EBADPLATFORM: {} is for os {:?} cpu {:?}, but this machine is {} {}",
                    e.path, e.os, e.cpu, platform.os, platform.cpu
                ));
            }
            continue;
        }
        if platform_ok && engines_ok(e, &mut node) {
            continue;
        }
        for j in graph.optional_set(i) {
            skipped[j] = true;
        }
    }
    let mut install = Vec::new();
    let mut present = HashSet::new();
    let mut skipped_list = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        if e.is_root() {
            continue;
        }
        if skipped[i] {
            skipped_list.push(i);
            continue;
        }
        present.insert(i);
        if e.in_node_modules() {
            install.push(i);
        }
    }
    Ok(Plan {
        install,
        present,
        skipped: skipped_list,
    })
}

fn depth(path: &str) -> usize {
    path.matches("node_modules/").count()
}

fn remove_contents(dir: &Path) -> io::Result<()> {
    let read = match fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in read {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            fs::remove_dir_all(&path)?;
        } else {
            fs::remove_file(&path)?;
        }
    }
    Ok(())
}

fn sort_nodes(lock: &Lockfile, indexes: &mut [usize]) {
    // rebuild.js sortNodes: depth, then ICU 'en' order of the path.
    indexes.sort_by(|&a, &b| {
        let (pa, pb) = (&lock.entries[a].path, &lock.entries[b].path);
        depth(pa).cmp(&depth(pb)).then_with(|| collate::compare(pa, pb))
    });
}

fn link_target<'a>(lock: &'a Lockfile, e: &Entry) -> Option<&'a Entry> {
    let target = e.resolved.as_deref()?;
    lock.entries.iter().find(|t| t.path == target)
}

pub fn ci(opts: &CiOptions) -> Result<(), String> {
    let started = Instant::now();
    let timing = std::env::var_os("LOCKSTEP_TIMING").is_some();
    let mut last = Instant::now();
    let mut mark = |label: &str| {
        if timing {
            eprintln!(
                "lockstep: {label:<14} {:>7.1} ms",
                last.elapsed().as_secs_f64() * 1000.0
            );
            last = Instant::now();
        }
    };
    let dir = &opts.dir;
    let manifest = read_manifest(dir)?;
    let mut lock = Lockfile::read(&dir.join("package-lock.json"))?;
    lock.root().ok_or("lockfile has no root entry (\"\")")?;
    // npm ci's ideal tree decides validity and the dependency flags.
    let ideal = crate::resolve::ci_ideal_flags(dir, &lock, &opts.store)?;
    // Extraneous lockfile entries are pruned from the ideal tree, so they
    // are not installed.
    lock.entries.retain(|e| e.is_root() || ideal.contains_key(&e.path));
    for e in &mut lock.entries {
        if let Some(f) = ideal.get(&e.path) {
            e.dev = f.dev;
            e.optional = f.optional;
            e.dev_optional = f.dev_optional;
            e.peer = f.peer;
        }
    }
    for e in &lock.entries {
        if e.is_root() || !e.in_node_modules() || e.link {
            continue;
        }
        if e.in_bundle || e.has_shrinkwrap {
            return Err(format!(
                "{}: bundled dependencies and npm-shrinkwrap.json are not supported",
                e.path
            ));
        }
        let resolved = e
            .resolved
            .as_deref()
            .ok_or_else(|| format!("{}: no resolved URL", e.path))?;
        crate::fetch::check_registry_url(resolved).map_err(|m| format!("{}: {m}", e.path))?;
        if e.integrity.is_none() {
            return Err(format!("{}: no integrity in the lockfile", e.path));
        }
    }
    for e in &lock.entries {
        if e.link && link_target(&lock, e).is_none() {
            return Err(format!(
                "{}: link target {:?} is not in the lockfile",
                e.path, e.resolved
            ));
        }
    }

    mark("validate");
    let platform = Platform::current();
    let umask = current_umask();
    let mut plan = plan(&lock, &opts.omit, &platform)?;
    let store = Store::new(opts.store.clone(), umask);

    // Every package to fetch, by integrity.
    let mut unique: Vec<(Integrity, String)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut integrity_of: HashMap<usize, Integrity> = HashMap::new();
    for &i in &plan.install {
        let e = &lock.entries[i];
        if e.link {
            continue;
        }
        let integrity =
            Integrity::parse(e.integrity.as_deref().unwrap_or_default()).map_err(|m| format!("{}: {m}", e.path))?;
        if seen.insert(integrity.store_key()) {
            unique.push((integrity.clone(), e.resolved.clone().unwrap_or_default()));
        }
        integrity_of.insert(i, integrity);
    }
    // npm ci empties the root node_modules (keeping the folder itself)
    // once the lockfile is validated. Link targets' node_modules are not
    // emptied; their packages are replaced one by one.
    let root_nm = dir.join("node_modules");
    remove_contents(&root_nm).map_err(|e| format!("removing {}: {e}", root_nm.display()))?;
    link::mkdirp(&root_nm, umask).map_err(|e| format!("{}: {e}", root_nm.display()))?;

    // reify's sparse tree creates every package folder before omitted and
    // skipped ones are trashed, so their parent folders (such as an empty
    // scope) remain.
    let mut skipped_set: HashSet<&str> = HashSet::new();
    for &i in &plan.skipped {
        skipped_set.insert(lock.entries[i].path.as_str());
    }
    for &i in &plan.skipped {
        let e = &lock.entries[i];
        if !e.in_node_modules() || e.link {
            continue;
        }
        let mut ancestor = crate::graph::parent_location(&e.path);
        let mut buried = false;
        while let Some(a) = ancestor {
            if skipped_set.contains(a) {
                buried = true;
                break;
            }
            ancestor = crate::graph::parent_location(a);
        }
        if buried {
            continue;
        }
        if let Some(parent) = Path::new(&e.path).parent() {
            let parent = dir.join(parent);
            link::mkdirp(&parent, umask).map_err(|err| format!("{}: {err}", parent.display()))?;
        }
    }
    mark("clear");

    let place = |i: usize, pkg: Option<&crate::store::StoredPackage>| -> Result<(), String> {
        let e = &lock.entries[i];
        let dest = dir.join(&e.path);
        if let Some(parent) = dest.parent() {
            link::mkdirp(parent, umask).map_err(|err| format!("{}: {err}", parent.display()))?;
        }
        remove_path(&dest).map_err(|err| format!("{}: {err}", dest.display()))?;
        if e.link {
            let target_abs = dir.join(e.resolved.as_deref().unwrap_or_default());
            let rel = relative_path(dest.parent().unwrap_or(dir), &target_abs);
            std::os::unix::fs::symlink(&rel, &dest).map_err(|err| format!("{}: {err}", dest.display()))?;
            return Ok(());
        }
        let pkg = pkg.expect("registry packages are stored before placing");
        link::materialize(&pkg.dir, &pkg.files, &dest, umask, e.has_install_script, &|| {
            crate::store::mark_linked(&pkg.dir)
        })
        .map_err(|err| format!("{}: {err}", e.path))
    };

    // Download, verify and store each package; a package directly under a
    // node_modules root is placed as soon as its store entry exists, while
    // nested ones wait for their parents.
    let mut locations: HashMap<String, Vec<usize>> = HashMap::new();
    for (&i, integrity) in &integrity_of {
        locations.entry(integrity.store_key()).or_default().push(i);
    }
    let fetch_jobs = (opts.jobs * 4).clamp(16, 48);
    let stored = pool::map(&unique, fetch_jobs, |(integrity, url)| {
        let key = integrity.store_key();
        let pkg = store.ensure(integrity, url)?;
        for &i in locations.get(&key).map(Vec::as_slice).unwrap_or_default() {
            if depth(&lock.entries[i].path) == 1 {
                place(i, Some(&pkg))?;
            }
        }
        Ok::<_, String>((key, pkg))
    })?;
    if timing {
        use std::sync::atomic::Ordering::Relaxed;
        eprintln!(
            "lockstep:   (thread time: download+unpack {} ms, {} MB, {} store entries replaced)",
            crate::store::DOWNLOAD_US.load(Relaxed) / 1000,
            crate::store::BYTES.load(Relaxed) / 1_000_000,
            crate::store::TAMPERED.load(Relaxed)
        );
    }
    mark("fetch+store");
    let downloaded = stored.iter().filter(|(_, p)| p.downloaded).count();
    let stored: HashMap<String, crate::store::StoredPackage> = stored.into_iter().collect();

    // Links and nested packages, shallowest first.
    let mut by_depth: Vec<Vec<usize>> = Vec::new();
    for &i in &plan.install {
        let e = &lock.entries[i];
        let d = depth(&e.path);
        if d == 1 && !e.link {
            continue;
        }
        if by_depth.len() <= d {
            by_depth.resize(d + 1, Vec::new());
        }
        by_depth[d].push(i);
    }
    for level in &by_depth {
        if level.is_empty() {
            continue;
        }
        pool::map(level, opts.jobs * 2, |&i| {
            let pkg = integrity_of.get(&i).map(|k| &stored[&k.store_key()]);
            place(i, pkg)
        })?;
    }

    mark("place");
    // Build: rebuild.js runs registry dependencies first, then links.
    let ctx = scripts::Context {
        command: "ci",
        node_env_production: opts.omit.dev,
    };
    let mut deps: Vec<usize> = plan
        .install
        .iter()
        .copied()
        .filter(|&i| !lock.entries[i].link)
        .collect();
    let mut links: Vec<usize> = plan.install.iter().copied().filter(|&i| lock.entries[i].link).collect();
    sort_nodes(&lock, &mut deps);
    sort_nodes(&lock, &mut links);

    let mut dep_nodes = Vec::new();
    for &i in &deps {
        let e = &lock.entries[i];
        if !e.has_install_script {
            continue;
        }
        let cwd = dir.join(&e.path);
        let mut package = e.raw.clone();
        package
            .entry("name")
            .or_insert_with(|| Value::String(e.location_name().to_string()));
        if let Ok(on_disk) = read_manifest(&cwd)
            && let Some(scripts) = on_disk.get("scripts")
        {
            package.insert("scripts".into(), scripts.clone());
        }
        let flag = |b: bool| if b { "1".to_string() } else { String::new() };
        dep_nodes.push(scripts::ScriptNode {
            index: i,
            location: e.path.clone(),
            cwd,
            package,
            extra_env: vec![
                ("npm_package_resolved".into(), e.resolved.clone().unwrap_or_default()),
                ("npm_package_integrity".into(), e.integrity.clone().unwrap_or_default()),
                ("npm_package_optional".into(), flag(e.optional)),
                ("npm_package_dev".into(), flag(e.dev)),
                ("npm_package_peer".into(), flag(e.peer)),
                ("npm_package_dev_optional".into(), flag(e.dev_optional)),
            ],
            optional: e.optional,
        });
    }
    let mut failed: Vec<usize> = Vec::new();
    let mut linker = BinLinker::new(dir, umask);
    let bins_of = |e: &Entry| -> Vec<(String, String)> {
        if e.link {
            match link_target(&lock, e).and_then(|t| read_manifest(&dir.join(&t.path)).ok()) {
                Some(pkg) => bins::normalize(pkg.get("name").and_then(Value::as_str), pkg.get("bin")),
                None => Vec::new(),
            }
        } else {
            bins::normalize(
                Some(e.raw.get("name").and_then(Value::as_str).unwrap_or(e.location_name())),
                e.raw.get("bin"),
            )
        }
    };
    if !opts.ignore_scripts {
        failed.extend(scripts::run_phase(&dep_nodes, "preinstall", &ctx)?);
    }
    for &i in &deps {
        let e = &lock.entries[i];
        linker
            .link(&e.path, &bins_of(e))
            .map_err(|err| format!("linking bins of {}: {err}", e.path))?;
    }
    if !opts.ignore_scripts {
        failed.extend(scripts::run_phase(&dep_nodes, "install", &ctx)?);
        failed.extend(scripts::run_phase(&dep_nodes, "postinstall", &ctx)?);
    }

    let mut link_nodes = Vec::new();
    for &i in &links {
        let e = &lock.entries[i];
        let target = link_target(&lock, e).expect("checked above");
        let cwd = dir.join(&target.path);
        let package = read_manifest(&cwd)?;
        link_nodes.push(scripts::ScriptNode {
            index: i,
            location: e.path.clone(),
            cwd,
            package,
            extra_env: Vec::new(),
            optional: e.optional,
        });
    }
    if !opts.ignore_scripts {
        failed.extend(scripts::run_phase(&link_nodes, "preinstall", &ctx)?);
    }
    // rebuild.js runs `prepare` for links even with --ignore-scripts.
    failed.extend(scripts::run_phase(&link_nodes, "prepare", &ctx)?);
    for &i in &links {
        let e = &lock.entries[i];
        linker
            .link(&e.path, &bins_of(e))
            .map_err(|err| format!("linking bins of {}: {err}", e.path))?;
    }
    if !opts.ignore_scripts {
        failed.extend(scripts::run_phase(&link_nodes, "install", &ctx)?);
        failed.extend(scripts::run_phase(&link_nodes, "postinstall", &ctx)?);
    }

    mark("bins+scripts");
    // A failed optional package is removed with everything only it needed.
    if !failed.is_empty() {
        let graph = Graph::build(&lock.entries);
        for i in failed {
            for j in graph.optional_set(i) {
                if plan.present.remove(&j) {
                    let path = dir.join(&lock.entries[j].path);
                    remove_path(&path).map_err(|err| format!("{}: {err}", path.display()))?;
                    for (name, _) in bins_of(&lock.entries[j]) {
                        if let Some(nm) = bins::node_modules_of(&lock.entries[j].path) {
                            let _ = fs::remove_file(dir.join(nm).join(".bin").join(name));
                        }
                    }
                }
            }
        }
        plan.install.retain(|i| plan.present.contains(i));
    }

    write_hidden_lockfile(dir, &lock, &plan).map_err(|e| format!("writing node_modules/.package-lock.json: {e}"))?;

    mark("hidden-lock");
    if !opts.ignore_scripts {
        if !plan.install.is_empty() {
            scripts::run_root(
                dir,
                &manifest,
                &["predependencies", "dependencies", "postdependencies"],
                &ctx,
            )?;
        }
        scripts::run_root(
            dir,
            &manifest,
            &[
                "preinstall",
                "install",
                "postinstall",
                "prepublish",
                "preprepare",
                "prepare",
                "postprepare",
            ],
            &ctx,
        )?;
    }

    if !opts.quiet {
        eprintln!(
            "lockstep: installed {} packages ({} downloaded) in {:.2}s",
            plan.install.len(),
            downloaded,
            started.elapsed().as_secs_f64()
        );
    }
    Ok(())
}

fn remove_path(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

fn relative_path(from_dir: &Path, to: &Path) -> PathBuf {
    let from: Vec<_> = from_dir.components().collect();
    let to_c: Vec<_> = to.components().collect();
    let common = from.iter().zip(to_c.iter()).take_while(|(a, b)| a == b).count();
    let mut out = PathBuf::new();
    for _ in common..from.len() {
        out.push("..");
    }
    for c in &to_c[common..] {
        out.push(c.as_os_str());
    }
    out
}

/// node_modules/.package-lock.json, as reify writes it: the lockfile's
/// top-level keys and a metaFromNode entry for every node in the tree
/// except the root, sorted by json-stringify-nice and formatted like the
/// root package.json.
fn write_hidden_lockfile(dir: &Path, lock: &Lockfile, plan: &Plan) -> Result<(), String> {
    let mut packages = Map::new();
    for (i, e) in lock.entries.iter().enumerate() {
        if !plan.present.contains(&i) {
            continue;
        }
        let meta = if e.link {
            let mut m = Map::new();
            if let Some(r) = &e.resolved {
                m.insert("resolved".into(), Value::String(r.clone()));
            }
            m.insert("link".into(), Value::Bool(true));
            m
        } else if !e.in_node_modules() {
            // A link target: arborist reads its package.json from disk.
            let pkg = read_manifest(&dir.join(&e.path))?;
            let mut pkg = pkg;
            normalize_bin(&mut pkg, e.path.rsplit('/').next().unwrap_or(&e.path));
            let scripts = pkg.get("scripts").and_then(Value::as_object);
            let has_install_script = e.has_install_script
                || scripts.is_some_and(|s| {
                    ["preinstall", "install", "postinstall"]
                        .iter()
                        .any(|k| s.contains_key(*k))
                });
            lockmeta::meta_from_node(&lockmeta::NodeInfo {
                package: &pkg,
                node_name: e.path.rsplit('/').next().unwrap_or(&e.path),
                is_top: true,
                integrity: None,
                resolved: None,
                in_bundle: false,
                has_shrinkwrap: false,
                has_install_script,
                extraneous: e.raw.get("extraneous").and_then(Value::as_bool).unwrap_or(false),
                dev: e.dev,
                optional: e.optional,
                dev_optional: e.dev_optional,
                peer: e.peer,
            })
        } else {
            let mut pkg = e.raw.clone();
            normalize_bin(&mut pkg, e.location_name());
            lockmeta::meta_from_node(&lockmeta::NodeInfo {
                package: &pkg,
                node_name: e.location_name(),
                is_top: false,
                integrity: e.integrity.as_deref(),
                resolved: e.resolved.as_deref(),
                in_bundle: e.in_bundle,
                has_shrinkwrap: e.has_shrinkwrap,
                has_install_script: e.has_install_script,
                extraneous: e.raw.get("extraneous").and_then(Value::as_bool).unwrap_or(false),
                dev: e.dev,
                optional: e.optional,
                dev_optional: e.dev_optional,
                peer: e.peer,
            })
        };
        packages.insert(e.path.clone(), Value::Object(meta));
    }
    let mut out = Map::new();
    for (key, value) in &lock.raw {
        if key != "packages" && key != "dependencies" {
            out.insert(key.clone(), value.clone());
        }
    }
    out.insert("lockfileVersion".into(), Value::from(3));
    out.insert("requires".into(), Value::Bool(true));
    out.insert("packages".into(), Value::Object(packages));
    let nm = dir.join("node_modules");
    if !nm.is_dir() {
        return Ok(());
    }
    let style = fs::read_to_string(dir.join("package.json"))
        .map(|t| json::Style::detect(&t))
        .unwrap_or_default();
    fs::write(
        nm.join(".package-lock.json"),
        lockmeta::lockfile_text(&Value::Object(out), &style),
    )
    .map_err(|e| e.to_string())
}

/// npm-normalize-package-bin mutates the manifest it links bins from, so
/// the normalised form is what lands in the hidden lockfile.
fn normalize_bin(pkg: &mut Map<String, Value>, folder_name: &str) {
    if !pkg.contains_key("bin") {
        return;
    }
    let name = pkg
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(folder_name)
        .to_string();
    let bins = bins::normalize(Some(&name), pkg.get("bin"));
    if bins.is_empty() {
        pkg.remove("bin");
    } else {
        let map: Map<String, Value> = bins.into_iter().map(|(k, v)| (k, Value::String(v))).collect();
        pkg.insert("bin".into(), Value::Object(map));
    }
}
