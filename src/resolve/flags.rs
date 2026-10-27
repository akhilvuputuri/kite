//! arborist calc-dep-flags.js, ported step for step (including treeverse's
//! stack order), because the result depends on visiting order.

use std::collections::HashSet;

use super::tree::{EdgeType, Id, Tree};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Flag {
    Extraneous,
    Dev,
    Peer,
    DevOptional,
    Optional,
}

fn get(t: &Tree, n: Id, f: Flag) -> bool {
    let node = &t.nodes[n];
    match f {
        Flag::Extraneous => node.extraneous,
        Flag::Dev => node.dev,
        Flag::Peer => node.peer,
        Flag::DevOptional => node.dev_optional,
        Flag::Optional => node.optional,
    }
}

fn set(t: &mut Tree, n: Id, f: Flag, v: bool) {
    let node = &mut t.nodes[n];
    match f {
        Flag::Extraneous => node.extraneous = v,
        Flag::Dev => node.dev = v,
        Flag::Peer => node.peer = v,
        Flag::DevOptional => node.dev_optional = v,
        Flag::Optional => node.optional = v,
    }
}

pub fn reset_all(t: &mut Tree, nodes: &[Id]) {
    for &n in nodes {
        if n == t.root {
            continue;
        }
        for f in [
            Flag::Extraneous,
            Flag::Dev,
            Flag::Optional,
            Flag::DevOptional,
            Flag::Peer,
        ] {
            set(t, n, f, true);
        }
    }
}

pub fn calc_dep_flags(t: &mut Tree, reset_root: bool) {
    t.refresh();
    let root = t.root;
    if reset_root {
        for f in [Flag::Dev, Flag::Optional, Flag::DevOptional, Flag::Peer] {
            set(t, root, f, false);
        }
    }
    let mut stack = vec![root];
    let mut seen: HashSet<Id> = HashSet::new();
    while let Some(node) = stack.pop() {
        if !seen.insert(node) {
            continue;
        }
        let result = step(t, node);
        for e in t.nodes[result].edges_out.clone() {
            if let Some(to) = t.edges[e].to {
                stack.push(to);
            }
        }
    }
}

fn step(t: &mut Tree, node: Id) -> Id {
    t.nodes[node].extraneous = false;
    for f in [
        Flag::Extraneous,
        Flag::Dev,
        Flag::Peer,
        Flag::DevOptional,
        Flag::Optional,
    ] {
        reset_parents(t, node, f);
    }
    if t.nodes[node].is_link {
        let Some(target) = t.nodes[node].target else {
            return node;
        };
        let (dev, opt, devopt, peer) = {
            let n = &t.nodes[node];
            (n.dev, n.optional, n.dev_optional, n.peer)
        };
        let tn = &mut t.nodes[target];
        tn.dev = dev;
        tn.optional = opt;
        tn.dev_optional = devopt;
        tn.peer = peer;
        return step(t, target);
    }
    for e in t.nodes[node].edges_out.clone() {
        let Some(to) = t.edges[e].to else { continue };
        let ty = t.edges[e].ty;
        t.nodes[to].extraneous = false;
        let n = t.nodes[node].clone();
        let (dev, optional, peer) = (ty.dev(), ty.optional(), ty.peer());
        let unset_dev_opt = !n.dev_optional && !n.dev && !n.optional && !dev && !optional;
        let unset_dev = unset_dev_opt || (!n.dev && !dev);
        let unset_opt = unset_dev_opt || (!n.optional && !optional);
        let unset_peer = !n.peer && !peer;
        if unset_peer {
            unset_flag(t, to, Flag::Peer);
        }
        if unset_dev_opt {
            unset_flag(t, to, Flag::DevOptional);
        }
        if unset_dev {
            unset_flag(t, to, Flag::Dev);
        }
        if unset_opt {
            unset_flag(t, to, Flag::Optional);
        }
    }
    node
}

fn reset_parents(t: &mut Tree, node: Id, f: Flag) {
    if get(t, node, f) {
        return;
    }
    let mut p = Some(node);
    while let Some(cur) = p {
        if !(cur == node || get(t, cur, f)) {
            break;
        }
        set(t, cur, f, false);
        p = t.resolve_parent(cur);
    }
}

fn unset_flag(t: &mut Tree, node: Id, f: Flag) {
    if !get(t, node, f) {
        return;
    }
    set(t, node, f, false);
    let mut stack = vec![node];
    let mut seen: HashSet<Id> = HashSet::new();
    while let Some(n) = stack.pop() {
        if !seen.insert(n) {
            continue;
        }
        set(t, n, Flag::Extraneous, false);
        set(t, n, f, false);
        let target = if t.nodes[n].is_link { t.nodes[n].target } else { None };
        if let Some(tg) = target {
            set(t, tg, Flag::Extraneous, false);
            set(t, tg, f, false);
        }
        let from = target.unwrap_or(n);
        for e in t.nodes[from].edges_out.clone() {
            let Some(to) = t.edges[e].to else { continue };
            let ty = t.edges[e].ty;
            if get(t, to, f) && ((f != Flag::Peer && ty == EdgeType::Peer) || ty == EdgeType::Prod) {
                stack.push(to);
            }
        }
    }
}
