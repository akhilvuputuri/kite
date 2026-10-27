//! An arena port of arborist's Node, Link and Edge: the logical
//! `node_modules` tree that the resolver builds and mutates.
//!
//! Edge targets (`edge.to`) and `edgesIn` are derived data. Arborist keeps
//! them current with targeted reloads; here every mutation marks the
//! affected trees dirty and the next read recomputes them for that tree.

use std::collections::HashSet;

use serde_json::{Map, Value};

use super::npa::{self, Spec};
use crate::semver::{Range, Version};

pub type Id = usize;
pub type EdgeId = usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeType {
    Prod,
    Dev,
    Optional,
    Peer,
    PeerOptional,
}

impl EdgeType {
    pub fn optional(self) -> bool {
        matches!(self, EdgeType::Optional | EdgeType::PeerOptional)
    }
    pub fn peer(self) -> bool {
        matches!(self, EdgeType::Peer | EdgeType::PeerOptional)
    }
    pub fn dev(self) -> bool {
        self == EdgeType::Dev
    }
}

#[derive(Clone, Debug)]
pub struct Edge {
    pub from: Id,
    pub ty: EdgeType,
    pub name: String,
    pub spec: String,
    pub to: Option<Id>,
    pub peer_conflicted: bool,
    pub detached: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Node {
    pub name: String,
    pub path: String,
    pub realpath: String,
    pub package: Map<String, Value>,
    pub resolved: Option<String>,
    pub integrity: Option<String>,
    pub parent: Option<Id>,
    pub fs_parent: Option<Id>,
    /// Insertion-ordered children keyed by lowercased name.
    pub children: Vec<(String, Id)>,
    pub fs_children: Vec<Id>,
    pub is_link: bool,
    pub target: Option<Id>,
    pub edges_out: Vec<EdgeId>,
    pub edges_in: Vec<EdgeId>,
    pub source_reference: Option<Id>,
    pub is_project_root: bool,
    pub dev: bool,
    pub optional: bool,
    pub dev_optional: bool,
    pub peer: bool,
    pub extraneous: bool,
    pub errors: Vec<String>,
    pub in_bundle: bool,
    pub has_shrinkwrap: bool,
}

pub struct Tree {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub root: Id,
    dirty: HashSet<Id>,
    all_dirty: bool,
}

fn key(name: &str) -> String {
    name.to_lowercase()
}

pub fn join(base: &str, rel: &str) -> String {
    if base.is_empty() {
        rel.to_string()
    } else if base == "/" {
        format!("/{rel}")
    } else {
        format!("{base}/{rel}")
    }
}

/// `path.relative(from, to)` for normalised absolute paths.
pub fn relpath(from: &str, to: &str) -> String {
    let f: Vec<&str> = from.split('/').filter(|s| !s.is_empty()).collect();
    let t: Vec<&str> = to.split('/').filter(|s| !s.is_empty()).collect();
    let common = f.iter().zip(&t).take_while(|(a, b)| a == b).count();
    let mut out: Vec<&str> = vec![".."; f.len() - common];
    out.extend_from_slice(&t[common..]);
    out.join("/")
}

pub struct NewNode {
    pub name: String,
    pub path: String,
    pub package: Map<String, Value>,
    pub resolved: Option<String>,
    pub integrity: Option<String>,
    /// Arborist loads dev edges only for nodes created as a top with a path:
    /// the root, link targets and virtual roots.
    pub load_dev: bool,
    pub errors: Vec<String>,
}

impl Tree {
    pub fn new(root_path: &str, package: Map<String, Value>) -> Tree {
        let mut tree = Tree {
            nodes: Vec::new(),
            edges: Vec::new(),
            root: 0,
            dirty: HashSet::new(),
            all_dirty: true,
        };
        let name = root_path.rsplit('/').next().unwrap_or("").to_string();
        let root = tree.add_node(NewNode {
            name,
            path: root_path.to_string(),
            package,
            resolved: None,
            integrity: None,
            load_dev: true,
            errors: Vec::new(),
        });
        tree.nodes[root].is_project_root = true;
        tree.root = root;
        tree
    }

    // ----- construction -------------------------------------------------

    pub fn add_node(&mut self, n: NewNode) -> Id {
        let id = self.nodes.len();
        self.nodes.push(Node {
            name: n.name,
            realpath: n.path.clone(),
            path: n.path,
            package: n.package,
            resolved: n.resolved,
            integrity: n.integrity,
            errors: n.errors,
            dev: true,
            optional: true,
            dev_optional: true,
            peer: true,
            extraneous: true,
            ..Default::default()
        });
        self.load_deps(id, n.load_dev);
        self.all_dirty = true;
        id
    }

    /// A Link node at `path` pointing to `target`.
    pub fn add_link(&mut self, name: &str, path: &str, target: Id) -> Id {
        let id = self.nodes.len();
        let package = self.nodes[target].package.clone();
        let realpath = self.nodes[target].realpath.clone();
        self.nodes.push(Node {
            name: name.to_string(),
            path: path.to_string(),
            realpath,
            package,
            is_link: true,
            target: Some(target),
            dev: true,
            optional: true,
            dev_optional: true,
            peer: true,
            extraneous: true,
            ..Default::default()
        });
        self.all_dirty = true;
        id
    }

    /// Node._loadDeps: peer and peerOptional, prod, optional, then dev.
    /// A later type for the same name replaces the earlier edge and moves
    /// it to the end.
    pub fn load_deps(&mut self, id: Id, load_dev: bool) {
        for e in std::mem::take(&mut self.nodes[id].edges_out) {
            self.edges[e].detached = true;
        }
        let pkg = self.nodes[id].package.clone();
        let deps = |field: &str| -> Vec<(String, String)> {
            pkg.get(field)
                .and_then(Value::as_object)
                .map(|m| {
                    m.iter()
                        .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                        .collect()
                })
                .unwrap_or_default()
        };
        let peer_meta = pkg.get("peerDependenciesMeta").and_then(Value::as_object);
        let is_optional_peer = |name: &str| {
            peer_meta
                .and_then(|m| m.get(name))
                .and_then(|v| v.get("optional"))
                .and_then(Value::as_bool)
                == Some(true)
        };
        let peers = deps("peerDependencies");
        for (name, spec) in &peers {
            if !is_optional_peer(name) {
                self.add_edge(id, EdgeType::Peer, name, spec);
            }
        }
        for (name, spec) in &peers {
            if is_optional_peer(name) {
                self.add_edge(id, EdgeType::PeerOptional, name, spec);
            }
        }
        for (name, spec) in deps("dependencies") {
            self.add_edge(id, EdgeType::Prod, &name, &spec);
        }
        for (name, spec) in deps("optionalDependencies") {
            self.add_edge(id, EdgeType::Optional, &name, &spec);
        }
        if load_dev {
            for (name, spec) in deps("devDependencies") {
                self.add_edge(id, EdgeType::Dev, &name, &spec);
            }
        }
        self.all_dirty = true;
    }

    fn add_edge(&mut self, from: Id, ty: EdgeType, name: &str, spec: &str) {
        let k = key(name);
        let existing: Vec<EdgeId> = self.nodes[from]
            .edges_out
            .iter()
            .copied()
            .filter(|&e| key(&self.edges[e].name) == k)
            .collect();
        for e in existing {
            self.edges[e].detached = true;
            self.nodes[from].edges_out.retain(|&x| x != e);
        }
        let id = self.edges.len();
        self.edges.push(Edge {
            from,
            ty,
            name: name.to_string(),
            spec: spec.to_string(),
            to: None,
            peer_conflicted: false,
            detached: false,
        });
        self.nodes[from].edges_out.push(id);
    }

    // ----- structure ----------------------------------------------------

    pub fn child(&self, parent: Id, name: &str) -> Option<Id> {
        let k = key(name);
        self.nodes[parent]
            .children
            .iter()
            .find(|(c, _)| *c == k)
            .map(|(_, id)| *id)
    }

    pub fn children(&self, id: Id) -> Vec<Id> {
        self.nodes[id].children.iter().map(|(_, c)| *c).collect()
    }

    pub fn resolve_parent(&self, id: Id) -> Option<Id> {
        self.nodes[id].parent.or(self.nodes[id].fs_parent)
    }

    pub fn is_top(&self, id: Id) -> bool {
        self.nodes[id].parent.is_none()
    }

    pub fn depth(&self, id: Id) -> usize {
        match self.nodes[id].parent {
            None => 0,
            Some(p) => self.depth(p) + 1,
        }
    }

    pub fn root_of(&self, mut id: Id) -> Id {
        while let Some(p) = self.resolve_parent(id) {
            id = p;
        }
        // A link's target follows the link's root.
        id
    }

    pub fn in_main_tree(&self, id: Id) -> bool {
        let mut cur = id;
        loop {
            if cur == self.root {
                return true;
            }
            match self.resolve_parent(cur) {
                Some(p) => cur = p,
                None => {
                    // A link target with no fsParent is in the tree if a
                    // link in the tree points at it.
                    return self
                        .nodes
                        .iter()
                        .enumerate()
                        .any(|(i, n)| n.is_link && n.target == Some(cur) && i != cur && self.in_main_tree_strict(i));
                }
            }
        }
    }

    fn in_main_tree_strict(&self, id: Id) -> bool {
        let mut cur = id;
        while let Some(p) = self.resolve_parent(cur) {
            cur = p;
        }
        cur == self.root
    }

    pub fn ancestry(&self, id: Id) -> Vec<Id> {
        let mut out = vec![id];
        let mut cur = id;
        while let Some(p) = self.resolve_parent(cur) {
            out.push(p);
            cur = p;
        }
        out
    }

    pub fn is_descendant_of(&self, id: Id, ancestor: Id) -> bool {
        let mut cur = Some(id);
        while let Some(c) = cur {
            if c == ancestor {
                return true;
            }
            cur = self.resolve_parent(c);
        }
        false
    }

    pub fn resolve(&self, id: Id, name: &str) -> Option<Id> {
        let mut cur = Some(id);
        while let Some(c) = cur {
            if let Some(child) = self.child(c, name) {
                return Some(child);
            }
            cur = self.resolve_parent(c);
        }
        None
    }

    pub fn location(&self, id: Id) -> String {
        let root = self.root_of(id);
        relpath(&self.nodes[root].realpath, &self.nodes[id].path)
    }

    pub fn package_name(&self, id: Id) -> Option<&str> {
        self.nodes[id].package.get("name").and_then(Value::as_str)
    }

    pub fn version(&self, id: Id) -> Option<&str> {
        self.nodes[id].package.get("version").and_then(Value::as_str)
    }

    pub fn target(&self, id: Id) -> Id {
        if self.nodes[id].is_link {
            self.nodes[id].target.unwrap_or(id)
        } else {
            id
        }
    }

    /// Sets `parent` as the node's parent, clobbering a same-named child,
    /// and moves the node's subtree along (Node `parent` setter).
    pub fn set_parent(&mut self, id: Id, parent: Id) {
        let parent = self.target(parent);
        if parent == id || self.nodes[id].parent == Some(parent) {
            return;
        }
        self.unlink_from_parents(id);
        let name = self.nodes[id].name.clone();
        if let Some(old) = self.child(parent, &name)
            && old != id
        {
            self.detach(old);
        }
        self.nodes[parent].children.push((key(&name), id));
        self.nodes[id].parent = Some(parent);
        let new_path = join(&join(&self.nodes[parent].path.clone(), "node_modules"), &name);
        self.change_path(id, &new_path);
        self.all_dirty = true;
    }

    pub fn set_fs_parent(&mut self, id: Id, fs_parent: Id) {
        self.unlink_from_parents(id);
        self.nodes[fs_parent].fs_children.push(id);
        self.nodes[id].fs_parent = Some(fs_parent);
        self.all_dirty = true;
    }

    fn unlink_from_parents(&mut self, id: Id) {
        if let Some(p) = self.nodes[id].parent.take() {
            self.nodes[p].children.retain(|(_, c)| *c != id);
        }
        if let Some(p) = self.nodes[id].fs_parent.take() {
            self.nodes[p].fs_children.retain(|c| *c != id);
        }
    }

    fn change_path(&mut self, id: Id, new_path: &str) {
        let old = self.nodes[id].path.clone();
        if old == new_path {
            return;
        }
        self.nodes[id].path = new_path.to_string();
        if !self.nodes[id].is_link {
            self.nodes[id].realpath = new_path.to_string();
        }
        for fs in self.nodes[id].fs_children.clone() {
            let rel = relpath(&old, &self.nodes[fs].path);
            self.change_path(fs, &join(new_path, &rel));
        }
        for (_, c) in self.nodes[id].children.clone() {
            let name = self.nodes[c].name.clone();
            self.change_path(c, &join(&join(new_path, "node_modules"), &name));
        }
    }

    /// `node.root = null`: removes the node and its subtree from its tree.
    pub fn detach(&mut self, id: Id) {
        self.unlink_from_parents(id);
        // A link's target leaves with it when the link was its only way in.
        self.all_dirty = true;
    }

    /// Node.replace: `new` takes `old`'s place and children.
    pub fn replace(&mut self, new: Id, old: Id) {
        let parent = self.nodes[old].parent;
        let fs_parent = self.nodes[old].fs_parent;
        if !self.nodes[new].is_link {
            for (_, kid) in self.nodes[old].children.clone() {
                self.set_parent(kid, new);
            }
        }
        self.nodes[new].name = self.nodes[old].name.clone();
        match (parent, fs_parent) {
            (Some(p), _) => self.set_parent(new, p),
            (None, Some(f)) => {
                let path = self.nodes[old].path.clone();
                self.detach(old);
                self.change_path(new, &path);
                self.set_fs_parent(new, f);
            }
            (None, None) => {}
        }
        self.all_dirty = true;
    }

    // ----- edges ----------------------------------------------------------

    pub fn mark_dirty(&mut self) {
        self.all_dirty = true;
    }

    /// Recomputes every live edge's target and every node's edgesIn.
    pub fn refresh(&mut self) {
        if !self.all_dirty && self.dirty.is_empty() {
            return;
        }
        for n in &mut self.nodes {
            n.edges_in.clear();
        }
        for e in 0..self.edges.len() {
            if self.edges[e].detached {
                continue;
            }
            let from = self.edges[e].from;
            let to = self.resolve(from, &self.edges[e].name.clone());
            self.edges[e].to = to;
            if let Some(t) = to {
                self.nodes[t].edges_in.push(e);
            }
        }
        self.dirty.clear();
        self.all_dirty = false;
    }

    pub fn edge_to(&mut self, e: EdgeId) -> Option<Id> {
        self.refresh();
        self.edges[e].to
    }

    pub fn edges_out(&self, id: Id) -> Vec<EdgeId> {
        self.nodes[id].edges_out.clone()
    }

    pub fn edge_out(&self, id: Id, name: &str) -> Option<EdgeId> {
        let k = key(name);
        self.nodes[id]
            .edges_out
            .iter()
            .copied()
            .find(|&e| key(&self.edges[e].name) == k)
    }

    pub fn edges_in(&mut self, id: Id) -> Vec<EdgeId> {
        self.refresh();
        self.nodes[id].edges_in.clone()
    }

    /// Edge.error === null
    pub fn edge_valid(&mut self, e: EdgeId) -> bool {
        self.refresh();
        let edge = self.edges[e].clone();
        let Some(to) = edge.to else {
            return edge.ty.optional();
        };
        if edge.ty.peer() && self.nodes[to].parent == Some(edge.from) && !self.is_top(edge.from) {
            return false;
        }
        self.satisfied_by(e, to)
    }

    /// Edge.satisfiedBy(node): same name and depValid.
    pub fn satisfied_by(&self, e: EdgeId, node: Id) -> bool {
        let edge = &self.edges[e];
        if self.nodes[node].name != edge.name {
            return false;
        }
        self.dep_valid(node, &edge.name, &edge.spec, edge.from)
    }

    /// arborist dep-valid.js for the spec types lockstep supports.
    pub fn dep_valid(&self, node: Id, name: &str, spec: &str, from: Id) -> bool {
        let Ok(parsed) = npa::parse(name, spec) else {
            return false;
        };
        match parsed {
            Spec::Range { range } => {
                if range.trim() == "*" {
                    return true;
                }
                self.version_satisfies(node, &range)
            }
            Spec::Version { version } => self.version_satisfies(node, &version),
            Spec::Tag { .. } => self.nodes[node]
                .resolved
                .as_deref()
                .is_some_and(|r| r.starts_with("http")),
            Spec::Alias { spec: sub, .. } => {
                // dep-valid.js checks only the alias's sub-spec.
                match *sub {
                    Spec::Range { range } => range.trim() == "*" || self.version_satisfies(node, &range),
                    Spec::Version { version } => self.version_satisfies(node, &version),
                    Spec::Tag { .. } => true,
                    _ => false,
                }
            }
            Spec::Directory { path } => {
                if !self.nodes[node].is_link {
                    return false;
                }
                let base = self.nodes[self.target(from)].realpath.clone();
                let want = npa::resolve_path(&base, &path);
                self.nodes[node].realpath == want
            }
        }
    }

    fn version_satisfies(&self, node: Id, range: &str) -> bool {
        let Some(v) = self.version(node).and_then(Version::parse) else {
            return false;
        };
        Range::parse(range).is_some_and(|r| r.satisfies(&v))
    }

    pub fn node_satisfies(&self, node: Id, e: EdgeId) -> bool {
        self.satisfied_by(e, node)
    }

    // ----- comparisons ---------------------------------------------------

    /// Node.matches
    pub fn matches(&self, a: Id, b: Id) -> bool {
        if a == b {
            return true;
        }
        let (na, nb) = (&self.nodes[a], &self.nodes[b]);
        if na.name != nb.name {
            return false;
        }
        if na.is_link {
            return nb.is_link
                && match (na.target, nb.target) {
                    (Some(x), Some(y)) => self.matches(x, y),
                    _ => false,
                };
        }
        if na.is_project_root && nb.is_project_root {
            return na.path == nb.path;
        }
        if let (Some(x), Some(y)) = (&na.integrity, &nb.integrity) {
            return x == y;
        }
        if let (Some(x), Some(y)) = (&na.resolved, &nb.resolved) {
            return x == y;
        }
        match (
            self.package_name(a),
            self.package_name(b),
            self.version(a),
            self.version(b),
        ) {
            (Some(pa), Some(pb), Some(va), Some(vb)) => pa == pb && va == vb,
            _ => false,
        }
    }

    /// gatherDepSet(set, filter)
    pub fn gather_dep_set(&mut self, start: &[Id], filter: &dyn Fn(&Tree, EdgeId) -> bool) -> Vec<Id> {
        self.refresh();
        let mut deps: Vec<Id> = Vec::new();
        let mut seen: HashSet<Id> = HashSet::new();
        for &s in start {
            if seen.insert(s) {
                deps.push(s);
            }
        }
        let mut i = 0;
        while i < deps.len() {
            let n = deps[i];
            for e in self.nodes[n].edges_out.clone() {
                if let Some(to) = self.edges[e].to
                    && filter(self, e)
                    && seen.insert(to)
                {
                    deps.push(to);
                }
            }
            i += 1;
        }
        let mut changed = true;
        while changed && !seen.is_empty() {
            changed = false;
            for dep in deps.clone() {
                if !seen.contains(&dep) {
                    continue;
                }
                for e in self.nodes[dep].edges_in.clone() {
                    let from = self.edges[e].from;
                    if !seen.contains(&from) && filter(self, e) {
                        changed = true;
                        seen.remove(&dep);
                        break;
                    }
                }
            }
        }
        deps.into_iter().filter(|d| seen.contains(d)).collect()
    }

    /// Node.canReplaceWith(other, ignorePeers): `self_id` can be replaced by
    /// `other`.
    pub fn can_replace_with(&mut self, self_id: Id, other: Id, ignore_peers: &[String]) -> bool {
        if self.nodes[other].name != self.nodes[self_id].name {
            return false;
        }
        if self.package_name(other) != self.package_name(self_id) {
            return false;
        }
        let me = self_id;
        let dep_set: HashSet<Id> = self
            .gather_dep_set(&[me], &|t: &Tree, e| t.edges[e].to != Some(me) && t.edge_valid_ro(e))
            .into_iter()
            .collect();
        for e in self.edges_in(self_id) {
            let edge = self.edges[e].clone();
            if !self.is_top(self_id)
                && self.nodes[edge.from].parent == self.nodes[self_id].parent
                && edge.ty.peer()
                && ignore_peers.contains(&self.nodes[edge.from].name)
            {
                continue;
            }
            if !dep_set.contains(&edge.from) && !self.satisfied_by(e, other) {
                return false;
            }
        }
        true
    }

    /// Edge validity without refreshing (caller has refreshed).
    pub fn edge_valid_ro(&self, e: EdgeId) -> bool {
        let edge = &self.edges[e];
        let Some(to) = edge.to else {
            return edge.ty.optional();
        };
        if edge.ty.peer() && self.nodes[to].parent == Some(edge.from) && self.nodes[edge.from].parent.is_some() {
            return false;
        }
        self.satisfied_by(e, to)
    }

    /// Node.canDedupe(preferDedupe)
    pub fn can_dedupe(&mut self, id: Id, prefer_dedupe: bool) -> bool {
        let Some(rp) = self.resolve_parent(id) else {
            return false;
        };
        let Some(rpp) = self.resolve_parent(rp) else {
            return false;
        };
        if self.edges_in(id).is_empty() {
            return true;
        }
        let name = self.nodes[id].name.clone();
        let Some(other) = self.resolve(rpp, &name) else {
            return false;
        };
        if self.matches(other, id) {
            return true;
        }
        if !self.can_replace_with(id, other, &[]) {
            return false;
        }
        if prefer_dedupe {
            return true;
        }
        match (
            self.version(other).and_then(Version::parse),
            self.version(id).and_then(Version::parse),
        ) {
            (Some(o), Some(m)) => o >= m,
            _ => false,
        }
    }

    /// The nodes reachable from the project root through children and
    /// fsChildren (arborist's inventory), root first.
    pub fn inventory(&self) -> Vec<Id> {
        let mut out = Vec::new();
        let mut stack = vec![self.root];
        let mut seen = HashSet::new();
        while let Some(n) = stack.pop() {
            if !seen.insert(n) {
                continue;
            }
            out.push(n);
            for c in self.nodes[n].fs_children.iter().rev() {
                stack.push(*c);
            }
            for (_, c) in self.nodes[n].children.iter().rev() {
                stack.push(*c);
            }
            if self.nodes[n].is_link
                && let Some(t) = self.nodes[n].target
            {
                stack.push(t);
            }
        }
        out
    }
}
