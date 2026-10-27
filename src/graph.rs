//! Dependency edges between lockfile locations, resolved the way Node and
//! arborist do: look in the dependent's own `node_modules`, then walk up.

use std::collections::{HashMap, HashSet};

use crate::lockfile::Entry;

/// The location whose `node_modules` is searched after `loc`'s own.
pub fn parent_location(loc: &str) -> Option<&str> {
    if loc.is_empty() {
        return None;
    }
    if let Some(i) = loc.rfind("/node_modules/") {
        return Some(&loc[..i]);
    }
    if loc.starts_with("node_modules/") {
        return Some("");
    }
    // A local package outside node_modules (a `file:` link target):
    // its fsParent is the project root.
    Some("")
}

pub fn resolve(from: &str, name: &str, present: &HashMap<&str, usize>) -> Option<usize> {
    let mut loc = Some(from);
    while let Some(l) = loc {
        let candidate = if l.is_empty() {
            format!("node_modules/{name}")
        } else {
            format!("{l}/node_modules/{name}")
        };
        if let Some(&i) = present.get(candidate.as_str()) {
            return Some(i);
        }
        loc = parent_location(l);
    }
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeKind {
    Prod,
    Optional,
    Peer,
    PeerOptional,
}

#[derive(Clone, Debug)]
pub struct Edge {
    pub from: usize,
    pub to: usize,
    pub kind: EdgeKind,
}

impl Edge {
    pub fn optional(&self) -> bool {
        matches!(self.kind, EdgeKind::Optional | EdgeKind::PeerOptional)
    }
}

pub struct Graph {
    pub out: Vec<Vec<Edge>>,
    pub inbound: Vec<Vec<Edge>>,
}

impl Graph {
    pub fn build(entries: &[Entry]) -> Graph {
        let mut by_loc: HashMap<&str, usize> = HashMap::new();
        for (i, e) in entries.iter().enumerate() {
            by_loc.insert(e.path.as_str(), i);
        }
        let mut out: Vec<Vec<Edge>> = vec![Vec::new(); entries.len()];
        let mut inbound: Vec<Vec<Edge>> = vec![Vec::new(); entries.len()];
        for (i, e) in entries.iter().enumerate() {
            // A link's edges belong to its target.
            if e.link {
                if let Some(target) = e.resolved.as_deref().and_then(|r| by_loc.get(r)) {
                    let edge = Edge {
                        from: i,
                        to: *target,
                        kind: EdgeKind::Prod,
                    };
                    out[i].push(edge.clone());
                    inbound[*target].push(edge);
                }
                continue;
            }
            let peer_optional: HashSet<&str> = e
                .raw
                .get("peerDependenciesMeta")
                .and_then(|m| m.as_object())
                .map(|m| {
                    m.iter()
                        .filter(|(_, v)| v.get("optional").and_then(|o| o.as_bool()) == Some(true))
                        .map(|(k, _)| k.as_str())
                        .collect()
                })
                .unwrap_or_default();
            let mut add = |name: &str, kind: EdgeKind| {
                if let Some(to) = resolve(&e.path, name, &by_loc) {
                    let edge = Edge { from: i, to, kind };
                    out[i].push(edge.clone());
                    inbound[to].push(edge);
                }
            };
            let optional: HashSet<&str> = e.optional_dependencies.iter().map(String::as_str).collect();
            for name in &e.dependencies {
                if !optional.contains(name.as_str()) {
                    add(name, EdgeKind::Prod);
                }
            }
            for name in &e.optional_dependencies {
                add(name, EdgeKind::Optional);
            }
            for name in &e.peer_dependencies {
                let kind = if peer_optional.contains(name.as_str()) {
                    EdgeKind::PeerOptional
                } else {
                    EdgeKind::Peer
                };
                add(name, kind);
            }
            if e.is_root() {
                for key in ["devDependencies"] {
                    if let Some(map) = e.raw.get(key).and_then(|v| v.as_object()) {
                        for name in map.keys() {
                            add(name, EdgeKind::Prod);
                        }
                    }
                }
            }
        }
        Graph { out, inbound }
    }

    /// arborist `optionalSet`: the failed optional node, every dependent that
    /// needs it non-optionally (up to the optional boundary), and the
    /// dependencies that nothing outside that set uses.
    pub fn optional_set(&self, node: usize) -> HashSet<usize> {
        let mut set: Vec<usize> = vec![node];
        let mut seen: HashSet<usize> = HashSet::from([node]);
        let mut i = 0;
        while i < set.len() {
            let n = set[i];
            for edge in &self.inbound[n] {
                if !edge.optional() && seen.insert(edge.from) {
                    set.push(edge.from);
                }
            }
            i += 1;
        }
        self.gather_dep_set(seen)
    }

    /// arborist `gatherDepSet(set, edge => !edge.optional)`: grow the set
    /// along non-optional dependencies, then drop members that a node
    /// outside the set needs non-optionally, until stable.
    fn gather_dep_set(&self, start: HashSet<usize>) -> HashSet<usize> {
        let mut set = start;
        let mut queue: Vec<usize> = set.iter().copied().collect();
        while let Some(n) = queue.pop() {
            for edge in &self.out[n] {
                if !edge.optional() && set.insert(edge.to) {
                    queue.push(edge.to);
                }
            }
        }
        loop {
            let mut changed = false;
            let members: Vec<usize> = set.iter().copied().collect();
            for n in members {
                if self.inbound[n].iter().any(|e| !e.optional() && !set.contains(&e.from)) {
                    set.remove(&n);
                    changed = true;
                }
            }
            if !changed || set.is_empty() {
                break;
            }
        }
        set
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parents() {
        assert_eq!(parent_location("node_modules/a"), Some(""));
        assert_eq!(
            parent_location("node_modules/a/node_modules/@s/b"),
            Some("node_modules/a")
        );
        assert_eq!(parent_location("pkg/node_modules/x"), Some("pkg"));
        assert_eq!(parent_location("pkg"), Some(""));
        assert_eq!(parent_location(""), None);
    }
}
