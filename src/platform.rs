//! The os/cpu/libc check npm applies to packages (npm-install-checks).

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Platform {
    pub os: String,
    pub cpu: String,
    pub libc: Option<String>,
}

impl Platform {
    /// The platform this binary runs on, named the way Node does
    /// (`process.platform`, `process.arch`, and npm's libc family).
    pub fn current() -> Platform {
        let os = match std::env::consts::OS {
            "macos" => "darwin",
            "windows" => "win32",
            other => other,
        };
        let cpu = match std::env::consts::ARCH {
            "x86_64" => "x64",
            "aarch64" => "arm64",
            "x86" => "ia32",
            "arm" => "arm",
            other => other,
        };
        let libc = if os == "linux" { detect_libc() } else { None };
        Platform {
            os: os.to_string(),
            cpu: cpu.to_string(),
            libc,
        }
    }

    /// npm-install-checks `checkPlatform`: every listed constraint must pass.
    pub fn accepts(&self, os: &[String], cpu: &[String], libc: &[String]) -> bool {
        let os_ok = os.is_empty() || check_list(&self.os, os);
        let cpu_ok = cpu.is_empty() || check_list(&self.cpu, cpu);
        let libc_ok = libc.is_empty()
            || match &self.libc {
                Some(family) => check_list(family, libc),
                None => false,
            };
        os_ok && cpu_ok && libc_ok
    }
}

/// npm-install-checks current-env.js: the host's C library, from the text
/// of /usr/bin/ldd. A static kite binary cannot ask its own runtime, so
/// when ldd is unreadable it looks for the dynamic loaders instead.
fn detect_libc() -> Option<String> {
    if let Ok(bytes) = std::fs::read("/usr/bin/ldd") {
        let text = String::from_utf8_lossy(&bytes);
        if text.contains("musl") {
            return Some("musl".into());
        }
        if text.contains("GNU C Library") {
            return Some("glibc".into());
        }
        return None;
    }
    let has = |dir: &str, prefix: &str| {
        std::fs::read_dir(dir)
            .map(|d| d.flatten().any(|e| e.file_name().to_string_lossy().starts_with(prefix)))
            .unwrap_or(false)
    };
    if has("/lib", "ld-musl-") {
        Some("musl".into())
    } else if has("/lib64", "ld-linux")
        || has("/lib", "ld-linux")
        || has("/lib/x86_64-linux-gnu", "libc.so.6")
        || has("/lib/aarch64-linux-gnu", "libc.so.6")
    {
        Some("glibc".into())
    } else {
        None
    }
}

/// npm-install-checks `checkList`: `any` matches everything, `!x` excludes x,
/// and a list made only of exclusions accepts everything else.
fn check_list(value: &str, list: &[String]) -> bool {
    if list.len() == 1 && list[0] == "any" {
        return true;
    }
    let mut negated = 0;
    let mut matched = false;
    for entry in list {
        if let Some(test) = entry.strip_prefix('!') {
            negated += 1;
            if value == test {
                return false;
            }
        } else {
            matched = matched || value == entry;
        }
    }
    matched || negated == list.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn linux() -> Platform {
        Platform {
            os: "linux".into(),
            cpu: "x64".into(),
            libc: Some("glibc".into()),
        }
    }

    #[test]
    fn lists() {
        assert!(check_list("linux", &v(&["linux"])));
        assert!(!check_list("darwin", &v(&["linux"])));
        assert!(check_list("darwin", &v(&["!win32"])));
        assert!(!check_list("win32", &v(&["!win32"])));
        assert!(check_list("anything", &v(&["any"])));
        assert!(check_list("linux", &v(&["!win32", "linux"])));
        assert!(!check_list("darwin", &v(&["!win32", "linux"])));
    }

    #[test]
    fn libc_only_matches_when_known() {
        let p = linux();
        assert!(p.accepts(&v(&["linux"]), &v(&["x64"]), &v(&["glibc"])));
        assert!(!p.accepts(&v(&["linux"]), &v(&["x64"]), &v(&["musl"])));
        let mac = Platform {
            os: "darwin".into(),
            cpu: "arm64".into(),
            libc: None,
        };
        assert!(!mac.accepts(&[], &[], &v(&["glibc"])));
        assert!(mac.accepts(&v(&["darwin"]), &v(&["arm64"]), &[]));
    }
}
