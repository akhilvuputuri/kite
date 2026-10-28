//! `kite run`, `kite test`, `kite exec`: package scripts and
//! local binaries, following @npmcli/run-script and libnpmexec.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::sync::atomic::{AtomicI32, Ordering};

use serde_json::{Map, Value};

/// npm's prefix detection: the nearest folder, starting at `start`, that
/// has a package.json or a node_modules folder.
pub fn find_package_root(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        if d.join("package.json").is_file() || d.join("node_modules").is_dir() {
            return Some(d.to_path_buf());
        }
        dir = d.parent();
    }
    None
}

/// @npmcli/promise-spawn `escape.sh`.
pub fn escape_sh(input: &str) -> String {
    if input.is_empty() {
        return "''".to_string();
    }
    let special = |c: char| "\t\n\r \"#$&'()*;<>?\\`|~".contains(c);
    if !input.chars().any(special) {
        return input.to_string();
    }
    let mut result = format!("'{}'", input.replace('\'', "'\\''"));
    // .replace(/^(?:'')+(?!$)/, '')
    let mut leading = 0;
    while result[leading..].starts_with("''") {
        leading += 2;
    }
    if leading == result.len() {
        leading -= 2;
    }
    if leading > 0 {
        result = result[leading..].to_string();
    }
    // .replace(/\\'''/g, `\\'`)
    result.replace("\\'''", "\\'")
}

/// @npmcli/run-script package-envs: name, version, config, engines, bin.
fn package_envs(pkg: &Map<String, Value>, env: &mut Vec<(String, String)>) {
    fn walk(prefix: &str, value: &Value, env: &mut Vec<(String, String)>) {
        match value {
            Value::Null | Value::Bool(false) => env.push((prefix.to_string(), String::new())),
            Value::Array(items) => {
                for (i, item) in items.iter().enumerate() {
                    walk(&format!("{prefix}_{i}"), item, env);
                }
            }
            Value::Object(map) => {
                for (k, v) in map {
                    walk(&format!("{prefix}_{k}"), v, env);
                }
            }
            Value::String(s) => env.push((prefix.to_string(), s.clone())),
            other => env.push((prefix.to_string(), other.to_string())),
        }
    }
    for key in ["name", "version", "config", "engines", "bin"] {
        if let Some(v) = pkg.get(key) {
            walk(&format!("npm_package_{key}"), v, env);
        }
    }
}

/// set-path.js: every ancestor's node_modules/.bin, then the old PATH
/// with duplicates removed.
pub fn script_path(project: &Path, extra_first: &[PathBuf]) -> OsString {
    let mut parts: Vec<OsString> = extra_first.iter().map(|p| p.clone().into_os_string()).collect();
    let mut dir = Some(project);
    while let Some(d) = dir {
        parts.push(d.join("node_modules").join(".bin").into_os_string());
        dir = d.parent();
    }
    let old = std::env::var_os("PATH").unwrap_or_default();
    // The old PATH keeps its order with later duplicates dropped.
    let mut seen: Vec<OsString> = Vec::new();
    for p in std::env::split_paths(&old) {
        let p = p.into_os_string();
        if !seen.contains(&p) {
            seen.push(p);
        }
    }
    parts.extend(seen);
    let mut out = OsString::new();
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            out.push(":");
        }
        out.push(p);
    }
    out
}

fn which(name: &str, path: &OsString) -> Option<PathBuf> {
    for dir in std::env::split_paths(path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

pub struct ScriptEnv {
    pub vars: Vec<(String, String)>,
    pub path: OsString,
}

pub fn script_env(project: &Path, pkg: &Map<String, Value>, event: &str, script: &str, command: &str) -> ScriptEnv {
    let path = script_path(project, &[]);
    let mut vars: Vec<(String, String)> = Vec::new();
    package_envs(pkg, &mut vars);
    vars.push((
        "npm_package_json".into(),
        project.join("package.json").display().to_string(),
    ));
    vars.push(("npm_lifecycle_event".into(), event.to_string()));
    vars.push(("npm_lifecycle_script".into(), script.to_string()));
    vars.push(("npm_command".into(), command.to_string()));
    if let Ok(cwd) = std::env::current_dir() {
        vars.push(("INIT_CWD".into(), cwd.display().to_string()));
    }
    if let Ok(exe) = std::env::current_exe() {
        vars.push(("npm_execpath".into(), exe.display().to_string()));
    }
    if let Some(node) = which("node", &path) {
        vars.push(("NODE".into(), node.display().to_string()));
        vars.push(("npm_node_execpath".into(), node.display().to_string()));
    }
    vars.push((
        "npm_config_user_agent".into(),
        format!(
            "kite/{} {} {}",
            env!("CARGO_PKG_VERSION"),
            crate::platform::Platform::current().os,
            crate::platform::Platform::current().cpu
        ),
    ));
    ScriptEnv { vars, path }
}

static CHILD: AtomicI32 = AtomicI32::new(0);

extern "C" fn forward(sig: libc::c_int) {
    let pid = CHILD.load(Ordering::SeqCst);
    if pid > 0 {
        // SAFETY: kill is async-signal-safe.
        unsafe {
            libc::kill(pid, sig);
        }
    }
}

/// Runs a command in the foreground. SIGINT and SIGTERM are forwarded to the
/// child (signal-manager.js); the child stays in our process group so a
/// group kill still reaches it.
pub fn run_foreground(mut cmd: Command) -> std::io::Result<ExitStatus> {
    let mut child = cmd.spawn()?;
    CHILD.store(child.id() as i32, Ordering::SeqCst);
    // SAFETY: installing a handler that only calls kill().
    let (old_int, old_term) = unsafe {
        (
            libc::signal(libc::SIGINT, forward as *const () as libc::sighandler_t),
            libc::signal(libc::SIGTERM, forward as *const () as libc::sighandler_t),
        )
    };
    let status = child.wait();
    CHILD.store(0, Ordering::SeqCst);
    // SAFETY: restoring the previous handlers.
    unsafe {
        libc::signal(libc::SIGINT, old_int);
        libc::signal(libc::SIGTERM, old_term);
    }
    status
}

/// Exit the way the child did: same code, or die from the same signal
/// (run-script-pkg.js sends the signal back to itself).
pub fn exit_like(status: ExitStatus) -> u8 {
    if let Some(code) = status.code() {
        return code.clamp(0, 255) as u8;
    }
    if let Some(sig) = status.signal() {
        let _ = std::io::stdout().flush();
        // SAFETY: resetting to the default action and re-raising.
        unsafe {
            libc::signal(sig, libc::SIG_DFL);
            libc::raise(sig);
        }
        return (128 + sig).clamp(0, 255) as u8;
    }
    1
}

pub struct RunOptions {
    pub root: PathBuf,
    pub script: Option<String>,
    pub args: Vec<String>,
    pub silent: bool,
    pub if_present: bool,
    pub ignore_scripts: bool,
    pub command: &'static str,
}

fn read_pkg(root: &Path) -> Result<Map<String, Value>, String> {
    crate::install::read_manifest(root)
}

pub fn run_script(opts: &RunOptions) -> Result<u8, String> {
    let pkg = read_pkg(&opts.root)?;
    let scripts: HashMap<String, String> = pkg
        .get("scripts")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();
    let Some(event) = &opts.script else {
        list_scripts(&pkg);
        return Ok(0);
    };
    let main = match scripts.get(event) {
        Some(s) => s.clone(),
        None if event == "start" && opts.root.join("server.js").is_file() => "node server.js".to_string(),
        None if opts.if_present => return Ok(0),
        None => {
            if !opts.silent {
                eprintln!(
                    "kite: Missing script: {event:?} in {}",
                    opts.root.join("package.json").display()
                );
            }
            return Ok(1);
        }
    };
    let mut steps: Vec<(String, String, Vec<String>)> = Vec::new();
    if !opts.ignore_scripts
        && let Some(pre) = scripts.get(&format!("pre{event}"))
    {
        steps.push((format!("pre{event}"), pre.clone(), Vec::new()));
    }
    steps.push((event.clone(), main, opts.args.clone()));
    if !opts.ignore_scripts
        && let Some(post) = scripts.get(&format!("post{event}"))
    {
        steps.push((format!("post{event}"), post.clone(), Vec::new()));
    }
    for (name, script, args) in steps {
        let status = run_one(&opts.root, &pkg, &name, &script, &args, opts.silent, opts.command)
            .map_err(|e| format!("running {name}: {e}"))?;
        let code = exit_like(status);
        if code != 0 {
            return Ok(code);
        }
    }
    Ok(0)
}

fn run_one(
    root: &Path,
    pkg: &Map<String, Value>,
    event: &str,
    script: &str,
    args: &[String],
    silent: bool,
    command: &str,
) -> std::io::Result<ExitStatus> {
    if !silent {
        let id = match (
            pkg.get("name").and_then(Value::as_str),
            pkg.get("version").and_then(Value::as_str),
        ) {
            (Some(n), Some(v)) => format!("{n}@{v} {event}"),
            _ => event.to_string(),
        };
        let mut banner = format!("\n> {id}\n> {}", script.trim().replace('\n', "\n> "));
        if !args.is_empty() {
            banner.push(' ');
            banner.push_str(&args.join(" "));
        }
        let mut out = std::io::stdout();
        writeln!(out, "{banner}\n")?;
        out.flush()?;
    }
    let mut full = script.to_string();
    for a in args {
        full.push(' ');
        full.push_str(&escape_sh(a));
    }
    let env = script_env(root, pkg, event, script, command);
    let shell = which("sh", &env.path).unwrap_or_else(|| PathBuf::from("/bin/sh"));
    let mut cmd = Command::new(shell);
    cmd.arg("-c").arg(&full).current_dir(root).env("PATH", &env.path);
    for (k, v) in &env.vars {
        cmd.env(k, v);
    }
    run_foreground(cmd)
}

fn list_scripts(pkg: &Map<String, Value>) {
    let Some(scripts) = pkg.get("scripts").and_then(Value::as_object) else {
        return;
    };
    let name = pkg.get("name").and_then(Value::as_str).unwrap_or("");
    let version = pkg.get("version").and_then(Value::as_str).unwrap_or("");
    println!("Scripts available in {name}@{version} via `kite run`:");
    for (k, v) in scripts {
        println!("  {k}\n    {}", v.as_str().unwrap_or(""));
    }
}

/// `kite exec <bin> [args]`: runs a binary from the nearest
/// `node_modules/.bin`, walking up from the package root. Never downloads.
/// The binary is executed directly (no shell), so arguments reach it
/// verbatim.
pub fn exec_bin(cwd: &Path, bin: &str, args: &[String]) -> Result<u8, String> {
    if bin.contains('/') {
        return Err(format!("exec takes a bin name, not a path: {bin:?}"));
    }
    let root = find_package_root(cwd).unwrap_or_else(|| cwd.to_path_buf());
    let mut found = None;
    let mut dir = Some(root.as_path());
    while let Some(d) = dir {
        let candidate = d.join("node_modules").join(".bin").join(bin);
        if fs::metadata(&candidate).is_ok() {
            found = Some(candidate);
            break;
        }
        dir = d.parent();
    }
    let Some(path) = found else {
        return Err(format!(
            "{bin:?} is not installed in node_modules/.bin (searched from {}); kite exec never downloads packages",
            root.display()
        ));
    };
    let bin_dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let env_path = script_path(cwd, &[bin_dir]);
    let pkg = read_pkg(&root).unwrap_or_default();
    let env = script_env(&root, &pkg, "npx", bin, "exec");
    let mut cmd = Command::new(&path);
    cmd.args(args).current_dir(cwd).env("PATH", &env_path);
    for (k, v) in &env.vars {
        if !k.starts_with("npm_package_") || k == "npm_package_json" {
            cmd.env(k, v);
        }
    }
    let status = run_foreground(cmd).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(exit_like(status))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_like_promise_spawn() {
        assert_eq!(escape_sh(""), "''");
        assert_eq!(escape_sh("plain-arg_1.2/x=y"), "plain-arg_1.2/x=y");
        assert_eq!(escape_sh("a b"), "'a b'");
        assert_eq!(escape_sh("it's"), "'it'\\''s'");
        assert_eq!(escape_sh("'quoted'"), "\\''quoted'\\'");
        assert_eq!(escape_sh("$HOME"), "'$HOME'");
        assert_eq!(escape_sh("a[b]"), "a[b]");
        assert_eq!(escape_sh("'"), "\\'");
    }
}
