//! arborist place-dep.js and can-place-dep.js: where a resolved dependency
//! (and the peer set that came with it) goes in the tree.

use std::collections::HashSet;

use super::tree::{EdgeId, Id, NewNode, Tree};
use crate::collate;
use crate::semver::Version;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    Ok,
    Replace,
    Keep,
    Conflict,
}

pub struct PlaceOptions<'a> {
    pub prefer_dedupe: bool,
    pub update_names: &'a [String],
    pub force: bool,
    pub strict_peer_deps: bool,
}

/// deepest-nesting-target.js
pub fn deepest_nesting_target(t: &Tree, start: Id, name: Option<&str>) -> Id {
    for target in t.ancestry(start) {
        if t.nodes[target].is_project_root || t.resolve_parent(target).is_none() {
            return target;
        }
        let Some(name) = name else { return target };
        match t.edge_out(target, name) {
            None => return target,
            Some(e) if !t.edges[e].ty.peer() => return target,
            _ => {}
        }
    }
    start
}

/// peer-entry-sets.js: the entry edges into the peer groups `node` is in,
/// each with the set of peers that edge brings in.
pub fn peer_entry_sets(t: &mut Tree, node: Id) -> Vec<(EdgeId, Vec<Id>)> {
    t.refresh();
    let mut union: Vec<Id> = vec![node];
    let mut in_union: HashSet<Id> = HashSet::from([node]);
    let mut i = 0;
    while i < union.len() {
        let n = union[i];
        for e in t.nodes[n].edges_out.clone() {
            if let Some(to) = t.edges[e].to
                && t.edges[e].ty.peer()
                && t.edge_valid_ro(e)
                && in_union.insert(to)
            {
                union.push(to);
            }
        }
        for e in t.nodes[n].edges_in.clone() {
            let from = t.edges[e].from;
            if t.edges[e].ty.peer() && t.edge_valid_ro(e) && in_union.insert(from) {
                union.push(from);
            }
        }
        i += 1;
    }
    let mut out: Vec<(EdgeId, Vec<Id>)> = Vec::new();
    for &peer in &union {
        for e in t.nodes[peer].edges_in.clone() {
            if !t.edge_valid_ro(e) {
                continue;
            }
            let from = t.edges[e].from;
            if !t.edges[e].ty.peer() || t.is_top(from) {
                let mut sub = vec![peer];
                let mut in_sub = HashSet::from([peer]);
                let mut j = 0;
                while j < sub.len() {
                    let s = sub[j];
                    for pe in t.nodes[s].edges_out.clone() {
                        if let Some(to) = t.edges[pe].to
                            && t.edges[pe].ty.peer()
                            && t.edge_valid_ro(pe)
                            && in_sub.insert(to)
                        {
                            sub.push(to);
                        }
                    }
                    j += 1;
                }
                if in_sub.contains(&node) {
                    match out.iter_mut().find(|(k, _)| *k == e) {
                        Some(slot) => slot.1 = sub,
                        None => out.push((e, sub)),
                    }
                }
            }
        }
    }
    out
}

/// What a CanPlaceDep check passes down to its peers' checks.
#[derive(Clone, Copy)]
pub struct CpdParent {
    /// The parent check's `deepestNestingTarget`.
    pub dnt: Id,
    /// `top.edge`, the peer entry edge.
    pub top_edge: EdgeId,
}

pub struct Cpd {
    pub can_place: Placement,
    pub can_place_self: Placement,
    pub target: Id,
    pub dnt: Id,
    pub top_edge: EdgeId,
}

struct CpdState {
    dep: Id,
    target: Id,
    edge: EdgeId,
    name: String,
    prefer_dedupe: bool,
    explicit: bool,
    parent: Option<CpdParent>,
    peer_path: Vec<Id>,
    can_place_self: Option<Placement>,
    peers_memo: Option<Placement>,
    dnt: Id,
    top_edge: EdgeId,
}

#[allow(clippy::too_many_arguments)]
pub fn can_place_dep(
    t: &mut Tree,
    dep: Id,
    target: Id,
    edge: EdgeId,
    prefer_dedupe: bool,
    explicit: bool,
    parent: Option<CpdParent>,
    peer_path: Vec<Id>,
) -> Cpd {
    let name = t.edges[edge].name.clone();
    let dnt_start = parent.map(|p| p.dnt).unwrap_or(t.edges[edge].from);
    let dnt = deepest_nesting_target(t, dnt_start, Some(&name));
    let top_edge = parent.map(|p| p.top_edge).unwrap_or(edge);
    let mut s = CpdState {
        dep,
        target,
        edge,
        name,
        prefer_dedupe: prefer_dedupe || t.edges[edge].ty.peer(),
        explicit,
        parent,
        peer_path,
        can_place_self: None,
        peers_memo: None,
        dnt,
        top_edge,
    };
    let can_place = check_can_place(t, &mut s);
    Cpd {
        can_place,
        can_place_self: s.can_place_self.unwrap_or(can_place),
        target,
        dnt,
        top_edge,
    }
}

fn check_can_place(t: &mut Tree, s: &mut CpdState) -> Placement {
    t.refresh();
    let current = t.child(s.target, &s.name);
    let target_edge = t.edge_out(s.target, &s.name);
    if !t.nodes[s.dep].errors.is_empty() {
        return if current.is_some() {
            Placement::Replace
        } else {
            Placement::Ok
        };
    }
    if let Some(te) = target_edge {
        if t.edges[te].ty.peer() && !t.is_top(s.target) {
            return Placement::Conflict;
        }
        if current.is_none() && !t.node_satisfies(s.dep, te) && te != s.edge {
            return Placement::Conflict;
        }
    }
    match current {
        Some(c) => check_current(t, s, c),
        None => check_no_current(t, s),
    }
}

fn check_current(t: &mut Tree, s: &mut CpdState, current: Id) -> Placement {
    let edge = s.edge;
    let edge_override = !t.node_satisfies(s.dep, edge);
    if t.matches(s.dep, current) && (t.node_satisfies(current, edge) || edge_override) {
        return if s.explicit {
            Placement::Replace
        } else {
            Placement::Keep
        };
    }
    let cur_ver = t.version(current).and_then(Version::parse);
    let new_ver = t.version(s.dep).and_then(Version::parse);
    let try_replace = matches!((&cur_ver, &new_ver), (Some(c), Some(n)) if n >= c);
    if try_replace && t.can_replace_with(current, s.dep, &[]) {
        let cpp = can_place_peers(t, s, Placement::Replace);
        if cpp != Placement::Conflict {
            return cpp;
        }
    }
    if t.node_satisfies(current, edge) && (!s.explicit || s.prefer_dedupe) {
        return Placement::Keep;
    }
    if s.prefer_dedupe && !try_replace && t.can_replace_with(current, s.dep, &[]) {
        let cpp = can_place_peers(t, s, Placement::Replace);
        if cpp != Placement::Conflict {
            return cpp;
        }
    }
    if s.target != s.dnt {
        return Placement::Conflict;
    }
    let edge_peer = t.edges[edge].ty.peer();
    if !edge_peer && s.target == t.edges[edge].from {
        return can_place_peers(t, s, Placement::Replace);
    }
    if s.parent.is_none() && !edge_peer {
        return Placement::Conflict;
    }
    let mut can_replace = true;
    let vr = t.nodes[s.dep].parent;
    let vr_children: Vec<String> = vr
        .map(|v| t.children(v).iter().map(|c| t.nodes[*c].name.clone()).collect())
        .unwrap_or_default();
    for (entry_edge, current_peers) in peer_entry_sets(t, current) {
        if entry_edge == s.edge || entry_edge == s.top_edge {
            continue;
        }
        let Some(entry_node) = t.edge_to(entry_edge) else {
            continue;
        };
        let entry_name = t.nodes[entry_node].name.clone();
        let entry_rep = vr.and_then(|v| t.child(v, &entry_name));
        if let Some(rep) = entry_rep
            && t.can_replace_with(entry_node, rep, &vr_children)
        {
            continue;
        }
        let mut can_clobber = entry_rep.is_none();
        if entry_rep.is_none() {
            let mut walk = vec![entry_node];
            let mut seen = HashSet::from([entry_node]);
            let mut i = 0;
            'outer: while i < walk.len() {
                let cur = walk[i];
                for e in t.edges_out(cur) {
                    if !t.edges[e].ty.peer() || !t.edge_valid(e) {
                        continue;
                    }
                    let rep = vr.and_then(|v| t.child(v, &t.edges[e].name.clone()));
                    match rep {
                        None => {
                            if let Some(to) = t.edge_to(e)
                                && seen.insert(to)
                            {
                                walk.push(to);
                            }
                        }
                        Some(r) => {
                            if !t.node_satisfies(r, e) {
                                can_clobber = false;
                                break 'outer;
                            }
                        }
                    }
                }
                i += 1;
            }
        }
        if can_clobber {
            continue;
        }
        let entry_from = t.edges[entry_edge].from;
        for peer in current_peers {
            let peer_name = t.nodes[peer].name.clone();
            let cur_deep = deepest_nesting_target(t, entry_from, Some(&peer_name));
            if cur_deep == s.target || t.is_descendant_of(s.target, cur_deep) {
                can_replace = false;
                break;
            }
        }
    }
    if can_replace {
        return can_place_peers(t, s, Placement::Replace);
    }
    Placement::Conflict
}

fn check_no_current(t: &mut Tree, s: &mut CpdState) -> Placement {
    let entry_from = t.edges[s.top_edge].from;
    let current = if s.target != entry_from {
        t.resolve(s.target, &s.name)
    } else {
        None
    };
    if let Some(cur) = current {
        for e in t.edges_in(cur) {
            let from = t.edges[e].from;
            if t.is_descendant_of(from, s.target) && t.edge_valid(e) && !t.node_satisfies(s.dep, e) {
                return Placement::Conflict;
            }
        }
    }
    can_place_peers(t, s, Placement::Ok)
}

fn can_place_peers(t: &mut Tree, s: &mut CpdState, state: Placement) -> Placement {
    s.can_place_self = Some(state);
    if let Some(memo) = s.peers_memo {
        return memo;
    }
    let mut peer_path = s.peer_path.clone();
    peer_path.push(s.dep);
    let mut saw_conflict = false;
    for pe in t.edges_out(s.dep) {
        if !t.edges[pe].ty.peer() {
            continue;
        }
        let Some(peer) = t.edge_to(pe) else { continue };
        if peer_path.contains(&peer) {
            continue;
        }
        let peer_name = t.nodes[peer].name.clone();
        let target = deepest_nesting_target(t, s.target, Some(&peer_name));
        let cpp = can_place_dep(
            t,
            peer,
            target,
            pe,
            true,
            false,
            Some(CpdParent {
                dnt: s.dnt,
                top_edge: s.top_edge,
            }),
            peer_path.clone(),
        );
        if cpp.can_place == Placement::Conflict {
            saw_conflict = true;
        }
    }
    let result = if saw_conflict { Placement::Conflict } else { state };
    s.peers_memo = Some(result);
    result
}

/// The outcome of one PlaceDep and the peers placed with it.
pub struct Pd {
    pub edge: EdgeId,
    pub placed: Option<Id>,
    pub can_place_self: Option<Placement>,
    pub need_evaluation: Vec<Id>,
    pub children: Vec<Pd>,
}

#[derive(Clone, Copy)]
struct PdParent {
    start: Id,
    cpd: CpdParent,
    top_edge: EdgeId,
}

pub fn place_dep(t: &mut Tree, opts: &PlaceOptions, edge: EdgeId, dep: Id, explicit: bool) -> Result<Pd, String> {
    place(t, opts, edge, dep, explicit, None, opts.update_names)
}

fn place(
    t: &mut Tree,
    opts: &PlaceOptions,
    edge: EdgeId,
    dep: Id,
    explicit: bool,
    parent: Option<PdParent>,
    update_names: &[String],
) -> Result<Pd, String> {
    let name = t.edges[edge].name.clone();
    let mut pd = Pd {
        edge,
        placed: None,
        can_place_self: None,
        need_evaluation: Vec::new(),
        children: Vec::new(),
    };
    let current = t.edge_to(edge);
    if current.is_some() && t.edge_valid(edge) && !explicit && !update_names.contains(&name) {
        return Ok(pd);
    }
    let from = parent.map(|p| p.start).unwrap_or(t.edges[edge].from);
    let start = deepest_nesting_target(t, from, Some(&name));
    let top_edge = parent.map(|p| p.top_edge).unwrap_or(edge);

    let mut can_place: Option<Cpd> = None;
    let mut can_place_self: Option<Cpd> = None;
    for target in t.ancestry(start) {
        if let Some(te) = t.edge_out(target, &name)
            && !t.is_top(target)
            && t.edges[te].ty.peer()
        {
            continue;
        }
        let cpd = can_place_dep(
            t,
            dep,
            target,
            edge,
            opts.prefer_dedupe,
            explicit,
            parent.map(|p| p.cpd),
            Vec::new(),
        );
        let conflict = cpd.can_place == Placement::Conflict;
        if cpd.can_place_self != Placement::Conflict {
            can_place_self = Some(Cpd { ..cpd_copy(&cpd) });
        }
        if conflict {
            break;
        }
        can_place = Some(cpd);
        if !t.nodes[dep].errors.is_empty() {
            break;
        }
    }
    let cp = match can_place {
        Some(c) => c,
        None => {
            if !opts.force && (is_mine(t, top_edge) || opts.strict_peer_deps) {
                return Err(eresolve(t, top_edge, dep));
            }
            match can_place_self {
                None => {
                    t.edges[edge].peer_conflicted = true;
                    return Ok(pd);
                }
                Some(c) => c,
            }
        }
    };
    pd.can_place_self = Some(cp.can_place_self);
    let target = cp.target;
    let placement = if cp.can_place == Placement::Conflict {
        cp.can_place_self
    } else {
        cp.can_place
    };
    if placement == Placement::Keep {
        if t.edges[edge].ty.peer() && !t.edge_valid(edge) {
            t.edges[edge].peer_conflicted = true;
        }
        prune_dedupable(t, opts, target, true);
        return Ok(pd);
    }
    // Break dependency loops with a link to the earlier instance.
    let mut p = Some(target);
    while let Some(cur) = p {
        if t.matches(cur, dep) && !t.is_top(cur) {
            let path = tree_child_path(t, target, &name);
            let link = t.add_link(&name, &path, cur);
            t.set_parent(link, target);
            pd.placed = Some(link);
            return Ok(pd);
        }
        p = t.resolve_parent(cur);
    }
    let virtual_root = t.nodes[dep].parent;
    let placed = if t.nodes[dep].is_link {
        let tgt = t.nodes[dep].target.expect("link has a target");
        let path = tree_child_path(t, target, &name);
        t.add_link(&name, &path, tgt)
    } else {
        let d = t.nodes[dep].clone();
        let path = tree_child_path(t, target, &name);
        t.add_node(NewNode {
            name: d.name.clone(),
            path,
            package: d.package.clone(),
            resolved: d.resolved.clone(),
            integrity: d.integrity.clone(),
            load_dev: false,
            errors: d.errors.clone(),
        })
    };
    pd.placed = Some(placed);
    match t.child(target, &name) {
        Some(old) => replace_old_dep(t, &mut pd, placed, old, dep),
        None => t.set_parent(placed, target),
    }
    if t.nodes[placed].is_link {
        adopt_link_target(t, placed);
    }
    if t.edges[edge].ty.peer() && !t.node_satisfies(placed, edge) {
        t.edges[edge].peer_conflicted = true;
    }
    if t.edge_valid(edge)
        && let Some(to) = t.edge_to(edge)
        && to != placed
    {
        prune_dedupable(t, opts, to, false);
    }
    let same_name: Vec<Id> = t.inventory().into_iter().filter(|&n| t.nodes[n].name == name).collect();
    for n in same_name {
        if t.is_descendant_of(n, target) && !t.is_top(n) && t.in_main_tree(n) {
            prune_dedupable(t, opts, n, false);
            if t.in_main_tree(n) {
                for kid in t.children(n) {
                    prune_dedupable(t, opts, kid, false);
                }
            }
        }
    }
    let child_parent = PdParent {
        start,
        cpd: CpdParent {
            dnt: cp.dnt,
            top_edge: cp.top_edge,
        },
        top_edge,
    };
    for pe in t.edges_out(placed) {
        if t.edge_valid(pe) || !t.edges[pe].ty.peer() || t.edges[pe].peer_conflicted {
            continue;
        }
        let Some(vr) = virtual_root else { continue };
        let Some(peer) = t.child(vr, &t.edges[pe].name.clone()) else {
            continue;
        };
        if !t.node_satisfies(peer, pe) {
            continue;
        }
        let child = place(t, opts, pe, peer, explicit, Some(child_parent), &[])?;
        pd.children.push(child);
    }
    Ok(pd)
}

fn cpd_copy(c: &Cpd) -> Cpd {
    Cpd {
        can_place: c.can_place,
        can_place_self: c.can_place_self,
        target: c.target,
        dnt: c.dnt,
        top_edge: c.top_edge,
    }
}

fn tree_child_path(t: &Tree, parent: Id, name: &str) -> String {
    super::tree::join(
        &super::tree::join(&t.nodes[t.target(parent)].path, "node_modules"),
        name,
    )
}

/// When a link lands in the tree, its target joins it at the target's own
/// path, as an fsChild of the project root when it is inside the project.
pub fn adopt_link_target(t: &mut Tree, link: Id) {
    let Some(target) = t.nodes[link].target else { return };
    if t.in_main_tree(target) && t.resolve_parent(target).is_some() {
        return;
    }
    let root = t.root;
    let root_path = t.nodes[root].path.clone();
    let real = t.nodes[target].realpath.clone();
    if real.starts_with(&format!("{root_path}/")) {
        t.set_fs_parent(target, root);
    }
    t.mark_dirty();
}

fn replace_old_dep(t: &mut Tree, pd: &mut Pd, placed: Id, old: Id, _dep: Id) {
    let target = t.nodes[old].parent.unwrap_or(t.root);
    let mut old_deps: Vec<Id> = Vec::new();
    for e in t.edges_out(old) {
        let name = t.edges[e].name.clone();
        if t.edge_out(placed, &name).is_none()
            && let Some(to) = t.edge_to(e)
        {
            let set = t.gather_dep_set(&[to], &|tr: &Tree, x| tr.edges[x].to != Some(to));
            old_deps.extend(set);
        }
    }
    let mut prune_peer_sets: Vec<Id> = Vec::new();
    for e in t.edges_in(old) {
        let from = t.edges[e].from;
        if t.node_satisfies(placed, e)
            || !t.edges[e].ty.peer()
            || t.nodes[from].parent != Some(target)
            || t.edges[e].peer_conflicted
        {
            continue;
        }
        for (entry_edge, _) in peer_entry_sets(t, from) {
            let Some(entry_node) = t.edge_to(entry_edge) else {
                continue;
            };
            let entry_from = t.edges[entry_edge].from;
            // deepestNestingTarget(entryNode) with no name is entryNode itself.
            if entry_node != target && !t.nodes[entry_from].is_project_root {
                let set = t.gather_dep_set(&[entry_node], &|tr: &Tree, x| {
                    tr.edges[x].to != Some(entry_node) && !tr.edges[x].peer_conflicted
                });
                prune_peer_sets.extend(set);
            } else {
                t.edges[e].peer_conflicted = true;
            }
        }
    }
    t.replace(placed, old);
    t.detach(old);
    prune_for_replacement(t, placed, &old_deps);
    for d in prune_peer_sets {
        for e in t.edges_in(d) {
            pd.need_evaluation.push(t.edges[e].from);
        }
        t.detach(d);
    }
}

fn prune_for_replacement(t: &mut Tree, node: Id, old_deps: &[Id]) {
    let mut invalid: Vec<Id> = Vec::new();
    for e in t.edges_out(node) {
        if let Some(to) = t.edge_to(e)
            && !t.edge_valid(e)
            && !invalid.contains(&to)
        {
            invalid.push(to);
        }
    }
    for &d in old_deps {
        let set = t.gather_dep_set(&[d], &|tr: &Tree, x| tr.edges[x].to != Some(d) && tr.edge_valid_ro(x));
        for n in set {
            if !invalid.contains(&n) {
                invalid.push(n);
            }
        }
    }
    let deps = t.gather_dep_set(&invalid, &|tr: &Tree, x| {
        tr.edges[x].from != node && tr.edges[x].to != Some(node) && tr.edge_valid_ro(x)
    });
    for d in deps {
        t.detach(d);
    }
}

pub fn prune_dedupable(t: &mut Tree, opts: &PlaceOptions, node: Id, descend: bool) {
    if !t.in_main_tree(node) {
        return;
    }
    if t.can_dedupe(node, opts.prefer_dedupe) {
        let deps = t.gather_dep_set(&[node], &|tr: &Tree, x| {
            tr.edges[x].to != Some(node) && tr.edge_valid_ro(x)
        });
        for d in deps {
            t.detach(d);
        }
        return;
    }
    if descend {
        let mut kids = t.children(node);
        kids.sort_by(|a, b| collate::compare(&t.location(*a), &t.location(*b)));
        for k in kids {
            prune_dedupable(t, opts, k, true);
        }
        let mut fs = t.nodes[node].fs_children.clone();
        fs.sort_by(|a, b| collate::compare(&t.location(*a), &t.location(*b)));
        for top in fs {
            let mut kids = t.children(top);
            kids.sort_by(|a, b| collate::compare(&t.location(*a), &t.location(*b)));
            for k in kids {
                prune_dedupable(t, opts, k, true);
            }
        }
    }
}

/// PlaceDep.isMine: the conflict is the project's own problem.
fn is_mine(t: &mut Tree, top_edge: EdgeId) -> bool {
    let node = t.edges[top_edge].from;
    if t.nodes[node].is_project_root {
        return true;
    }
    if !t.edges[top_edge].ty.peer() {
        return false;
    }
    let mut has_peer_edges = false;
    for e in t.edges_in(node) {
        if t.edges[e].ty.peer() {
            has_peer_edges = true;
            continue;
        }
        if t.nodes[t.edges[e].from].is_project_root {
            return true;
        }
    }
    if has_peer_edges {
        for (e, _) in peer_entry_sets(t, node) {
            if t.nodes[t.edges[e].from].is_project_root {
                return true;
            }
        }
    }
    false
}

fn eresolve(t: &Tree, edge: EdgeId, dep: Id) -> String {
    let e = &t.edges[edge];
    format!(
        "ERESOLVE: could not resolve {}@{} (wanted by {}) with {}@{}",
        e.name,
        e.spec,
        if t.nodes[e.from].is_project_root {
            "the project".to_string()
        } else {
            t.location(e.from)
        },
        t.package_name(dep).unwrap_or(&t.nodes[dep].name),
        t.version(dep).unwrap_or("?")
    )
}
