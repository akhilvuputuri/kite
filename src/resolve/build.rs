//! arborist build-ideal-tree.js: from package.json (and the lockfile, when
//! there is one) to the ideal tree.

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value};

use super::flags;
use super::npa::{self, Spec};
use super::place::{self, PlaceOptions, Placement};
use super::registry::{PickSpec, Registry, normalize_manifest};
use super::tree::{EdgeId, EdgeType, Id, NewNode, Tree};
use crate::collate;
use crate::graph::Graph;
use crate::lockfile::Lockfile;
use crate::platform::Platform;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveType {
    Prod,
    Dev,
    Optional,
    Peer,
}

impl SaveType {
    pub fn field(self) -> &'static str {
        match self {
            SaveType::Prod => "dependencies",
            SaveType::Dev => "devDependencies",
            SaveType::Optional => "optionalDependencies",
            SaveType::Peer => "peerDependencies",
        }
    }
}

#[derive(Clone, Debug)]
pub struct AddRequest {
    pub name: String,
    pub raw_spec: String,
}

#[derive(Default)]
pub struct Requests {
    pub add: Vec<AddRequest>,
    pub save_type: Option<SaveType>,
    pub rm: Vec<String>,
    pub update_all: bool,
    pub update_names: Vec<String>,
}

pub struct Builder<'r> {
    pub t: Tree,
    reg: &'r Registry,
    queue: Vec<Id>,
    seen: HashSet<Id>,
    explicit: HashSet<EdgeId>,
    update_names: Vec<String>,
    load_failures: HashSet<Id>,
    link_nodes: Vec<Id>,
    virtual_roots: HashMap<Id, Id>,
    peer_set_source: HashMap<Id, Id>,
    manifests: HashMap<String, Result<Map<String, Value>, String>>,
    mutate_tree: bool,
    meta_from_disk: bool,
    flags_suspect: bool,
    /// Root dependency names added by the user, for saving package.json.
    pub added: Vec<(String, SaveType)>,
}

/// read-package-json-fast normalisation for a root or link-target manifest.
pub fn normalize_root_package(mut pkg: Map<String, Value>) -> Map<String, Value> {
    if let Some(b) = pkg.remove("bundledDependencies") {
        pkg.entry("bundleDependencies").or_insert(b);
    }
    let optional: Vec<String> = pkg
        .get("optionalDependencies")
        .and_then(Value::as_object)
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    if let Some(Value::Object(deps)) = pkg.get_mut("dependencies") {
        for n in &optional {
            deps.remove(n);
        }
    }
    if let Some(Value::Object(scripts)) = pkg.get_mut("scripts") {
        scripts.retain(|_, v| v.is_string());
    }
    if let Some(Value::String(url)) = pkg.get("funding").cloned() {
        let mut f = Map::new();
        f.insert("url".into(), Value::String(url));
        pkg.insert("funding".into(), Value::Object(f));
    }
    if pkg.contains_key("bin") {
        let name = pkg.get("name").and_then(Value::as_str).map(str::to_string);
        let bins = crate::bins::normalize(name.as_deref(), pkg.get("bin"));
        if bins.is_empty() {
            pkg.remove("bin");
        } else {
            pkg.insert(
                "bin".into(),
                Value::Object(bins.into_iter().map(|(k, v)| (k, Value::String(v))).collect()),
            );
        }
    }
    pkg
}

fn read_package(dir: &str) -> Map<String, Value> {
    std::fs::read_to_string(format!("{dir}/package.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.as_object().cloned())
        .map(normalize_root_package)
        .unwrap_or_default()
}

fn location_name(loc: &str) -> String {
    crate::lockfile::location_name(loc).to_string()
}

impl<'r> Builder<'r> {
    /// initTree: the root from package.json, plus the lockfile tree unless
    /// there is none or every dependency is being updated.
    pub fn new(
        project: &str,
        root_pkg: Map<String, Value>,
        lock: Option<&Lockfile>,
        reg: &'r Registry,
        update_all: bool,
    ) -> Result<Self, String> {
        let mut t = Tree::new(project, root_pkg);
        let root = t.root;
        {
            let r = &mut t.nodes[root];
            r.dev = false;
            r.optional = false;
            r.dev_optional = false;
            r.peer = false;
            r.extraneous = false;
        }
        let mut b = Builder {
            t,
            reg,
            queue: Vec::new(),
            seen: HashSet::new(),
            explicit: HashSet::new(),
            update_names: Vec::new(),
            load_failures: HashSet::new(),
            link_nodes: Vec::new(),
            virtual_roots: HashMap::new(),
            peer_set_source: HashMap::new(),
            manifests: HashMap::new(),
            mutate_tree: false,
            meta_from_disk: lock.is_some(),
            flags_suspect: false,
            added: Vec::new(),
        };
        if let (Some(lock), false) = (lock, update_all) {
            b.load_virtual(project, lock)?;
        }
        Ok(b)
    }

    fn load_virtual(&mut self, project: &str, lock: &Lockfile) -> Result<(), String> {
        let mut by_loc: HashMap<String, Id> = HashMap::new();
        by_loc.insert(String::new(), self.t.root);
        let mut entries: Vec<&crate::lockfile::Entry> = lock.entries.iter().filter(|e| !e.is_root()).collect();
        entries.sort_by_key(|e| e.path.matches('/').count());
        let mut links: Vec<&crate::lockfile::Entry> = Vec::new();
        for e in &entries {
            if e.link {
                links.push(e);
                continue;
            }
            if e.in_bundle || e.has_shrinkwrap {
                return Err(format!(
                    "{}: bundled dependencies and shrinkwraps are not supported",
                    e.path
                ));
            }
            let path = format!("{project}/{}", e.path);
            let in_nm = e.in_node_modules();
            let package = if in_nm {
                let mut p = e.raw.clone();
                p.entry("name").or_insert_with(|| Value::String(location_name(&e.path)));
                p
            } else {
                let mut p = read_package(&path);
                if p.is_empty() {
                    p = e.raw.clone();
                }
                p
            };
            let name = if in_nm {
                location_name(&e.path)
            } else {
                e.path.rsplit('/').next().unwrap_or(&e.path).to_string()
            };
            let id = self.t.add_node(NewNode {
                name,
                path,
                package,
                resolved: e.resolved.clone(),
                integrity: e.integrity.clone(),
                load_dev: !in_nm,
                errors: Vec::new(),
            });
            by_loc.insert(e.path.clone(), id);
            if in_nm {
                let parent_loc = crate::graph::parent_location(&e.path).unwrap_or("");
                let parent = *by_loc
                    .get(parent_loc)
                    .ok_or_else(|| format!("{}: parent {parent_loc:?} is not in the lockfile", e.path))?;
                self.t.set_parent(id, parent);
            } else {
                self.t.set_fs_parent(id, self.t.root);
            }
        }
        for e in links {
            let target_loc = e.resolved.clone().unwrap_or_default();
            let target = match by_loc.get(&target_loc) {
                Some(&t) => t,
                None => {
                    // A link to a folder outside the lockfile's packages.
                    let path = npa::resolve_path(project, &target_loc);
                    self.t.add_node(NewNode {
                        name: target_loc.rsplit('/').next().unwrap_or("").to_string(),
                        path: path.clone(),
                        package: read_package(&path),
                        resolved: None,
                        integrity: None,
                        load_dev: true,
                        errors: Vec::new(),
                    })
                }
            };
            let path = format!("{project}/{}", e.path);
            let link = self.t.add_link(&location_name(&e.path), &path, target);
            let parent_loc = crate::graph::parent_location(&e.path).unwrap_or("");
            let parent = *by_loc.get(parent_loc).unwrap_or(&self.t.root);
            self.t.set_parent(link, parent);
            by_loc.insert(e.path.clone(), link);
        }
        // loadVirtual with a root: flags are always recomputed.
        self.flags_suspect = true;
        let all: Vec<Id> = (0..self.t.nodes.len()).collect();
        flags::reset_all(&mut self.t, &all);
        flags::calc_dep_flags(&mut self.t, false);
        Ok(())
    }

    /// Seeds the queue with every node reachable from the root that has an
    /// invalid edge (treeverse depth order).
    fn seed_queue(&mut self) {
        self.t.refresh();
        let mut stack = vec![self.t.root];
        let mut seen = HashSet::new();
        while let Some(n) = stack.pop() {
            if !seen.insert(n) {
                continue;
            }
            let edges = self.t.edges_out(n);
            if edges.iter().any(|&e| !self.t.edge_valid(e)) {
                self.push(n);
            }
            for e in edges {
                if let Some(to) = self.t.edge_to(e) {
                    stack.push(to);
                }
            }
        }
    }

    /// applyUserRequests for the root.
    pub fn apply_requests(&mut self, req: &Requests) -> Result<(), String> {
        let root = self.t.root;
        if req.update_all || !req.add.is_empty() || !req.rm.is_empty() || !req.update_names.is_empty() {
            self.mutate_tree = true;
        }
        self.update_names = req.update_names.clone();
        self.seed_queue();
        if !req.update_names.is_empty() {
            // queueNamedUpdates: re-check every dependent of a named package.
            let inv = self.t.inventory();
            for n in inv {
                if !self.t.is_top(n) && req.update_names.contains(&self.t.nodes[n].name) {
                    for e in self.t.edges_in(n) {
                        let from = self.t.edges[e].from;
                        self.push(from);
                    }
                }
            }
        }
        let mut pkg = self.t.nodes[root].package.clone();
        for name in &req.rm {
            for field in [
                "dependencies",
                "devDependencies",
                "optionalDependencies",
                "peerDependencies",
            ] {
                if let Some(Value::Object(m)) = pkg.get_mut(field) {
                    m.remove(name);
                    if m.is_empty() {
                        pkg.remove(field);
                    }
                }
            }
            if let Some(Value::Object(meta)) = pkg.get_mut("peerDependenciesMeta") {
                meta.remove(name);
            }
            if let Some(Value::Array(b)) = pkg.get_mut("bundleDependencies") {
                b.retain(|v| v.as_str() != Some(name));
            }
        }
        for add in &req.add {
            npa::parse(&add.name, &add.raw_spec)?;
            let section = req.save_type.unwrap_or_else(|| infer_save_type(&pkg, &add.name));
            // Conflicting sections lose the name.
            let remove_from: &[&str] = match section {
                SaveType::Prod => &["devDependencies", "peerDependencies"],
                SaveType::Dev => &["dependencies"],
                SaveType::Optional => &["peerDependencies"],
                SaveType::Peer => &["dependencies", "optionalDependencies"],
            };
            for field in remove_from {
                if let Some(Value::Object(m)) = pkg.get_mut(*field) {
                    m.remove(&add.name);
                    if m.is_empty() {
                        pkg.remove(*field);
                    }
                }
            }
            let field = section.field();
            let existing = pkg.get(field).and_then(|m| m.get(&add.name)).is_some();
            if !(add.raw_spec == "*" && existing) {
                let entry = pkg.entry(field).or_insert_with(|| Value::Object(Map::new()));
                if let Value::Object(m) = entry {
                    m.insert(add.name.clone(), Value::String(add.raw_spec.clone()));
                }
            }
            self.added.push((add.name.clone(), section));
        }
        if !req.add.is_empty() || !req.rm.is_empty() {
            self.t.nodes[root].package = pkg;
            self.t.load_deps(root, true);
            self.t.refresh();
            for (name, _) in &self.added {
                if let Some(e) = self.t.edge_out(root, name) {
                    self.explicit.insert(e);
                }
            }
        }
        self.push(root);
        Ok(())
    }

    fn push(&mut self, n: Id) {
        if !self.queue.contains(&n) {
            self.queue.push(n);
        }
    }

    fn pop(&mut self) -> Option<Id> {
        if self.queue.is_empty() {
            return None;
        }
        let t = &self.t;
        let mut keyed: Vec<(usize, String, Id)> = self
            .queue
            .iter()
            .map(|&n| (t.depth(n), t.nodes[n].path.clone(), n))
            .collect();
        keyed.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| collate::compare(&a.1, &b.1)));
        let first = keyed[0].2;
        self.queue = keyed.into_iter().skip(1).map(|(_, _, n)| n).collect();
        Some(first)
    }

    fn problem_edges(&mut self, node: Id) -> Vec<EdgeId> {
        let bundled: Vec<String> = if self.t.nodes[node].is_project_root {
            Vec::new()
        } else {
            self.t.nodes[node]
                .package
                .get("bundleDependencies")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                .unwrap_or_default()
        };
        let mut out = Vec::new();
        for e in self.t.edges_out(node) {
            let name = self.t.edges[e].name.clone();
            if bundled.contains(&name) {
                continue;
            }
            let to = self.t.edge_to(e);
            if let Some(to) = to
                && self.load_failures.contains(&to)
            {
                continue;
            }
            match to {
                None => {
                    if self.t.edges[e].ty != EdgeType::PeerOptional || self.explicit.contains(&e) {
                        out.push(e);
                    }
                }
                Some(_) => {
                    if !self.t.edge_valid(e) || self.update_names.contains(&name) || self.explicit.contains(&e) {
                        out.push(e);
                    }
                }
            }
        }
        out
    }

    fn virtual_root(&mut self, source: Id, reuse: bool) -> Id {
        if reuse && let Some(&vr) = self.virtual_roots.get(&source) {
            return vr;
        }
        let src = self.t.nodes[source].clone();
        let load_dev = self.t.is_top(source);
        let vr = self.t.add_node(NewNode {
            name: src.name.clone(),
            path: src.realpath.clone(),
            package: src.package.clone(),
            resolved: src.resolved.clone(),
            integrity: src.integrity.clone(),
            load_dev,
            errors: Vec::new(),
        });
        self.t.nodes[vr].source_reference = Some(source);
        for child in self.t.children(source) {
            if self.t.nodes[child].is_link
                && let Some(target) = self.t.nodes[child].target
            {
                let tn = self.t.nodes[target].clone();
                let copy = self.t.add_node(NewNode {
                    name: tn.name.clone(),
                    path: tn.realpath.clone(),
                    package: tn.package.clone(),
                    resolved: None,
                    integrity: None,
                    load_dev: true,
                    errors: Vec::new(),
                });
                self.t.nodes[copy].source_reference = Some(target);
                if tn.realpath.starts_with(&format!("{}/", src.realpath)) {
                    self.t.set_fs_parent(copy, vr);
                }
            }
        }
        self.virtual_roots.insert(source, vr);
        vr
    }

    fn fetch_manifest(&mut self, name: &str, spec: &Spec) -> Result<Map<String, Value>, String> {
        let key = format!("{name}@{spec:?}");
        if let Some(r) = self.manifests.get(&key) {
            return r.clone();
        }
        let (real, pick) = match spec {
            Spec::Range { range } => (name.to_string(), PickSpec::Range(range.clone())),
            Spec::Version { version } => (name.to_string(), PickSpec::Version(version.clone())),
            Spec::Tag { tag } => (name.to_string(), PickSpec::Tag(tag.clone())),
            Spec::Alias { name: real, spec } => match spec.as_ref() {
                Spec::Range { range } => (real.clone(), PickSpec::Range(range.clone())),
                Spec::Version { version } => (real.clone(), PickSpec::Version(version.clone())),
                Spec::Tag { tag } => (real.clone(), PickSpec::Tag(tag.clone())),
                _ => return Err(format!("{name}: unsupported alias")),
            },
            Spec::Directory { .. } => unreachable!("directories are linked"),
        };
        let result = self
            .reg
            .packument(&real)
            .and_then(|doc| self.reg.pick(&doc, &pick))
            .map(normalize_manifest);
        if let Ok(m) = &result {
            if m.get("_hasShrinkwrap").and_then(Value::as_bool) == Some(true) {
                return Err(format!(
                    "{real}@{}: packages that ship npm-shrinkwrap.json are not supported",
                    m.get("version").and_then(Value::as_str).unwrap_or("?")
                ));
            }
            let bundles = m
                .get("bundleDependencies")
                .and_then(Value::as_array)
                .is_some_and(|a| !a.is_empty());
            if bundles {
                return Err(format!(
                    "{real}@{}: packages with bundleDependencies are not supported",
                    m.get("version").and_then(Value::as_str).unwrap_or("?")
                ));
            }
            // Warm the cache for what this package will need next.
            let mut next: Vec<String> = Vec::new();
            for field in ["dependencies", "optionalDependencies", "peerDependencies"] {
                if let Some(d) = m.get(field).and_then(Value::as_object) {
                    for (dep, s) in d {
                        let real = match npa::parse(dep, s.as_str().unwrap_or("")) {
                            Ok(Spec::Alias { name, .. }) => name,
                            Ok(Spec::Directory { .. }) | Err(_) => continue,
                            Ok(_) => dep.clone(),
                        };
                        next.push(real);
                    }
                }
            }
            self.reg.prefetch(next);
        }
        self.manifests.insert(key, result.clone());
        result
    }

    fn node_from_spec(&mut self, name: &str, spec_text: &str, base: &str, parent: Id) -> Result<Id, String> {
        npa::check_name(name)?;
        let spec = npa::parse(name, spec_text)?;
        if let Spec::Directory { path } = &spec {
            let real = npa::resolve_path(base, path);
            let package = read_package(&real);
            let target = self.t.add_node(NewNode {
                name: real.rsplit('/').next().unwrap_or("").to_string(),
                path: real.clone(),
                package,
                resolved: None,
                integrity: None,
                load_dev: true,
                errors: Vec::new(),
            });
            let link_path = super::tree::join(
                &super::tree::join(&self.t.nodes[parent].path.clone(), "node_modules"),
                name,
            );
            let link = self.t.add_link(name, &link_path, target);
            self.t.set_parent(link, parent);
            let vr_root = self.t.root_of(parent);
            if real.starts_with(&format!("{}/", self.t.nodes[vr_root].path)) {
                self.t.set_fs_parent(target, vr_root);
            }
            self.link_nodes.push(link);
            return Ok(link);
        }
        let path = super::tree::join(
            &super::tree::join(&self.t.nodes[parent].path.clone(), "node_modules"),
            name,
        );
        let id = match self.fetch_manifest(name, &spec) {
            Ok(m) => {
                let resolved = m.get("_resolved").and_then(Value::as_str).map(str::to_string);
                let integrity = m.get("_integrity").and_then(Value::as_str).map(str::to_string);
                self.t.add_node(NewNode {
                    name: name.to_string(),
                    path,
                    package: m,
                    resolved,
                    integrity,
                    load_dev: false,
                    errors: Vec::new(),
                })
            }
            Err(error) => {
                let id = self.t.add_node(NewNode {
                    name: name.to_string(),
                    path,
                    package: Map::new(),
                    resolved: None,
                    integrity: None,
                    load_dev: false,
                    errors: vec![error],
                });
                self.load_failures.insert(id);
                id
            }
        };
        self.t.set_parent(id, parent);
        Ok(id)
    }

    fn node_from_edge(
        &mut self,
        edge: EdgeId,
        parent: Option<Id>,
        second: Option<EdgeId>,
        required: &mut HashSet<Id>,
    ) -> Result<Id, String> {
        let from = self.t.edges[edge].from;
        let parent = match parent {
            Some(p) => p,
            None => self.virtual_root(from, false),
        };
        let name = self.t.edges[edge].name.clone();
        let spec = self.t.edges[edge].spec.clone();
        let base = self.t.nodes[self.t.target(from)].realpath.clone();
        let first = self.node_from_spec(&name, &spec, &base, parent)?;
        let second_node = match second {
            Some(se) if !self.t.edge_valid(se) => {
                let sfrom = self.t.edges[se].from;
                let sbase = self.t.nodes[self.t.target(sfrom)].realpath.clone();
                let sspec = self.t.edges[se].spec.clone();
                Some(self.node_from_spec(&name, &sspec, &sbase, parent)?)
            }
            _ => None,
        };
        let node = match second_node {
            Some(s) if self.t.edge_valid(edge) => s,
            _ => first,
        };
        if self.t.nodes[node].parent != Some(parent) || self.t.child(parent, &name) != Some(node) {
            self.t.set_parent(node, parent);
        }
        let req = (required.contains(&from) && self.t.edges[edge].ty != EdgeType::PeerOptional)
            || second.is_some_and(|se| {
                required.contains(&self.t.edges[se].from) && self.t.edges[se].ty != EdgeType::PeerOptional
            });
        if req {
            required.insert(node);
        }
        if let Some(src) = self.t.nodes[parent].source_reference {
            self.peer_set_source.insert(node, src);
        }
        self.load_peer_set(node, required)
    }

    fn load_peer_set(&mut self, node: Id, required: &mut HashSet<Id>) -> Result<Id, String> {
        let mut peer_edges: Vec<EdgeId> = self
            .t
            .edges_out(node)
            .into_iter()
            .filter(|&e| self.t.edges[e].ty.peer())
            .collect();
        peer_edges.retain(|&e| !(self.t.edge_valid(e) && self.t.edge_to(e).is_some()));
        peer_edges.sort_by(|&a, &b| collate::compare(&self.t.edges[a].name, &self.t.edges[b].name));
        for edge in peer_edges {
            if self.t.edge_valid(edge) && self.t.edge_to(edge).is_some() {
                continue;
            }
            let parent = self.t.nodes[node]
                .parent
                .expect("peer set nodes live in a virtual root");
            let name = self.t.edges[edge].name.clone();
            let parent_edge = self.t.edge_out(parent, &name);
            let is_mine = self.t.nodes[parent]
                .source_reference
                .is_some_and(|s| self.t.nodes[s].is_project_root);
            let conflict_ok = !is_mine;
            match self.t.edge_to(edge) {
                None => match parent_edge {
                    None => {
                        self.node_from_edge(edge, Some(parent), None, required)?;
                        continue;
                    }
                    Some(pe) => {
                        let dep = self.node_from_edge(pe, Some(parent), Some(edge), required)?;
                        if self.t.edge_valid(edge) {
                            continue;
                        }
                        if conflict_ok || !required.contains(&dep) {
                            self.t.edges[edge].peer_conflicted = true;
                            continue;
                        }
                        return Err(self.peer_conflict(edge));
                    }
                },
                Some(current) => {
                    let dep = self.node_from_edge(edge, None, None, required)?;
                    if self.t.can_replace_with(current, dep, &[]) {
                        self.node_from_edge(edge, Some(parent), None, required)?;
                        continue;
                    }
                    let from = self.t.edges[edge].from;
                    if conflict_ok || !required.contains(&from) {
                        continue;
                    }
                    return Err(self.peer_conflict(edge));
                }
            }
        }
        Ok(node)
    }

    fn peer_conflict(&self, edge: EdgeId) -> String {
        let e = &self.t.edges[edge];
        format!(
            "ERESOLVE: unable to resolve dependency tree: {} needs peer {}@{}",
            self.t.package_name(e.from).unwrap_or(&self.t.nodes[e.from].name),
            e.name,
            e.spec
        )
    }

    /// The queue loop (buildDepStep), until the tree is complete.
    pub fn build_deps(&mut self) -> Result<(), String> {
        // Warm the cache only for edges that will be resolved; an in-sync
        // lockfile needs no registry metadata at all.
        let root = self.t.root;
        let root_names: Vec<String> = self
            .problem_edges(root)
            .iter()
            .filter_map(|&e| {
                let name = self.t.edges[e].name.clone();
                match npa::parse(&name, &self.t.edges[e].spec) {
                    Ok(Spec::Alias { name, .. }) => Some(name),
                    Ok(Spec::Directory { .. }) | Err(_) => None,
                    Ok(_) => Some(name),
                }
            })
            .collect();
        self.reg.prefetch(root_names);
        loop {
            let Some(node) = self.pop() else {
                if self.resolve_links() {
                    continue;
                }
                break;
            };
            if self.seen.contains(&node) || !self.t.in_main_tree(node) {
                continue;
            }
            self.seen.insert(node);
            let peer_source = self.peer_set_source.get(&node).copied().unwrap_or(node);
            let mut tasks: Vec<(EdgeId, Id)> = Vec::new();
            for edge in self.problem_edges(node) {
                if self.t.edges[edge].peer_conflicted {
                    continue;
                }
                let is_peer = self.t.edges[edge].ty.peer();
                let source = if is_peer { peer_source } else { node };
                let vr = self.virtual_root(source, true);
                let name = self.t.edges[edge].name.clone();
                let vr_dep = self.t.edge_out(vr, &name).and_then(|ve| {
                    if self.t.edge_valid(ve) {
                        self.t.edge_to(ve)
                    } else {
                        None
                    }
                });
                let mut required: HashSet<Id> = HashSet::from([self.t.edges[edge].from]);
                let dep = match vr_dep {
                    Some(d) if self.t.node_satisfies(d, edge) => d,
                    _ => self.node_from_edge(edge, if is_peer { Some(vr) } else { None }, None, &mut required)?,
                };
                tasks.push((edge, dep));
            }
            tasks.sort_by(|a, b| collate::compare(&self.t.edges[a.0].name, &self.t.edges[b.0].name));
            for (edge, dep) in tasks {
                let opts = PlaceOptions {
                    prefer_dedupe: false,
                    update_names: &self.update_names.clone(),
                    force: false,
                    strict_peer_deps: false,
                };
                let explicit = self.explicit.contains(&edge);
                let pd = place::place_dep(&mut self.t, &opts, edge, dep, explicit)?;
                self.visit_placed(&pd)?;
            }
            for e in self.t.edges_out(node) {
                if let Some(to) = self.t.edge_to(e)
                    && self.t.nodes[to].is_link
                    && self.t.nodes[to].target.is_some()
                    && !self.link_nodes.contains(&to)
                {
                    self.link_nodes.push(to);
                }
            }
        }
        Ok(())
    }

    /// The visit over a PlaceDep and its peers, in treeverse order (a stack,
    /// so later peers are visited first).
    fn visit_placed(&mut self, top: &place::Pd) -> Result<(), String> {
        let mut stack: Vec<&place::Pd> = vec![top];
        while let Some(pd) = stack.pop() {
            self.visit_one(pd);
            for child in &pd.children {
                stack.push(child);
            }
        }
        Ok(())
    }

    fn visit_one(&mut self, pd: &place::Pd) {
        let Some(placed) = pd.placed else { return };
        if !self.t.nodes[placed].errors.is_empty() {
            self.load_failures.insert(placed);
        }
        self.mutate_tree = true;
        match pd.can_place_self {
            Some(Placement::Ok) => {
                for e in self.t.edges_in(placed) {
                    if e == pd.edge {
                        continue;
                    }
                    let from = self.t.edges[e].from;
                    if !self.t.edges[e].peer_conflicted && !self.t.edge_valid(e) && !self.seen.contains(&from) {
                        self.push(from);
                    }
                }
            }
            Some(Placement::Replace) => {
                for e in self.t.edges_in(placed) {
                    if e == pd.edge {
                        continue;
                    }
                    let from = self.t.edges[e].from;
                    if !self.t.edge_valid(e) && !self.t.edges[e].peer_conflicted {
                        self.seen.remove(&from);
                        self.push(from);
                    }
                }
            }
            _ => {}
        }
        if pd.can_place_self == Some(Placement::Conflict) {
            return;
        }
        self.push(placed);
        for &n in &pd.need_evaluation {
            self.seen.remove(&n);
            self.push(n);
        }
    }

    /// resolveLinks: queue link targets inside the project that have not
    /// been processed. Returns whether anything was queued.
    fn resolve_links(&mut self) -> bool {
        let links = std::mem::take(&mut self.link_nodes);
        for link in links {
            if !self.t.in_main_tree(link) {
                continue;
            }
            let Some(target) = self.t.nodes[link].target else {
                continue;
            };
            place::adopt_link_target(&mut self.t, link);
            let root = self.t.root;
            if !self.t.is_descendant_of(target, root) {
                continue;
            }
            if self.t.resolve_parent(target).is_some() && !self.seen.contains(&target) {
                self.push(target);
            }
        }
        !self.queue.is_empty()
    }

    /// fixDepFlags, pruneFailedOptional and checkEngineAndPlatform.
    pub fn finish(&mut self) -> Result<(), String> {
        let inventory = self.t.inventory();
        if self.meta_from_disk && self.mutate_tree {
            flags::reset_all(&mut self.t, &inventory);
        }
        if !self.meta_from_disk || self.mutate_tree {
            flags::calc_dep_flags(&mut self.t, true);
        } else {
            let root = self.t.root;
            let r = &mut self.t.nodes[root];
            r.extraneous = false;
            r.dev = false;
            r.optional = false;
            r.dev_optional = false;
            r.peer = false;
        }
        if self.meta_from_disk && (self.mutate_tree || self.flags_suspect) {
            for n in self.t.inventory() {
                // `node.parent = null`: link targets (fsChildren) stay.
                if self.t.nodes[n].extraneous && n != self.t.root && self.t.nodes[n].parent.is_some() {
                    self.t.detach(n);
                }
            }
        }
        // pruneFailedOptional
        let failures: Vec<Id> = self.load_failures.iter().copied().collect();
        for n in failures {
            if !self.t.in_main_tree(n) {
                continue;
            }
            if !self.t.nodes[n].optional {
                return Err(self.t.nodes[n]
                    .errors
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "resolution failed".into()));
            }
            for m in optional_set(&mut self.t, n) {
                self.t.detach(m);
            }
        }
        // checkEngineAndPlatform: a non-optional platform mismatch is fatal.
        let platform = Platform::current();
        for n in self.t.inventory() {
            let node = &self.t.nodes[n];
            if node.optional || node.is_link {
                continue;
            }
            let list = |k: &str| -> Vec<String> {
                match node.package.get(k) {
                    Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
                    Some(Value::String(s)) => vec![s.clone()],
                    _ => Vec::new(),
                }
            };
            if !platform.accepts(&list("os"), &list("cpu"), &list("libc")) {
                return Err(format!(
                    "EBADPLATFORM: {}@{} is not supported on {} {}",
                    self.t.package_name(n).unwrap_or(&node.name),
                    self.t.version(n).unwrap_or("?"),
                    platform.os,
                    platform.cpu
                ));
            }
        }
        Ok(())
    }
}

/// arborist optionalSet on the logical tree.
fn optional_set(t: &mut Tree, node: Id) -> Vec<Id> {
    if !t.nodes[node].optional {
        return Vec::new();
    }
    let mut set = vec![node];
    let mut i = 0;
    while i < set.len() {
        let n = set[i];
        for e in t.edges_in(n) {
            let from = t.edges[e].from;
            if !t.edges[e].ty.optional() && !set.contains(&from) {
                set.push(from);
            }
        }
        i += 1;
    }
    t.gather_dep_set(&set, &|tr: &Tree, e| !tr.edges[e].ty.optional())
}

/// add-rm-pkg-deps inferSaveType: the first section that already has the
/// name, in the order dev, optional, prod, peerOptional, peer.
fn infer_save_type(pkg: &Map<String, Value>, name: &str) -> SaveType {
    let has = |f: &str| pkg.get(f).and_then(|m| m.get(name)).is_some();
    if has("devDependencies") {
        SaveType::Dev
    } else if has("optionalDependencies") {
        SaveType::Optional
    } else if has("dependencies") {
        SaveType::Prod
    } else if has("peerDependencies") {
        SaveType::Peer
    } else {
        SaveType::Prod
    }
}

// Keep the lockfile graph type reachable for callers that verify results.
pub type _Graph = Graph;
