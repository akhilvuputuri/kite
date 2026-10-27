//! node-semver compatible versions and ranges (loose parsing, no
//! includePrerelease), as npm uses them for dependency specs.

use std::cmp::Ordering;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Ident {
    Num(u64),
    Alpha(String),
}

impl Ord for Ident {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Ident::Num(a), Ident::Num(b)) => a.cmp(b),
            (Ident::Num(_), Ident::Alpha(_)) => Ordering::Less,
            (Ident::Alpha(_), Ident::Num(_)) => Ordering::Greater,
            (Ident::Alpha(a), Ident::Alpha(b)) => a.cmp(b),
        }
    }
}

impl PartialOrd for Ident {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Ident {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ident::Num(n) => write!(f, "{n}"),
            Ident::Alpha(s) => f.write_str(s),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: Vec<Ident>,
}

impl Version {
    pub fn new(major: u64, minor: u64, patch: u64) -> Version {
        Version {
            major,
            minor,
            patch,
            pre: Vec::new(),
        }
    }

    fn with_pre(major: u64, minor: u64, patch: u64, pre: Vec<Ident>) -> Version {
        Version {
            major,
            minor,
            patch,
            pre,
        }
    }

    /// Parses a full version (`1.2.3`, `v1.2.3`, `=1.2.3-beta.1+build`).
    pub fn parse(text: &str) -> Option<Version> {
        let text = text.trim();
        let text = text.strip_prefix('=').unwrap_or(text).trim_start();
        let text = text.strip_prefix('v').unwrap_or(text);
        let partial = Partial::parse(text)?;
        match partial {
            Partial {
                major: Some(major),
                minor: Some(minor),
                patch: Some(patch),
                pre,
                ..
            } => Some(Version::with_pre(major, minor, patch, pre)),
            _ => None,
        }
    }

    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }

    fn triple(&self) -> (u64, u64, u64) {
        (self.major, self.minor, self.patch)
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.triple()
            .cmp(&other.triple())
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => self.pre.cmp(&other.pre),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre.is_empty() {
            let parts: Vec<String> = self.pre.iter().map(|p| p.to_string()).collect();
            write!(f, "-{}", parts.join("."))?;
        }
        Ok(())
    }
}

/// A possibly partial version such as `1`, `1.x`, `1.2.*`.
#[derive(Clone, Debug, Default)]
struct Partial {
    major: Option<u64>,
    minor: Option<u64>,
    patch: Option<u64>,
    pre: Vec<Ident>,
}

fn parse_num(part: &str) -> Option<Option<u64>> {
    match part {
        "x" | "X" | "*" => Some(None),
        _ if !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()) => {
            // node-semver rejects numbers above MAX_SAFE_INTEGER.
            let n: u64 = part.parse().ok()?;
            if n > 9_007_199_254_740_991 { None } else { Some(Some(n)) }
        }
        _ => None,
    }
}

fn parse_pre(text: &str) -> Option<Vec<Ident>> {
    let mut out = Vec::new();
    for part in text.split('.') {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return None;
        }
        if part.bytes().all(|b| b.is_ascii_digit()) {
            // Loose mode accepts leading zeros; numeric value is what counts.
            out.push(Ident::Num(part.parse().ok()?));
        } else {
            out.push(Ident::Alpha(part.to_string()));
        }
    }
    Some(out)
}

impl Partial {
    fn parse(text: &str) -> Option<Partial> {
        if text.is_empty() {
            return Some(Partial::default());
        }
        let (main, _build) = match text.split_once('+') {
            Some((m, b)) => (m, Some(b)),
            None => (text, None),
        };
        // Loose mode allows `1.2.3beta` as well as `1.2.3-beta`.
        let mut core_end = main.len();
        let mut dots = 0;
        for (i, c) in main.char_indices() {
            if c == '.' {
                dots += 1;
                if dots > 2 {
                    core_end = i;
                    break;
                }
                continue;
            }
            if c == '-' || (!c.is_ascii_digit() && !matches!(c, 'x' | 'X' | '*')) {
                core_end = i;
                break;
            }
        }
        let core = &main[..core_end];
        let rest = &main[core_end..];
        let mut parts = core.split('.');
        let major = parse_num(parts.next()?)?;
        let minor = match parts.next() {
            Some(p) => parse_num(p)?,
            None => None,
        };
        let patch = match parts.next() {
            Some(p) => parse_num(p)?,
            None => None,
        };
        let has_minor_text = core.split('.').count() >= 2;
        let has_patch_text = core.split('.').count() >= 3;
        let pre = if rest.is_empty() {
            Vec::new()
        } else {
            // Prerelease only makes sense on a full version.
            if !has_patch_text || patch.is_none() {
                return None;
            }
            let rest = rest.strip_prefix('-').unwrap_or(rest);
            parse_pre(rest)?
        };
        // An x in the middle forces everything after it to x.
        let (minor, patch) = if major.is_none() || minor.is_none() {
            (None, None)
        } else {
            (minor, patch)
        };
        let _ = has_minor_text;
        Some(Partial {
            major,
            minor,
            patch,
            pre,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Comparator {
    op: Op,
    version: Version,
}

impl Comparator {
    fn test(&self, v: &Version) -> bool {
        match self.op {
            Op::Lt => v < &self.version,
            Op::Le => v <= &self.version,
            Op::Gt => v > &self.version,
            Op::Ge => v >= &self.version,
            Op::Eq => v == &self.version,
        }
    }
}

/// One `||`-separated alternative. `None` means it can match nothing.
type Set = Option<Vec<Comparator>>;

#[derive(Clone, Debug)]
pub struct Range {
    sets: Vec<Set>,
}

fn zero_pre() -> Vec<Ident> {
    vec![Ident::Num(0)]
}

fn ge(v: Version) -> Comparator {
    Comparator { op: Op::Ge, version: v }
}

fn lt(v: Version) -> Comparator {
    Comparator { op: Op::Lt, version: v }
}

impl Range {
    /// node-semver `new Range(text, { loose: true })`. Sets in which no
    /// comparator parses are dropped; the range is invalid only when every
    /// set is dropped.
    pub fn parse(text: &str) -> Option<Range> {
        let mut sets = Vec::new();
        for alt in text.split("||") {
            if let Some(set) = parse_set(alt.trim()) {
                sets.push(set);
            }
        }
        if sets.is_empty() { None } else { Some(Range { sets }) }
    }

    pub fn satisfies(&self, v: &Version) -> bool {
        self.sets.iter().any(|set| match set {
            None => false,
            Some(comps) => test_set(comps, v),
        })
    }

    /// `satisfies(v, range, { includePrerelease: true })`.
    pub fn satisfies_including_prerelease(&self, v: &Version) -> bool {
        self.sets.iter().any(|set| match set {
            None => false,
            Some(comps) => comps.iter().all(|c| c.test(v)),
        })
    }
}

fn test_set(comps: &[Comparator], v: &Version) -> bool {
    if !comps.iter().all(|c| c.test(v)) {
        return false;
    }
    if v.is_prerelease() {
        // A prerelease only matches when a comparator in the same set names
        // a prerelease of the same major.minor.patch.
        return comps
            .iter()
            .any(|c| c.version.is_prerelease() && c.version.triple() == v.triple());
    }
    true
}

/// Splits a comparator set into tokens, joining an operator with the
/// version that follows it (`>= 1.2` is one comparator in loose mode).
fn tokens(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut pending_op = String::new();
    for raw in text.split_whitespace() {
        let op_len = raw
            .find(|c: char| !matches!(c, '<' | '>' | '=' | '~' | '^'))
            .unwrap_or(raw.len());
        if op_len == raw.len() {
            pending_op.push_str(raw);
            continue;
        }
        let token = format!("{pending_op}{raw}");
        pending_op.clear();
        out.push(token);
    }
    if !pending_op.is_empty() {
        out.push(pending_op);
    }
    out
}

fn parse_set(text: &str) -> Option<Set> {
    if text.is_empty() {
        return Some(Some(Vec::new()));
    }
    // Hyphen range: `a - b` with spaces around the hyphen.
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() == 3 && words[1] == "-" {
        return hyphen(words[0], words[2]).map(|set| set.map(drop_gte0));
    }
    let mut comps = Vec::new();
    let mut parsed_any = false;
    let mut never = false;
    for token in tokens(text) {
        // Loose mode silently drops comparators that do not parse.
        match parse_comparator(&token) {
            None => {}
            Some(None) => {
                parsed_any = true;
                never = true;
            }
            Some(Some(mut c)) => {
                parsed_any = true;
                comps.append(&mut c);
            }
        }
    }
    if !parsed_any {
        return None;
    }
    if never {
        return Some(None);
    }
    Some(Some(drop_gte0(comps)))
}

/// replaceGTE0: a bare `>=0.0.0` comparator is removed, which matters for
/// prerelease matching.
fn drop_gte0(comps: Vec<Comparator>) -> Vec<Comparator> {
    comps
        .into_iter()
        .filter(|c| !(c.op == Op::Ge && c.version == Version::new(0, 0, 0)))
        .collect()
}

fn strip_v(text: &str) -> &str {
    let text = text.trim();
    text.strip_prefix('v')
        .or_else(|| text.strip_prefix('V'))
        .unwrap_or(text)
}

fn hyphen(from: &str, to: &str) -> Option<Set> {
    let from = Partial::parse(strip_v(from))?;
    let to = Partial::parse(strip_v(to))?;
    let mut comps = Vec::new();
    match from.major {
        None => {}
        Some(major) => match (from.minor, from.patch) {
            (None, _) => comps.push(ge(Version::new(major, 0, 0))),
            (Some(minor), None) => comps.push(ge(Version::new(major, minor, 0))),
            (Some(minor), Some(patch)) => comps.push(ge(Version::with_pre(major, minor, patch, from.pre))),
        },
    }
    match to.major {
        None => {}
        Some(major) => match (to.minor, to.patch) {
            (None, _) => comps.push(lt(Version::with_pre(major + 1, 0, 0, zero_pre()))),
            (Some(minor), None) => comps.push(lt(Version::with_pre(major, minor + 1, 0, zero_pre()))),
            (Some(minor), Some(patch)) => comps.push(Comparator {
                op: Op::Le,
                version: Version::with_pre(major, minor, patch, to.pre),
            }),
        },
    }
    Some(Some(comps))
}

/// Returns Some(None) when the comparator can never match.
fn parse_comparator(token: &str) -> Option<Option<Vec<Comparator>>> {
    let (op, rest) = if let Some(r) = token.strip_prefix("~>") {
        ("~", r)
    } else if let Some(r) = token.strip_prefix(">=") {
        (">=", r)
    } else if let Some(r) = token.strip_prefix("<=") {
        ("<=", r)
    } else if let Some(r) = token.strip_prefix('>') {
        (">", r)
    } else if let Some(r) = token.strip_prefix('<') {
        ("<", r)
    } else if let Some(r) = token.strip_prefix('=') {
        ("=", r)
    } else if let Some(r) = token.strip_prefix('~') {
        ("~", r)
    } else if let Some(r) = token.strip_prefix('^') {
        ("^", r)
    } else {
        ("", token)
    };
    let p = Partial::parse(strip_v(rest.trim_start_matches('=')))?;
    let Partial {
        major,
        minor,
        patch,
        pre,
    } = p;
    let out = match op {
        "~" => match (major, minor, patch) {
            (None, _, _) => vec![],
            (Some(m), None, _) => vec![
                ge(Version::new(m, 0, 0)),
                lt(Version::with_pre(m + 1, 0, 0, zero_pre())),
            ],
            (Some(m), Some(n), None) => {
                vec![
                    ge(Version::new(m, n, 0)),
                    lt(Version::with_pre(m, n + 1, 0, zero_pre())),
                ]
            }
            (Some(m), Some(n), Some(p)) => vec![
                ge(Version::with_pre(m, n, p, pre)),
                lt(Version::with_pre(m, n + 1, 0, zero_pre())),
            ],
        },
        "^" => match (major, minor, patch) {
            (None, _, _) => vec![],
            (Some(m), None, _) => vec![
                ge(Version::new(m, 0, 0)),
                lt(Version::with_pre(m + 1, 0, 0, zero_pre())),
            ],
            (Some(m), Some(n), None) => {
                if m == 0 {
                    vec![
                        ge(Version::new(m, n, 0)),
                        lt(Version::with_pre(m, n + 1, 0, zero_pre())),
                    ]
                } else {
                    vec![
                        ge(Version::new(m, n, 0)),
                        lt(Version::with_pre(m + 1, 0, 0, zero_pre())),
                    ]
                }
            }
            (Some(m), Some(n), Some(p)) => {
                let upper = if m != 0 {
                    Version::with_pre(m + 1, 0, 0, zero_pre())
                } else if n != 0 {
                    Version::with_pre(0, n + 1, 0, zero_pre())
                } else {
                    Version::with_pre(0, 0, p + 1, zero_pre())
                };
                vec![ge(Version::with_pre(m, n, p, pre)), lt(upper)]
            }
        },
        "" | "=" => match (major, minor, patch) {
            (None, _, _) => vec![],
            (Some(m), None, _) => vec![
                ge(Version::new(m, 0, 0)),
                lt(Version::with_pre(m + 1, 0, 0, zero_pre())),
            ],
            (Some(m), Some(n), None) => {
                vec![
                    ge(Version::new(m, n, 0)),
                    lt(Version::with_pre(m, n + 1, 0, zero_pre())),
                ]
            }
            (Some(m), Some(n), Some(p)) => vec![Comparator {
                op: Op::Eq,
                version: Version::with_pre(m, n, p, pre),
            }],
        },
        ">" | ">=" | "<" | "<=" => {
            let full = patch.is_some();
            match (major, minor, patch) {
                (None, _, _) => {
                    // `>*` and `<*` match nothing; `>=*` and `<=*` match anything.
                    return Some(if op == ">" || op == "<" { None } else { Some(vec![]) });
                }
                _ if full => {
                    let v = Version::with_pre(major.unwrap(), minor.unwrap(), patch.unwrap(), pre);
                    let op = match op {
                        ">" => Op::Gt,
                        ">=" => Op::Ge,
                        "<" => Op::Lt,
                        _ => Op::Le,
                    };
                    vec![Comparator { op, version: v }]
                }
                (Some(m), mi, _) => {
                    let n = mi.unwrap_or(0);
                    match op {
                        ">" => match mi {
                            None => vec![ge(Version::new(m + 1, 0, 0))],
                            Some(n) => vec![ge(Version::new(m, n + 1, 0))],
                        },
                        ">=" => vec![ge(Version::new(m, n, 0))],
                        "<" => vec![lt(Version::with_pre(m, n, 0, zero_pre()))],
                        _ => match mi {
                            None => vec![lt(Version::with_pre(m + 1, 0, 0, zero_pre()))],
                            Some(n) => vec![lt(Version::with_pre(m, n + 1, 0, zero_pre()))],
                        },
                    }
                }
            }
        }
        _ => return None,
    };
    Some(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sat(range: &str, version: &str) -> bool {
        Range::parse(range)
            .unwrap_or_else(|| panic!("range {range}"))
            .satisfies(&Version::parse(version).unwrap_or_else(|| panic!("version {version}")))
    }

    #[test]
    fn ordering() {
        let mut vs: Vec<Version> = [
            "1.0.0",
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta.11",
            "1.0.0-beta.2",
            "1.0.0-rc.1",
            "0.9.9",
        ]
        .iter()
        .map(|s| Version::parse(s).unwrap())
        .collect();
        vs.sort();
        let s: Vec<String> = vs.iter().map(|v| v.to_string()).collect();
        assert_eq!(
            s,
            [
                "0.9.9",
                "1.0.0-alpha",
                "1.0.0-alpha.1",
                "1.0.0-alpha.beta",
                "1.0.0-beta.2",
                "1.0.0-beta.11",
                "1.0.0-rc.1",
                "1.0.0"
            ]
        );
    }

    #[test]
    fn ranges() {
        assert!(sat("^1.2.3", "1.9.0"));
        assert!(!sat("^1.2.3", "2.0.0"));
        assert!(!sat("^1.2.3", "2.0.0-beta"));
        assert!(sat("^0.2.3", "0.2.9"));
        assert!(!sat("^0.2.3", "0.3.0"));
        assert!(sat("^0.0.3", "0.0.3"));
        assert!(!sat("^0.0.3", "0.0.4"));
        assert!(sat("~1.2.3", "1.2.9"));
        assert!(!sat("~1.2.3", "1.3.0"));
        assert!(sat("1.x", "1.5.0"));
        assert!(sat("*", "3.0.0"));
        assert!(!sat("*", "3.0.0-rc.1"));
        assert!(sat("", "1.0.0"));
        assert!(sat(">=1.2 <2", "1.5.0"));
        assert!(sat(">= 1.2.0", "1.2.0"));
        assert!(sat("1.2.3 - 2.3", "2.3.9"));
        assert!(!sat("1.2.3 - 2.3", "2.4.0"));
        assert!(sat("^1.2.3-beta.2", "1.2.3-beta.4"));
        assert!(!sat("^1.2.3-beta.2", "1.2.4-beta.2"));
        assert!(sat("1.2.3 || 2.x", "2.1.0"));
        assert!(sat("^16 || ^18 || >=20", "22.1.0"));
        assert!(!sat(">1", "1.9.0"));
        assert!(sat(">1", "2.0.0"));
        assert!(!sat("<1.2", "1.2.0"));
        assert!(sat("<=1.2", "1.2.9"));
        assert!(!sat(">*", "1.0.0"));
        assert!(sat("=1.2.3", "1.2.3"));
        assert!(sat("v1.2.3", "1.2.3"));
        assert!(sat("~>1.2", "1.2.5"));
        assert!(sat("^0.x", "0.9.0"));
        assert!(sat("^0.0.x", "0.0.5"));
        assert!(!sat("^0.0.x", "0.1.0"));
    }

    #[test]
    fn invalid() {
        assert!(Range::parse("latest").is_none());
        assert!(Range::parse("github:a/b").is_none());
        assert!(Version::parse("1.2").is_none());
    }
}

/// A version interval: lower and upper bounds with inclusivity; None is
/// unbounded.
#[derive(Clone, Debug)]
struct Interval {
    lo: Option<(Version, bool)>,
    hi: Option<(Version, bool)>,
}

impl Interval {
    fn is_empty(&self) -> bool {
        match (&self.lo, &self.hi) {
            (Some((l, li)), Some((h, hi))) => l > h || (l == h && !(*li && *hi)),
            _ => false,
        }
    }

    /// Whether `self` lies within `other`.
    fn within(&self, other: &Interval) -> bool {
        let lo_ok = match (&other.lo, &self.lo) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some((ol, oi)), Some((sl, si))) => sl > ol || (sl == ol && (*oi || !*si)),
        };
        let hi_ok = match (&other.hi, &self.hi) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some((oh, oi)), Some((sh, si))) => sh < oh || (sh == oh && (*oi || !*si)),
        };
        lo_ok && hi_ok
    }
}

impl Range {
    fn intervals(&self) -> Vec<Interval> {
        let mut out = Vec::new();
        for set in self.sets.iter().flatten() {
            let mut iv = Interval { lo: None, hi: None };
            for c in set {
                let v = c.version.clone();
                match c.op {
                    Op::Ge | Op::Gt => {
                        let inc = c.op == Op::Ge;
                        let tighter = match &iv.lo {
                            None => true,
                            Some((l, li)) => v > *l || (v == *l && *li && !inc),
                        };
                        if tighter {
                            iv.lo = Some((v, inc));
                        }
                    }
                    Op::Le | Op::Lt => {
                        let inc = c.op == Op::Le;
                        let tighter = match &iv.hi {
                            None => true,
                            Some((h, hi)) => v < *h || (v == *h && *hi && !inc),
                        };
                        if tighter {
                            iv.hi = Some((v, inc));
                        }
                    }
                    Op::Eq => {
                        iv.lo = Some((v.clone(), true));
                        iv.hi = Some((v, true));
                    }
                }
            }
            if !iv.is_empty() {
                out.push(iv);
            }
        }
        out
    }
}

/// `semver.subset(sub, dom)`: every version matching `sub` matches `dom`.
/// Prerelease subtleties are ignored; lockstep only uses this to choose
/// the saved spec of an added dependency.
pub fn subset(sub: &str, dom: &str) -> bool {
    let (Some(s), Some(d)) = (Range::parse(sub), Range::parse(dom)) else {
        return false;
    };
    let dom = d.intervals();
    s.intervals().iter().all(|iv| dom.iter().any(|d| iv.within(d)))
}

#[cfg(test)]
mod subset_tests {
    use super::subset;

    #[test]
    fn subsets() {
        assert!(subset("^3.0.1", "^3.0.0"));
        assert!(!subset("^6.0.3", "~6.0.0"));
        assert!(subset("^1.5.0", "1.x || 2.x"));
        assert!(!subset("^6.0.3", ">=6.0.0 <6.0.3"));
        assert!(subset("^1.2.3", "*"));
    }
}
