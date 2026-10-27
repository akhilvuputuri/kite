//! A content-addressable store of verified, extracted packages.
//!
//! Layout: `<root>/v1/u<umask>/<algorithm>-<hex digest>/` holding
//! `package/` (the extracted files, modes already applied) and `index`
//! (one `<octal mode> <path>` line per file). An entry only appears under
//! its final name after it is complete, so a present entry is trusted:
//! its tarball matched the lockfile integrity before anything was unpacked.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::extract::{self, ExtractedFile};
use crate::fetch;
use crate::integrity::Integrity;

pub struct Store {
    root: PathBuf,
    umask: u32,
}

pub struct StoredPackage {
    pub dir: PathBuf,
    pub files: Vec<ExtractedFile>,
    pub downloaded: bool,
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

impl Store {
    pub fn new(root: PathBuf, umask: u32) -> Store {
        Store { root, umask }
    }

    pub fn default_root() -> PathBuf {
        if let Some(dir) = std::env::var_os("LOCKSTEP_STORE").filter(|v| !v.is_empty()) {
            return PathBuf::from(dir);
        }
        if let Some(dir) = std::env::var_os("XDG_CACHE_HOME").filter(|v| !v.is_empty()) {
            return PathBuf::from(dir).join("lockstep");
        }
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        if cfg!(target_os = "macos") {
            home.join("Library/Caches/lockstep")
        } else {
            home.join(".cache/lockstep")
        }
    }

    fn entries_dir(&self) -> PathBuf {
        self.root.join("v1").join(format!("u{:03o}", self.umask))
    }

    fn entry_dir(&self, integrity: &Integrity) -> PathBuf {
        self.entries_dir().join(integrity.store_key())
    }

    /// Returns the stored package, downloading and verifying it first if needed.
    pub fn ensure(&self, integrity: &Integrity, url: &str) -> Result<StoredPackage, String> {
        let dir = self.entry_dir(integrity);
        if let Some(files) = read_index(&dir.join("index")) {
            return Ok(StoredPackage {
                dir: dir.join("package"),
                files,
                downloaded: false,
            });
        }
        fetch::check_registry_url(url)?;
        let data = fetch::get(url, None).map_err(|e| e.to_string())?;
        let mut hasher = integrity.hasher();
        hasher.update(&data);
        if !integrity.matches(&hasher.finish()) {
            return Err(format!(
                "integrity mismatch for {url}: expected {}, got {}",
                integrity.to_sri(),
                crate::integrity::sha512_sri(&data)
            ));
        }
        self.insert(integrity, &data)
            .map_err(|e| format!("storing {url}: {e}"))
            .map(|(dir, files)| StoredPackage {
                dir,
                files,
                downloaded: true,
            })
    }

    fn insert(&self, integrity: &Integrity, data: &[u8]) -> io::Result<(PathBuf, Vec<ExtractedFile>)> {
        let final_dir = self.entry_dir(integrity);
        let tmp_root = self.root.join("tmp");
        fs::create_dir_all(&tmp_root)?;
        fs::create_dir_all(self.entries_dir())?;
        let tmp = tmp_root.join(format!(
            "{}.{}.{}",
            integrity.store_key(),
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir(&tmp)?;
        let package = tmp.join("package");
        fs::create_dir(&package)?;
        fs::set_permissions(
            &package,
            std::os::unix::fs::PermissionsExt::from_mode(extract::dir_mode(self.umask)),
        )?;
        let files = extract::extract_tgz(data, &package, self.umask)?;
        let mut index = Vec::new();
        for file in &files {
            writeln!(index, "{:o} {}", file.mode, file.path)?;
        }
        fs::write(tmp.join("index"), index)?;
        match fs::rename(&tmp, &final_dir) {
            Ok(()) => {}
            // Another process stored the same content first.
            Err(_) if final_dir.join("index").exists() => {
                let _ = fs::remove_dir_all(&tmp);
            }
            Err(e) => {
                let _ = fs::remove_dir_all(&tmp);
                return Err(e);
            }
        }
        Ok((final_dir.join("package"), files))
    }
}

fn read_index(path: &Path) -> Option<Vec<ExtractedFile>> {
    let text = fs::read_to_string(path).ok()?;
    let mut files = Vec::new();
    for line in text.lines() {
        let (mode, path) = line.split_once(' ')?;
        files.push(ExtractedFile {
            path: path.to_string(),
            mode: u32::from_str_radix(mode, 8).ok()?,
        });
    }
    Some(files)
}
