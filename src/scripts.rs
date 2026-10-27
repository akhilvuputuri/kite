//! Install lifecycle scripts, following arborist's rebuild.js and npm ci.
//!
//! Dependencies: `preinstall` for all, then bin links, then `install`, then
//! `postinstall`. Packages start in depth-then-path order with at most
//! `cpus - 1` running at once; their output is captured and shown only if
//! the script fails. Only lockfile entries marked `hasInstallScript` have
//! their package.json scripts read. A package with a `binding.gyp` and no
//! install/preinstall script gets the implicit `node-gyp rebuild`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Map, Value};

use crate::pool;
use crate::run;

#[derive(Clone)]
pub struct ScriptNode {
    pub index: usize,
    pub location: String,
    pub cwd: PathBuf,
    pub package: Map<String, Value>,
    pub extra_env: Vec<(String, String)>,
    pub optional: bool,
}

impl ScriptNode {
    pub fn script(&self, event: &str) -> Option<String> {
        let scripts = self.package.get("scripts").and_then(Value::as_object);
        if let Some(s) = scripts.and_then(|s| s.get(event)).and_then(Value::as_str) {
            return Some(s.to_string());
        }
        if event == "install" {
            let has = |k: &str| scripts.is_some_and(|s| s.contains_key(k));
            let gypfile_off = self.package.get("gypfile") == Some(&Value::Bool(false));
            if !has("install") && !has("preinstall") && !gypfile_off && self.cwd.join("binding.gyp").is_file() {
                return Some("node-gyp rebuild".to_string());
            }
        }
        None
    }
}

pub struct Context {
    pub command: &'static str,
    pub node_env_production: bool,
}

/// Runs `event` for every node that has it. Returns the indexes of optional
/// nodes whose script failed; a failing non-optional node is an error.
pub fn run_phase(nodes: &[ScriptNode], event: &str, ctx: &Context) -> Result<Vec<usize>, String> {
    let todo: Vec<(&ScriptNode, String)> = nodes.iter().filter_map(|n| n.script(event).map(|s| (n, s))).collect();
    if todo.is_empty() {
        return Ok(Vec::new());
    }
    let jobs = pool::default_jobs().saturating_sub(1).max(1);
    let results = pool::map(&todo, jobs, |(node, script)| -> Result<Option<usize>, String> {
        let output = spawn_captured(node, event, script, ctx).map_err(|e| format!("{} {event}: {e}", node.location))?;
        if output.status.success() {
            return Ok(None);
        }
        if node.optional {
            return Ok(Some(node.index));
        }
        let mut message = format!(
            "{} {event} script failed ({}): {script}",
            node.location,
            describe(&output.status)
        );
        for stream in [&output.stdout, &output.stderr] {
            let text = String::from_utf8_lossy(stream);
            if !text.trim().is_empty() {
                message.push('\n');
                message.push_str(text.trim_end());
            }
        }
        Err(message)
    })?;
    Ok(results.into_iter().flatten().collect())
}

fn describe(status: &std::process::ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    match (status.code(), status.signal()) {
        (Some(c), _) => format!("exit code {c}"),
        (None, Some(s)) => format!("signal {s}"),
        _ => "unknown status".into(),
    }
}

fn base_command(cwd: &Path, package: &Map<String, Value>, event: &str, script: &str, ctx: &Context) -> Command {
    let env = run::script_env(cwd, package, event, script, ctx.command);
    let shell = std::env::split_paths(&env.path)
        .map(|d| d.join("sh"))
        .find(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("/bin/sh"));
    let mut cmd = Command::new(shell);
    cmd.arg("-c").arg(script).current_dir(cwd).env("PATH", &env.path);
    for (k, v) in &env.vars {
        cmd.env(k, v);
    }
    if ctx.node_env_production {
        cmd.env("NODE_ENV", "production");
    }
    cmd
}

fn spawn_captured(
    node: &ScriptNode,
    event: &str,
    script: &str,
    ctx: &Context,
) -> std::io::Result<std::process::Output> {
    let mut cmd = base_command(&node.cwd, &node.package, event, script, ctx);
    for (k, v) in &node.extra_env {
        cmd.env(k, v);
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.output()
}

/// Root package scripts during ci run in the foreground with a banner.
pub fn run_root(root: &Path, package: &Map<String, Value>, events: &[&str], ctx: &Context) -> Result<(), String> {
    let scripts = package.get("scripts").and_then(Value::as_object);
    for event in events {
        let Some(script) = scripts.and_then(|s| s.get(*event)).and_then(Value::as_str) else {
            continue;
        };
        let id = match (
            package.get("name").and_then(Value::as_str),
            package.get("version").and_then(Value::as_str),
        ) {
            (Some(n), Some(v)) => format!("{n}@{v} {event}"),
            _ => event.to_string(),
        };
        println!("\n> {id}\n> {}\n", script.trim().replace('\n', "\n> "));
        let cmd = base_command(root, package, event, script, ctx);
        let status = run::run_foreground(cmd).map_err(|e| format!("{event}: {e}"))?;
        if !status.success() {
            return Err(format!("root {event} script failed ({})", describe(&status)));
        }
    }
    Ok(())
}
