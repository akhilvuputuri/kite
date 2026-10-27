//! A content-addressable store of verified, extracted packages.
//!
//! Layout: `<root>/v1/u<umask>/<algorithm>-<hex digest>/` holding
//! `package/` (the extracted files, modes already applied) and `index`
//! (one `<octal mode> <path>` line per file). An entry only appears under
//! its final name after it is complete, so a present entry is trusted:
//! its whole tarball matched the lockfile integrity before the entry was
//! renamed into place, and its files are checked against the index (mode,
//! size, modification time) before every reuse.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::MetadataExt;
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
pub static DOWNLOAD_US: AtomicU64 = AtomicU64::new(0);
pub static BYTES: AtomicU64 = AtomicU64::new(0);
pub static TAMPERED: AtomicU64 = AtomicU64::new(0);

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

    /// Returns the stored package, downloading and verifying it first if
    /// needed. A stored entry whose files no longer match its index (edited
    /// through a hard link, for example) is discarded and fetched again.
    pub fn ensure(&self, integrity: &Integrity, url: &str) -> Result<StoredPackage, String> {
        let dir = self.entry_dir(integrity);
        if let Some(files) = read_index(&dir.join("index")) {
            // macOS clones are copy-on-write, so only hard-linked (Linux)
            // stores can be changed through node_modules.
            if cfg!(target_os = "macos") || verify(&dir.join("package"), &files) {
                return Ok(StoredPackage {
                    dir: dir.join("package"),
                    files: files.into_iter().map(|f| f.file).collect(),
                    downloaded: false,
                });
            }
            TAMPERED.fetch_add(1, Ordering::Relaxed);
            let _ = fs::remove_dir_all(&dir);
        }
        fetch::check_registry_url(url)?;
        let mut last = String::new();
        for attempt in 0..3 {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_millis(500 * (1 << attempt)));
            }
            match self.download(integrity, url) {
                Ok((dir, files)) => {
                    return Ok(StoredPackage {
                        dir,
                        files,
                        downloaded: true,
                    });
                }
                Err(Failure::Fatal(e)) => return Err(e),
                Err(Failure::Retry(e)) => last = e,
            }
        }
        Err(last)
    }

    /// Streams the tarball through the integrity hash and the unpacker into a
    /// private temporary folder, which becomes the store entry only after the
    /// whole tarball matched the lockfile's integrity.
    fn download(&self, integrity: &Integrity, url: &str) -> Result<(PathBuf, Vec<ExtractedFile>), Failure> {
        let io_err = |e: io::Error| Failure::Retry(format!("{url}: {e}"));
        let final_dir = self.entry_dir(integrity);
        let tmp_root = self.root.join("tmp");
        fs::create_dir_all(&tmp_root).map_err(io_err)?;
        fs::create_dir_all(self.entries_dir()).map_err(io_err)?;
        let tmp = tmp_root.join(format!(
            "{}.{}.{}",
            integrity.store_key(),
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&tmp);
        let cleanup = TmpDir(tmp.clone());
        fs::create_dir(&tmp).map_err(io_err)?;
        let package = tmp.join("package");
        fs::create_dir(&package).map_err(io_err)?;
        fs::set_permissions(
            &package,
            std::os::unix::fs::PermissionsExt::from_mode(extract::dir_mode(self.umask)),
        )
        .map_err(io_err)?;
        let t0 = std::time::Instant::now();
        let body = fetch::open(url).map_err(|e| match e.status {
            Some(s) if s != 408 && s != 429 && s < 500 => Failure::Fatal(e.message),
            _ => Failure::Retry(e.message),
        })?;
        let mut hashing = HashingReader {
            inner: io::BufReader::with_capacity(256 * 1024, body),
            hasher: Some(integrity.hasher()),
            bytes: 0,
        };
        let files = extract::extract_stream(&mut hashing, &package, self.umask).map_err(io_err)?;
        let digest = hashing.hasher.take().expect("hasher").finish();
        DOWNLOAD_US.fetch_add(t0.elapsed().as_micros() as u64, Ordering::Relaxed);
        BYTES.fetch_add(hashing.bytes, Ordering::Relaxed);
        if !integrity.matches(&digest) {
            return Err(Failure::Fatal(format!(
                "integrity mismatch for {url}: the lockfile expects {}",
                integrity.to_sri()
            )));
        }
        let mut index = Vec::new();
        for file in &files {
            let meta = fs::symlink_metadata(package.join(&file.path)).map_err(io_err)?;
            writeln!(
                index,
                "{:o} {} {}.{} {}",
                file.mode,
                meta.len(),
                meta.mtime(),
                meta.mtime_nsec(),
                file.path
            )
            .map_err(io_err)?;
        }
        fs::write(tmp.join("index"), index).map_err(io_err)?;
        match fs::rename(&tmp, &final_dir) {
            Ok(()) => {}
            // Another process stored the same content first.
            Err(_) if final_dir.join("index").exists() => {}
            Err(e) => return Err(io_err(e)),
        }
        drop(cleanup);
        Ok((final_dir.join("package"), files))
    }
}

enum Failure {
    Retry(String),
    Fatal(String),
}

/// Removes a temporary folder when dropped (after a rename it is gone).
struct TmpDir(PathBuf);

impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct HashingReader<R> {
    inner: R,
    hasher: Option<crate::integrity::Hasher>,
    bytes: u64,
}

impl<R: io::Read> io::Read for HashingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if let Some(h) = self.hasher.as_mut() {
            h.update(&buf[..n]);
        }
        self.bytes += n as u64;
        Ok(n)
    }
}

struct IndexedFile {
    file: ExtractedFile,
    size: u64,
    mtime: (i64, i64),
}

/// Every file is still the one that was unpacked: same type, mode, size
/// and modification time.
fn verify(package: &Path, files: &[IndexedFile]) -> bool {
    files
        .iter()
        .all(|f| match fs::symlink_metadata(package.join(&f.file.path)) {
            Ok(m) => {
                m.is_file()
                    && m.mode() & 0o7777 == f.file.mode
                    && m.len() == f.size
                    && (m.mtime(), m.mtime_nsec()) == f.mtime
            }
            Err(_) => false,
        })
}

fn read_index(path: &Path) -> Option<Vec<IndexedFile>> {
    let text = fs::read_to_string(path).ok()?;
    let mut files = Vec::new();
    for line in text.lines() {
        let mut parts = line.splitn(4, ' ');
        let mode = u32::from_str_radix(parts.next()?, 8).ok()?;
        let size: u64 = parts.next()?.parse().ok()?;
        let (secs, nanos) = parts.next()?.split_once('.')?;
        let path = parts.next()?;
        files.push(IndexedFile {
            file: ExtractedFile {
                path: path.to_string(),
                mode,
            },
            size,
            mtime: (secs.parse().ok()?, nanos.parse().ok()?),
        });
    }
    Some(files)
}
