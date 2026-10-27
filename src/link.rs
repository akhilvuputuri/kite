//! Placing stored package files into `node_modules`.
//!
//! macOS clones the whole package folder with `clonefile` (copy-on-write on
//! APFS). Linux hard-links each file from the store. When neither works (for
//! example across filesystems) the file is copied; that is how the bytes get
//! there, not a different installer. A package that may change its own files
//! (install scripts, bin fixes) gets private copies so the store stays intact.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::extract::{ExtractedFile, dir_mode};

static HARDLINK_FAILED: AtomicBool = AtomicBool::new(false);

/// `before_hard_link` runs once, before the first file is hard-linked from
/// the store, so the store can record that this entry now shares inodes
/// with a node_modules folder.
pub fn materialize(
    src: &Path,
    files: &[ExtractedFile],
    dest: &Path,
    umask: u32,
    private: bool,
    before_hard_link: &dyn Fn() -> io::Result<()>,
) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        if !dest.exists() && clone_tree(src, dest).is_ok() {
            return Ok(());
        }
    }
    let dmode = dir_mode(umask);
    create_dir(dest, dmode)?;
    let mut made: HashSet<PathBuf> = HashSet::new();
    made.insert(dest.to_path_buf());
    let mut marked = false;
    for file in files {
        let target = dest.join(&file.path);
        if let Some(parent) = target.parent() {
            create_dirs(dest, parent, dmode, &mut made)?;
        }
        let source = src.join(&file.path);
        if !private && !HARDLINK_FAILED.load(Ordering::Relaxed) {
            if !marked {
                before_hard_link()?;
                marked = true;
            }
            match fs::hard_link(&source, &target) {
                Ok(()) => continue,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    fs::remove_file(&target)?;
                    if fs::hard_link(&source, &target).is_ok() {
                        continue;
                    }
                }
                Err(_) => HARDLINK_FAILED.store(true, Ordering::Relaxed),
            }
        }
        copy_file(&source, &target, file.mode)?;
    }
    Ok(())
}

fn copy_file(source: &Path, target: &Path, mode: u32) -> io::Result<()> {
    if fs::symlink_metadata(target).is_ok() {
        fs::remove_file(target)?;
    }
    fs::copy(source, target)?;
    fs::set_permissions(target, fs::Permissions::from_mode(mode))
}

fn create_dir(dir: &Path, mode: u32) -> io::Result<()> {
    match fs::create_dir(dir) {
        Ok(()) => fs::set_permissions(dir, fs::Permissions::from_mode(mode)),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

fn create_dirs(root: &Path, dir: &Path, mode: u32, made: &mut HashSet<PathBuf>) -> io::Result<()> {
    if made.contains(dir) {
        return Ok(());
    }
    if let Some(parent) = dir.parent()
        && parent.starts_with(root)
    {
        create_dirs(root, parent, mode, made)?;
    }
    create_dir(dir, mode)?;
    made.insert(dir.to_path_buf());
    Ok(())
}

/// Creates `dir` and missing parents with npm's directory mode.
pub fn mkdirp(dir: &Path, umask: u32) -> io::Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    if let Some(parent) = dir.parent() {
        mkdirp(parent, umask)?;
    }
    create_dir(dir, dir_mode(umask))
}

/// Gives `path` its own inode so changing it cannot change the store.
pub fn make_private(path: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.nlink() <= 1 {
        return Ok(());
    }
    let tmp = path.with_file_name(format!(
        ".{}.lockstep-tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file")
    ));
    fs::copy(path, &tmp)?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(meta.mode() & 0o7777))?;
    fs::rename(&tmp, path)
}

#[cfg(target_os = "macos")]
fn clone_tree(src: &Path, dest: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    unsafe extern "C" {
        fn clonefile(src: *const libc::c_char, dst: *const libc::c_char, flags: u32) -> libc::c_int;
    }
    const CLONE_NOFOLLOW: u32 = 0x0001;
    const CLONE_NOOWNERCOPY: u32 = 0x0002;
    let src = CString::new(src.as_os_str().as_bytes())?;
    let dst = CString::new(dest.as_os_str().as_bytes())?;
    // SAFETY: both arguments are valid NUL-terminated paths.
    let rc = unsafe { clonefile(src.as_ptr(), dst.as_ptr(), CLONE_NOFOLLOW | CLONE_NOOWNERCOPY) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
