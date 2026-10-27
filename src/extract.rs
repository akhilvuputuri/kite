//! Unpacking a package tarball the way pacote does (pacote/lib/fetcher.js
//! `#tarxOptions` and `#entryMode`, node-tar with `strip: 1`).
//!
//! - Only file entries are written. Directory entries are dropped by pacote's
//!   filter, so directories are created implicitly with mode 0777 & ~umask.
//!   Symlinks and hard links are skipped.
//! - File mode is `((mode | 0o666) | 0o600) & ~umask`. pacote's own `umask`
//!   option is 0 and `noChmod` leaves the process umask to the OS.
//! - A `.gitignore` is renamed to `.npmignore` unless a `.npmignore` for the
//!   same folder came earlier in the archive, in which case it is skipped.
//! - Paths containing `..` are skipped and absolute paths are made relative,
//!   as node-tar does.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtractedFile {
    /// Path relative to the package folder, `/`-separated.
    pub path: String,
    pub mode: u32,
}

const MAX_FILE: u64 = 1 << 30;
const MAX_PACKAGE: u64 = 4 << 30;

pub fn file_mode(tar_mode: u32, umask: u32) -> u32 {
    ((tar_mode | 0o666) | 0o600) & !umask & 0o7777
}

pub fn dir_mode(umask: u32) -> u32 {
    0o777 & !umask
}

/// Splits off the first path component (node-tar `strip: 1`) and applies
/// node-tar's path safety rules. Returns None when the entry is skipped.
fn strip_path(raw: &str) -> Option<String> {
    let raw = raw.trim_start_matches('/');
    let mut parts = raw.split('/');
    parts.next()?;
    let rest: Vec<&str> = parts.collect();
    if rest.contains(&"..") {
        return None;
    }
    let joined: Vec<&str> = rest.into_iter().filter(|p| !p.is_empty() && *p != ".").collect();
    if joined.is_empty() {
        return None;
    }
    Some(joined.join("/"))
}

#[cfg(test)]
pub fn extract_tgz(data: &[u8], dest: &Path, umask: u32) -> io::Result<Vec<ExtractedFile>> {
    let mut reader = data;
    extract_stream(&mut reader, dest, umask)
}

/// Unpacks a gzipped tarball read from `input`, then reads `input` to the
/// end so that a hash wrapped around it sees every byte.
pub fn extract_stream<R: Read>(input: &mut R, dest: &Path, umask: u32) -> io::Result<Vec<ExtractedFile>> {
    let files = {
        let decoder = GzDecoder::new(&mut *input);
        let mut archive = tar::Archive::new(decoder);
        let files = extract_archive(&mut archive, dest, umask)?;
        let mut decoder = archive.into_inner();
        io::copy(&mut decoder, &mut io::sink())?;
        files
    };
    io::copy(input, &mut io::sink())?;
    Ok(files)
}

fn extract_archive<R: Read>(archive: &mut tar::Archive<R>, dest: &Path, umask: u32) -> io::Result<Vec<ExtractedFile>> {
    archive.set_ignore_zeros(false);
    let mut seen_npmignore: HashSet<String> = HashSet::new();
    let mut made_dirs: HashSet<PathBuf> = HashSet::new();
    let mut files: Vec<ExtractedFile> = Vec::new();
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let dmode = dir_mode(umask);
    let mut total: u64 = 0;

    for entry in archive.entries()? {
        let mut entry = entry?;
        let kind = entry.header().entry_type();
        let is_file = matches!(kind, tar::EntryType::Regular | tar::EntryType::Continuous) || kind.as_byte() == b'\0';
        if !is_file {
            continue;
        }
        let raw = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
        let mut raw_path = raw.replace('\\', "/");
        let base = raw_path.rsplit('/').next().unwrap_or("").to_string();
        if base == ".npmignore" {
            seen_npmignore.insert(raw_path.clone());
        } else if base == ".gitignore" {
            let renamed = format!("{}.npmignore", &raw_path[..raw_path.len() - ".gitignore".len()]);
            if seen_npmignore.contains(&renamed) {
                continue;
            }
            raw_path = renamed;
        }
        let Some(rel) = strip_path(&raw_path) else {
            continue;
        };
        let tar_mode = entry.header().mode().unwrap_or(0o644);
        let mode = file_mode(tar_mode, umask);
        let target = dest.join(&rel);
        if let Some(parent) = target.parent() {
            ensure_dirs(dest, parent, dmode, &mut made_dirs)?;
        }
        // Unpacked bytes are not trusted until the hash is checked, so their
        // size is bounded: 1 GiB per file, 4 GiB per package.
        let size = entry.size();
        total = total.saturating_add(size);
        if size > MAX_FILE || total > MAX_PACKAGE {
            return Err(io::Error::other("package is too large to unpack"));
        }
        write_file(&target, &mut (&mut entry).take(MAX_FILE), mode)?;
        match index.get(&rel) {
            // A later duplicate replaces the content; node-tar keeps the
            // first file's mode because it does not chmod (noChmod).
            Some(_) => {}
            None => {
                index.insert(rel.clone(), files.len());
                files.push(ExtractedFile { path: rel, mode });
            }
        }
    }
    Ok(files)
}

fn ensure_dirs(root: &Path, dir: &Path, mode: u32, made: &mut HashSet<PathBuf>) -> io::Result<()> {
    if dir == root || made.contains(dir) {
        return Ok(());
    }
    if let Some(parent) = dir.parent()
        && parent.starts_with(root)
    {
        ensure_dirs(root, parent, mode, made)?;
    }
    match fs::symlink_metadata(dir) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => {
            // A file entry earlier in the archive sits where a folder is
            // needed now; node-tar replaces it.
            fs::remove_file(dir)?;
            create_dir(dir, mode)?;
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => create_dir(dir, mode)?,
        Err(e) => return Err(e),
    }
    made.insert(dir.to_path_buf());
    Ok(())
}

/// mkdir with npm's directory mode; a chmod is only needed when the
/// process umask would not produce that mode by itself.
fn create_dir(dir: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().mode(0o777).create(dir)?;
    if mode != 0o777 & !crate::install::current_umask() {
        fs::set_permissions(dir, fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

fn write_file(path: &Path, contents: &mut dyn Read, mode: u32) -> io::Result<()> {
    let existing = fs::symlink_metadata(path).ok();
    if let Some(meta) = &existing
        && meta.is_dir()
    {
        fs::remove_dir_all(path)?;
    }
    let mut file: File = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(mode)
        .open(path)?;
    io::copy(contents, &mut file)?;
    // The kernel applies the process umask to `mode`, which already
    // includes it, so no chmod is needed. A file that existed keeps its
    // first mode (node-tar's noChmod).
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn modes() {
        assert_eq!(file_mode(0o644, 0o022), 0o644);
        assert_eq!(file_mode(0o755, 0o022), 0o755);
        assert_eq!(file_mode(0o400, 0o022), 0o644);
        assert_eq!(file_mode(0o700, 0o022), 0o744);
        assert_eq!(file_mode(0o4755, 0o022), 0o4755);
        assert_eq!(file_mode(0o644, 0o002), 0o664);
        assert_eq!(dir_mode(0o022), 0o755);
    }

    #[test]
    fn strips() {
        assert_eq!(strip_path("package/a/b.js").as_deref(), Some("a/b.js"));
        assert_eq!(strip_path("other/x").as_deref(), Some("x"));
        assert_eq!(strip_path("package/"), None);
        assert_eq!(strip_path("package"), None);
        assert_eq!(strip_path("package/../x"), None);
        assert_eq!(strip_path("/package/x").as_deref(), Some("x"));
        assert_eq!(strip_path("package/./a//b").as_deref(), Some("a/b"));
    }

    fn tgz(entries: &[(&str, u32, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, mode, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(*mode);
            header.set_entry_type(tar::EntryType::Regular);
            builder.append_data(&mut header, path, *data).unwrap();
        }
        let tar = builder.into_inner().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&tar).unwrap();
        gz.finish().unwrap()
    }

    #[test]
    fn extracts_with_npm_rules() {
        let dir = std::env::temp_dir().join(format!("lockstep-extract-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let data = tgz(&[
            ("package/package.json", 0o600, b"{}"),
            ("package/bin/cli.js", 0o755, b"#!/usr/bin/env node\n"),
            ("package/lib/.gitignore", 0o644, b"a"),
            ("package/src/.npmignore", 0o644, b"n"),
            ("package/src/.gitignore", 0o644, b"g"),
        ]);
        let files = extract_tgz(&data, &dir, 0o022).unwrap();
        let paths: Vec<_> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            ["package.json", "bin/cli.js", "lib/.npmignore", "src/.npmignore"]
        );
        let mode = |p: &str| fs::metadata(dir.join(p)).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode("package.json"), 0o644);
        assert_eq!(mode("bin/cli.js"), 0o755);
        assert_eq!(mode("bin"), 0o755);
        assert_eq!(fs::read(dir.join("src/.npmignore")).unwrap(), b"n");
        fs::remove_dir_all(&dir).unwrap();
    }
}
