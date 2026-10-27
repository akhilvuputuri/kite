//! Registry metadata: full packuments (npm's resolver uses full documents,
//! not the abbreviated install format), cached on disk with ETag
//! revalidation, fetched in parallel ahead of need, and npm-pick-manifest.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use serde_json::{Map, Value};

use crate::fetch;
use crate::semver::{Range, Version};

/// npm's resolution also checks `engines.npm`; lockstep mirrors npm 10.9.9.
pub const NPM_VERSION: &str = "10.9.9";

type Slot = Result<Arc<Value>, String>;

struct Shared {
    done: Mutex<HashMap<String, Slot>>,
    /// Names requested but not finished (queued or being fetched).
    pending: Mutex<HashSet<String>>,
    queue: Mutex<VecDeque<String>>,
    /// Signals new queue items (paired with `queue`).
    queued: Condvar,
    /// Signals finished fetches (paired with `done`).
    finished: Condvar,
    cache_dir: PathBuf,
}

pub struct Registry {
    shared: Arc<Shared>,
    started: Mutex<bool>,
    pub node_version: Option<Version>,
    pub before: Option<String>,
}

const WORKERS: usize = 24;
/// The registry serves packuments with `cache-control: max-age=300`; like
/// npm's HTTP cache, a copy younger than that is used without a request.
const FRESH_SECS: u64 = 300;

pub fn packument_url(name: &str) -> String {
    format!("{}{}", fetch::REGISTRY, name.replace('/', "%2f"))
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Registry {
    pub fn new(cache_dir: PathBuf, node_version: Option<Version>, before: Option<String>) -> Registry {
        Registry {
            shared: Arc::new(Shared {
                done: Mutex::new(HashMap::new()),
                pending: Mutex::new(HashSet::new()),
                queue: Mutex::new(VecDeque::new()),
                queued: Condvar::new(),
                finished: Condvar::new(),
                cache_dir,
            }),
            started: Mutex::new(false),
            node_version,
            before,
        }
    }

    fn load(cache_dir: &PathBuf, name: &str) -> Result<Value, String> {
        let safe: String = name.replace('/', "%2f");
        let body_path = cache_dir.join(format!("{safe}.json"));
        let meta_path = cache_dir.join(format!("{safe}.etag"));
        let meta = fs::read_to_string(&meta_path).ok();
        let (etag, fetched_at) = match meta.as_deref().and_then(|m| m.split_once('\n')) {
            Some((tag, at)) => (
                Some(tag.to_string()).filter(|t| !t.is_empty()),
                at.trim().parse::<u64>().unwrap_or(0),
            ),
            None => (meta.clone().filter(|t| !t.is_empty()), 0),
        };
        let cached = body_path.exists();
        let read_cached = || -> Result<Value, String> {
            let text = fs::read(&body_path).map_err(|e| format!("{name}: cached packument: {e}"))?;
            serde_json::from_slice(&text).map_err(|e| format!("{name}: cached packument: {e}"))
        };
        if cached
            && now_secs().saturating_sub(fetched_at) < FRESH_SECS
            && let Ok(v) = read_cached()
        {
            return Ok(v);
        }
        let url = packument_url(name);
        let write_meta = |tag: Option<&str>| {
            let _ = fs::write(&meta_path, format!("{}\n{}", tag.unwrap_or(""), now_secs()));
        };
        match fetch::get_revalidate(&url, "application/json", if cached { etag.as_deref() } else { None }) {
            Ok(fetch::Revalidated::NotModified) => {
                write_meta(etag.as_deref());
                read_cached()
            }
            Ok(fetch::Revalidated::Fresh { body, etag }) => {
                let value: Value = serde_json::from_slice(&body).map_err(|e| format!("{name}: packument: {e}"))?;
                let _ = fs::create_dir_all(cache_dir);
                let tmp = cache_dir.join(format!(
                    "{safe}.json.{}.{:?}",
                    std::process::id(),
                    thread::current().id()
                ));
                if fs::write(&tmp, &body).is_ok() && fs::rename(&tmp, &body_path).is_ok() {
                    write_meta(etag.as_deref());
                }
                Ok(value)
            }
            Err(e) if e.status == Some(404) => Err(format!("E404: {name} is not in the npm registry")),
            Err(e) => Err(e.message),
        }
    }

    fn start_workers(&self) {
        let mut started = self.started.lock().unwrap();
        if *started {
            return;
        }
        *started = true;
        for _ in 0..WORKERS {
            let shared = Arc::clone(&self.shared);
            thread::spawn(move || {
                loop {
                    let name = {
                        let mut q = shared.queue.lock().unwrap();
                        loop {
                            if let Some(n) = q.pop_front() {
                                break n;
                            }
                            q = shared.queued.wait(q).unwrap();
                        }
                    };
                    let result = Registry::load(&shared.cache_dir, &name).map(Arc::new);
                    shared.done.lock().unwrap().insert(name.clone(), result);
                    shared.pending.lock().unwrap().remove(&name);
                    shared.finished.notify_all();
                }
            });
        }
    }

    fn enqueue(&self, name: &str, front: bool) {
        if self.shared.done.lock().unwrap().contains_key(name) {
            return;
        }
        if !self.shared.pending.lock().unwrap().insert(name.to_string()) {
            if front {
                // Move an already-queued name to the front.
                let mut q = self.shared.queue.lock().unwrap();
                if let Some(i) = q.iter().position(|n| n == name) {
                    let n = q.remove(i).unwrap();
                    q.push_front(n);
                }
            }
            return;
        }
        self.start_workers();
        let mut q = self.shared.queue.lock().unwrap();
        if front {
            q.push_front(name.to_string());
        } else {
            q.push_back(name.to_string());
        }
        drop(q);
        self.shared.queued.notify_all();
    }

    /// Queues `names` for fetching in the background.
    pub fn prefetch(&self, names: impl IntoIterator<Item = String>) {
        for name in names {
            self.enqueue(&name, false);
        }
    }

    pub fn packument(&self, name: &str) -> Result<Arc<Value>, String> {
        self.enqueue(name, true);
        let mut done = self.shared.done.lock().unwrap();
        loop {
            if let Some(r) = done.get(name) {
                return r.clone();
            }
            done = self.shared.finished.wait(done).unwrap();
        }
    }

    fn engine_ok(&self, manifest: &Value) -> bool {
        let Some(engines) = manifest.get("engines").and_then(Value::as_object) else {
            return true;
        };
        let check = |want: Option<&Value>, have: Option<Version>| -> bool {
            let Some(want) = want.and_then(Value::as_str) else {
                return true;
            };
            let Some(have) = have else { return true };
            Range::parse(want).is_some_and(|r| r.satisfies_including_prerelease(&have))
        };
        check(engines.get("node"), self.node_version.clone()) && check(engines.get("npm"), Version::parse(NPM_VERSION))
    }

    fn is_before(&self, times: Option<&Map<String, Value>>, version: &str) -> bool {
        let (Some(before), Some(times)) = (&self.before, times) else {
            return true;
        };
        match times.get(version).and_then(Value::as_str) {
            Some(t) => t <= before.as_str(),
            None => true,
        }
    }

    /// npm-pick-manifest for a registry spec: a tag, an exact version or a
    /// range. Returns the chosen version's manifest.
    pub fn pick(&self, packument: &Value, wanted: &PickSpec) -> Result<Map<String, Value>, String> {
        let name = packument.get("name").and_then(Value::as_str).unwrap_or("?");
        let empty = Map::new();
        let versions = packument.get("versions").and_then(Value::as_object).unwrap_or(&empty);
        let tags = packument.get("dist-tags").and_then(Value::as_object);
        let times = if self.before.is_some() {
            packument.get("time").and_then(Value::as_object)
        } else {
            None
        };
        let get = |v: &str| versions.get(v).and_then(Value::as_object).cloned();
        let range_text = match wanted {
            PickSpec::Tag(tag) => {
                let ver = tags
                    .and_then(|t| t.get(tag))
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("ETARGET: no dist-tag {tag:?} for {name}"))?;
                if self.is_before(times, ver) {
                    return get(ver).ok_or_else(|| format!("ETARGET: {name}@{ver} is missing"));
                }
                format!("<={ver}")
            }
            PickSpec::Version(v) => {
                let clean = Version::parse(v).map(|v| v.to_string()).unwrap_or_else(|| v.clone());
                if !self.is_before(times, &clean) {
                    return Err(format!("ETARGET: {name}@{v} is newer than --before"));
                }
                return get(&clean).ok_or_else(|| format!("ETARGET: no version {v} of {name}"));
            }
            PickSpec::Range(r) => r.clone(),
        };
        let range_text = if range_text.trim().is_empty() {
            "*".to_string()
        } else {
            range_text
        };
        let range = Range::parse(&range_text).ok_or_else(|| format!("invalid range {range_text:?} for {name}"))?;
        // 1. dist-tags.latest when it qualifies.
        if let Some(latest) = tags.and_then(|t| t.get("latest")).and_then(Value::as_str) {
            let satisfies = range_text.trim() == "*" || Version::parse(latest).is_some_and(|v| range.satisfies(&v));
            if satisfies
                && let Some(m) = versions.get(latest)
                && self.is_before(times, latest)
                && self.engine_ok(m)
                && !deprecated(m)
            {
                return Ok(m.as_object().cloned().unwrap_or_default());
            }
        }
        // 2. Otherwise the best candidate.
        let mut candidates: Vec<(Version, &Value)> = versions
            .iter()
            .filter(|(v, _)| self.is_before(times, v))
            .filter_map(|(v, m)| Version::parse(v).map(|parsed| (parsed, m)))
            .filter(|(v, _)| range.satisfies(v))
            .collect();
        if versions.is_empty() {
            return Err(format!("ENOVERSIONS: no versions of {name} are available"));
        }
        candidates.sort_by(|(va, ma), (vb, mb)| {
            let key = |m: &Value| {
                let engine = self.engine_ok(m);
                let dep = deprecated(m);
                (!dep && engine, engine, !dep)
            };
            key(mb).cmp(&key(ma)).then_with(|| vb.cmp(va))
        });
        candidates
            .first()
            .and_then(|(_, m)| m.as_object().cloned())
            .ok_or_else(|| format!("ETARGET: no matching version of {name} for {range_text:?}"))
    }
}

fn deprecated(m: &Value) -> bool {
    match m.get("deprecated") {
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Bool(b)) => *b,
        _ => false,
    }
}

#[derive(Clone, Debug)]
pub enum PickSpec {
    Tag(String),
    Version(String),
    Range(String),
}

/// The manifest pacote returns and arborist normalises before building a
/// Node: bundled/optional/scripts/funding/bin fixes, plus `_resolved` and
/// `_integrity` from `dist`.
pub fn normalize_manifest(mut m: Map<String, Value>) -> Map<String, Value> {
    if let Some(b) = m.remove("bundledDependencies") {
        m.entry("bundleDependencies").or_insert(b);
    }
    match m.get("bundleDependencies").cloned() {
        Some(Value::Bool(true)) => {
            let names: Vec<Value> = m
                .get("dependencies")
                .and_then(Value::as_object)
                .map(|d| d.keys().map(|k| Value::String(k.clone())).collect())
                .unwrap_or_default();
            m.insert("bundleDependencies".into(), Value::Array(names));
        }
        Some(Value::Bool(false)) => {
            m.insert("bundleDependencies".into(), Value::Array(Vec::new()));
        }
        Some(Value::Object(o)) => {
            let names = o.keys().map(|k| Value::String(k.clone())).collect();
            m.insert("bundleDependencies".into(), Value::Array(names));
        }
        _ => {}
    }
    let optional: Vec<String> = m
        .get("optionalDependencies")
        .and_then(Value::as_object)
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    if !optional.is_empty()
        && let Some(Value::Object(deps)) = m.get_mut("dependencies")
    {
        for name in &optional {
            deps.remove(name);
        }
        if deps.is_empty() {
            m.remove("dependencies");
        }
    }
    if let Some(Value::Object(scripts)) = m.get_mut("scripts") {
        scripts.retain(|_, v| v.is_string());
    }
    if let Some(Value::String(url)) = m.get("funding").cloned() {
        let mut f = Map::new();
        f.insert("url".into(), Value::String(url));
        m.insert("funding".into(), Value::Object(f));
    }
    if m.contains_key("bin") {
        let name = m.get("name").and_then(Value::as_str).map(str::to_string);
        let bins = crate::bins::normalize(name.as_deref(), m.get("bin"));
        if bins.is_empty() {
            m.remove("bin");
        } else {
            let map: Map<String, Value> = bins.into_iter().map(|(k, v)| (k, Value::String(v))).collect();
            m.insert("bin".into(), Value::Object(map));
        }
    }
    let dist = m.get("dist").cloned();
    if let Some(tarball) = dist.as_ref().and_then(|d| d.get("tarball")).and_then(Value::as_str) {
        m.insert("_resolved".into(), Value::String(tarball.to_string()));
    }
    let integrity = dist
        .as_ref()
        .and_then(|d| d.get("integrity"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            dist.as_ref()
                .and_then(|d| d.get("shasum"))
                .and_then(Value::as_str)
                .and_then(sha1_sri)
        });
    if let Some(i) = integrity {
        m.insert("_integrity".into(), Value::String(i));
    }
    m
}

fn sha1_sri(hex: &str) -> Option<String> {
    use base64::Engine;
    if hex.len() != 40 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let bytes: Option<Vec<u8>> = (0..20)
        .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok())
        .collect();
    Some(format!(
        "sha1-{}",
        base64::engine::general_purpose::STANDARD.encode(bytes?)
    ))
}
