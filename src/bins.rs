//! `node_modules/.bin` links, following bin-links for non-global installs.
//!
//! - `bin` is normalised by npm-normalize-package-bin.
//! - The link goes into the `.bin` of the `node_modules` folder holding the
//!   package, pointing at a relative path to the bin file.
//! - The first package to claim a link path in a run wins (link-gently's
//!   `seen` set); later claims are ignored. Non-top packages clobber stale
//!   links and files left from earlier installs (`force: !top`).
//! - A linked target is chmodded to `0o777 & ~umask` and a CRLF line ending
//!   on its shebang line is rewritten to LF (fix-bin.js).
//! - A missing target file gets no link and no fix.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use crate::link;

/// npm-normalize-package-bin applied to a manifest's `bin` field.
pub fn normalize(name: Option<&str>, bin: Option<&Value>) -> Vec<(String, String)> {
    let pairs: Vec<(String, Value)> = match bin {
        Some(Value::String(target)) => match name {
            Some(name) if !name.is_empty() => vec![(name.to_string(), Value::String(target.clone()))],
            _ => return Vec::new(),
        },
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|v| v.as_str())
            .map(|k| (basename(k).to_string(), Value::String(k.to_string())))
            .collect(),
        Some(Value::Object(map)) => map.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        _ => return Vec::new(),
    };
    let mut out: Vec<(String, String)> = Vec::new();
    for (key, target) in pairs {
        let Value::String(target) = target else { continue };
        let key = key.replace(['\\', ':'], "/");
        let base = normalize_join(basename(&key));
        if base.is_empty() {
            continue;
        }
        let target = normalize_join(&target.replace('\\', "/"));
        if target.is_empty() {
            continue;
        }
        // Later keys with the same normalised name replace earlier ones.
        if let Some(slot) = out.iter_mut().find(|(k, _)| *k == base) {
            slot.1 = target;
        } else {
            out.push((base, target));
        }
    }
    out
}

/// Node's `path.basename` on a `/`-separated path (trailing slashes ignored).
fn basename(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    trimmed.rsplit('/').next().unwrap_or("")
}

/// `path.join('/', p).slice(1)`: resolves `.` and `..` against the root.
fn normalize_join(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    let mut joined = parts.join("/");
    if path.ends_with('/') && !joined.is_empty() {
        joined.push('/');
    }
    joined
}

/// The `node_modules` folder that holds the package at `location`.
pub fn node_modules_of(location: &str) -> Option<&str> {
    let parent = location.rsplit_once('/')?.0;
    if parent.ends_with("node_modules") && (parent == "node_modules" || parent.ends_with("/node_modules")) {
        return Some(parent);
    }
    // Scoped: node_modules/@scope/name
    let grand = parent.rsplit_once('/').map(|(g, _)| g).unwrap_or("");
    Some(grand)
}

fn relative(from_dir: &Path, to: &Path) -> PathBuf {
    let from: Vec<Component> = from_dir.components().collect();
    let to_c: Vec<Component> = to.components().collect();
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

pub struct BinLinker<'a> {
    root: &'a Path,
    umask: u32,
    seen: HashSet<PathBuf>,
}

impl<'a> BinLinker<'a> {
    pub fn new(root: &'a Path, umask: u32) -> Self {
        BinLinker {
            root,
            umask,
            seen: HashSet::new(),
        }
    }

    /// Links the bins of the package installed at `location` (relative to
    /// the project root). `package_dir` is where its files are.
    pub fn link(&mut self, location: &str, bins: &[(String, String)]) -> io::Result<()> {
        let Some(nm) = node_modules_of(location) else {
            return Ok(());
        };
        let bin_dir = self.root.join(nm).join(".bin");
        let pkg_path = self.root.join(location);
        for (name, target) in bins {
            let to = bin_dir.join(name);
            if !self.seen.insert(to.clone()) {
                continue;
            }
            let abs_from = pkg_path.join(target);
            if fs::symlink_metadata(&abs_from).is_err() {
                continue;
            }
            let from = relative(&bin_dir, &abs_from);
            match fs::symlink_metadata(&to) {
                Ok(meta) => {
                    if meta.is_symlink() && fs::read_link(&to)? == from {
                        // Already set up; link-gently skips it, including the fix.
                        continue;
                    }
                    if meta.is_dir() {
                        fs::remove_dir_all(&to)?;
                    } else {
                        fs::remove_file(&to)?;
                    }
                }
                Err(_) => link::mkdirp(&bin_dir, self.umask)?,
            }
            std::os::unix::fs::symlink(&from, &to)?;
            fix_bin(&abs_from, self.umask)?;
        }
        Ok(())
    }
}

/// bin-links fix-bin.js: chmod to `0o777 & ~umask`, then rewrite a CRLF
/// shebang line to LF. The rewrite decodes the file as UTF-8 (lossily),
/// like `readFile(file, 'utf8')`, and replaces it atomically.
pub fn fix_bin(path: &Path, umask: u32) -> io::Result<()> {
    let real = fs::canonicalize(path)?;
    link::make_private(&real)?;
    let mode = 0o777 & !umask;
    fs::set_permissions(&real, fs::Permissions::from_mode(mode))?;
    let mut head = vec![0u8; 2048];
    let n = {
        use std::io::Read;
        let mut file = fs::File::open(&real)?;
        let mut total = 0;
        loop {
            let read = file.read(&mut head[total..])?;
            if read == 0 || total + read == head.len() {
                total += read;
                break;
            }
            total += read;
        }
        total
    };
    if !is_windows_hashbang(&head[..n], &head) {
        return Ok(());
    }
    let bytes = fs::read(&real)?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let Some(line_end) = text.find('\n') else { return Ok(()) };
    if !text.starts_with("#!") || line_end == 0 || text.as_bytes()[line_end - 1] != b'\r' {
        return Ok(());
    }
    let fixed = format!("{}\n{}", &text[..line_end - 1], &text[line_end + 1..]);
    let tmp = real.with_file_name(format!(
        ".{}.lockstep-tmp",
        real.file_name().and_then(|n| n.to_str()).unwrap_or("bin")
    ));
    fs::write(&tmp, fixed)?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(mode))?;
    fs::rename(&tmp, &real)
}

/// `/^#![^\n]+\r\n/.test(buf.toString())` on a zero-filled 2048-byte buffer.
fn is_windows_hashbang(read: &[u8], _buf: &[u8]) -> bool {
    if read.len() < 2 || read[0] != b'#' || read[1] != b'!' {
        return false;
    }
    let text = String::from_utf8_lossy(read);
    let rest = &text[2..];
    match rest.find('\n') {
        Some(i) => i >= 2 && rest.as_bytes()[i - 1] == b'\r',
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalises_like_npm() {
        assert_eq!(
            normalize(Some("@s/tool"), Some(&json!("./bin/x.js"))),
            vec![("tool".into(), "bin/x.js".into())]
        );
        assert_eq!(
            normalize(None, Some(&json!({"a": "../../x", "../b": "y/./z", "c:d": "q"}))),
            vec![
                ("a".into(), "x".into()),
                ("b".into(), "y/z".into()),
                ("d".into(), "q".into())
            ]
        );
        assert!(normalize(Some("x"), Some(&json!({"a": 1}))).is_empty());
    }

    #[test]
    fn bin_dirs() {
        assert_eq!(node_modules_of("node_modules/a"), Some("node_modules"));
        assert_eq!(node_modules_of("node_modules/@s/a"), Some("node_modules"));
        assert_eq!(
            node_modules_of("node_modules/a/node_modules/@s/b"),
            Some("node_modules/a/node_modules")
        );
        assert_eq!(node_modules_of("pkg/node_modules/b"), Some("pkg/node_modules"));
    }

    #[test]
    fn relative_links() {
        assert_eq!(
            relative(
                Path::new("/p/node_modules/.bin"),
                Path::new("/p/node_modules/typescript/bin/tsc")
            ),
            PathBuf::from("../typescript/bin/tsc")
        );
    }

    #[test]
    fn hashbang() {
        assert!(is_windows_hashbang(b"#!/usr/bin/env node\r\nx", &[]));
        assert!(!is_windows_hashbang(b"#!/usr/bin/env node\nx", &[]));
        assert!(!is_windows_hashbang(b"#!\r\n", &[]));
    }
}
