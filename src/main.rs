mod bins;
mod collate;
mod config;
mod extract;
mod fetch;
mod graph;
mod install;
mod integrity;
mod json;
mod link;
mod lockfile;
mod lockmeta;
mod platform;
mod pool;
mod resolve;
mod run;
mod scripts;
mod semver;
mod store;

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "\
lockstep — install and run JavaScript packages from package-lock.json

Usage:
  lockstep [options] ci [--omit=dev|optional|peer]... [--ignore-scripts]
  lockstep [options] install [pkg[@spec]...] [-D|-O|-P|--save-peer] [-E] [--lockfile-only]
  lockstep [options] uninstall <pkg>... [--lockfile-only]
  lockstep [options] update [pkg...] [--lockfile-only]
  lockstep [options] run [--silent] [--if-present] [--ignore-scripts] <script> [--] [args...]
  lockstep [options] test [args...]        (also: start)
  lockstep [options] exec <bin> [--] [args...]
  lockstep --version

Options (before or after the command):
  --prefix DIR, -C DIR   Run in DIR instead of the current directory
  --store DIR            Package store (default: $LOCKSTEP_STORE or the user cache)
  --jobs N               Parallel jobs (default: number of CPUs)
  --quiet                Print nothing on success
";

#[derive(Default)]
struct Global {
    prefix: Option<PathBuf>,
    store: Option<PathBuf>,
    jobs: Option<usize>,
    quiet: bool,
}

/// Splits `--flag=value` or takes the next argument as the value.
fn value(arg: &str, rest: &mut std::vec::IntoIter<String>) -> Result<String, String> {
    if let Some((_, v)) = arg.split_once('=') {
        return Ok(v.to_string());
    }
    rest.next().ok_or_else(|| format!("{arg} needs a value"))
}

/// Consumes a global option if `arg` is one. Returns Ok(false) otherwise.
fn global_option(g: &mut Global, arg: &str, rest: &mut std::vec::IntoIter<String>) -> Result<bool, String> {
    match arg.split('=').next().unwrap_or("") {
        "--prefix" | "-C" => g.prefix = Some(PathBuf::from(value(arg, rest)?)),
        "--store" => g.store = Some(PathBuf::from(value(arg, rest)?)),
        "--jobs" | "-j" => {
            let v = value(arg, rest)?;
            g.jobs = Some(v.parse().map_err(|_| format!("invalid --jobs value {v:?}"))?);
        }
        "--quiet" | "-q" => g.quiet = true,
        _ => return Ok(false),
    }
    Ok(true)
}

fn real_main() -> Result<u8, String> {
    install::current_umask();
    let mut args = std::env::args().skip(1).collect::<Vec<_>>().into_iter();
    let mut g = Global::default();
    let command = loop {
        let Some(arg) = args.next() else {
            print!("{USAGE}");
            return Ok(if g.prefix.is_some() { 1 } else { 0 });
        };
        match arg.as_str() {
            "--version" | "-v" => {
                println!("{}", env!("CARGO_PKG_VERSION"));
                return Ok(0);
            }
            "--help" | "-h" | "help" => {
                print!("{USAGE}");
                return Ok(0);
            }
            _ => {}
        }
        if !global_option(&mut g, &arg, &mut args)? {
            if arg.starts_with('-') {
                return Err(format!("unknown option {arg:?}\n\n{USAGE}"));
            }
            break arg;
        }
    };

    match command.as_str() {
        "ci" | "clean-install" => {
            let mut omit = install::Omit::default();
            let mut include = (false, false, false);
            let mut production = false;
            let mut ignore_scripts = false;
            while let Some(arg) = args.next() {
                if global_option(&mut g, &arg, &mut args)? {
                    continue;
                }
                match arg.split('=').next().unwrap_or("") {
                    "--omit" => match value(&arg, &mut args)?.as_str() {
                        "dev" => omit.dev = true,
                        "optional" => omit.optional = true,
                        "peer" => omit.peer = true,
                        other => {
                            return Err(format!("--omit={other} is not supported (dev, optional, peer)"));
                        }
                    },
                    "--include" => match value(&arg, &mut args)?.as_str() {
                        "dev" => include.0 = true,
                        "optional" => include.1 = true,
                        "peer" => include.2 = true,
                        "prod" => {}
                        other => return Err(format!("--include={other} is not supported")),
                    },
                    "--production" => production = true,
                    "--ignore-scripts" => ignore_scripts = true,
                    _ => return Err(format!("unknown argument {arg:?} for ci")),
                }
            }
            let dir = project_dir(&g)?;
            let mut cli = config::Cli::default();
            for (on, name) in [(omit.dev, "dev"), (omit.optional, "optional"), (omit.peer, "peer")] {
                if on {
                    cli.omit.push(name.into());
                }
            }
            for (on, name) in [(include.0, "dev"), (include.1, "optional"), (include.2, "peer")] {
                if on {
                    cli.include.push(name.into());
                }
            }
            cli.production = production;
            let cfg = config::Config::load(&dir, &cli)?;
            install::ci(&install::CiOptions {
                dir,
                omit: install::Omit {
                    dev: cfg.omit_dev,
                    optional: cfg.omit_optional,
                    peer: cfg.omit_peer,
                },
                ignore_scripts: ignore_scripts || cfg.ignore_scripts,
                store: g.store.clone().unwrap_or_else(store::Store::default_root),
                jobs: g.jobs.unwrap_or_else(pool::default_jobs),
                quiet: g.quiet,
            })?;
            Ok(0)
        }
        "install" | "i" | "add" | "uninstall" | "remove" | "rm" | "un" | "r" | "update" | "up" | "upgrade" => {
            let mode = match command.as_str() {
                "install" | "i" | "add" => "install",
                "update" | "up" | "upgrade" => "update",
                _ => "uninstall",
            };
            let mut omit = install::Omit::default();
            let mut ignore_scripts = false;
            let mut lockfile_only = false;
            let mut save_type = None;
            let mut save_exact = false;
            let mut before = None;
            let mut names: Vec<String> = Vec::new();
            while let Some(arg) = args.next() {
                if global_option(&mut g, &arg, &mut args)? {
                    continue;
                }
                match arg.split('=').next().unwrap_or("") {
                    "--omit" => match value(&arg, &mut args)?.as_str() {
                        "dev" => omit.dev = true,
                        "optional" => omit.optional = true,
                        "peer" => omit.peer = true,
                        other => {
                            return Err(format!("--omit={other} is not supported (dev, optional, peer)"));
                        }
                    },
                    "--ignore-scripts" => ignore_scripts = true,
                    "--lockfile-only" | "--package-lock-only" => lockfile_only = true,
                    "-D" | "--save-dev" => save_type = Some(resolve::SaveType::Dev),
                    "-O" | "--save-optional" => save_type = Some(resolve::SaveType::Optional),
                    "-P" | "--save-prod" => save_type = Some(resolve::SaveType::Prod),
                    "--save-peer" => save_type = Some(resolve::SaveType::Peer),
                    "-E" | "--save-exact" => save_exact = true,
                    "--before" => before = Some(value(&arg, &mut args)?),
                    _ if arg.starts_with('-') => return Err(format!("unknown option {arg:?} for {command}")),
                    _ => names.push(arg),
                }
            }
            if mode == "uninstall" && names.is_empty() {
                return Err("uninstall needs at least one package name".into());
            }
            if mode == "update" && names.iter().any(|n| n.chars().skip(1).any(|c| c == '@')) {
                return Err("update takes package names only; use install name@spec to change a range".into());
            }
            let dir = project_dir(&g)?;
            let mut cli = config::Cli::default();
            for (on, name) in [(omit.dev, "dev"), (omit.optional, "optional"), (omit.peer, "peer")] {
                if on {
                    cli.omit.push(name.into());
                }
            }
            let cfg = config::Config::load(&dir, &cli)?;
            let save_exact = save_exact || cfg.save_exact;
            resolve::run(resolve::ResolveOptions {
                ci: install::CiOptions {
                    dir,
                    omit: install::Omit {
                        dev: cfg.omit_dev,
                        optional: cfg.omit_optional,
                        peer: cfg.omit_peer,
                    },
                    ignore_scripts: ignore_scripts || cfg.ignore_scripts,
                    store: g.store.clone().unwrap_or_else(store::Store::default_root),
                    jobs: g.jobs.unwrap_or_else(pool::default_jobs),
                    quiet: g.quiet,
                },
                add: if mode == "install" { names.clone() } else { Vec::new() },
                save_type,
                save_exact,
                rm: if mode == "uninstall" { names.clone() } else { Vec::new() },
                update: if mode == "update" { Some(names) } else { None },
                lockfile_only,
                before,
            })?;
            Ok(0)
        }
        "run" | "run-script" | "test" | "t" | "tst" | "start" => {
            let mut silent = false;
            let mut if_present = false;
            let mut ignore_scripts = false;
            let mut script = match command.as_str() {
                "test" | "t" | "tst" => Some("test".to_string()),
                "start" => Some("start".to_string()),
                _ => None,
            };
            let mut rest: Vec<String> = Vec::new();
            while let Some(arg) = args.next() {
                if script.is_none() {
                    if global_option(&mut g, &arg, &mut args)? {
                        continue;
                    }
                    match arg.as_str() {
                        "--silent" | "-s" => silent = true,
                        "--if-present" => if_present = true,
                        "--ignore-scripts" => ignore_scripts = true,
                        "--" if script.is_none() => {}
                        _ if arg.starts_with('-') && script.is_none() => {
                            return Err(format!("unknown option {arg:?} for run"));
                        }
                        _ if script.is_none() => script = Some(arg),
                        _ => rest.push(arg),
                    }
                    continue;
                }
                rest.push(arg);
            }
            // `run x -- args` and `run x args` both pass args to the script.
            if rest.first().map(String::as_str) == Some("--") {
                rest.remove(0);
            }
            let start = project_dir(&g)?;
            let root = if g.prefix.is_some() {
                start
            } else {
                run::find_package_root(&start).ok_or("no package.json found in this folder or above")?
            };
            run::run_script(&run::RunOptions {
                root,
                script,
                args: rest,
                silent,
                if_present,
                ignore_scripts,
                command: match command.as_str() {
                    "test" | "t" | "tst" => "test",
                    "start" => "start",
                    _ => "run-script",
                },
            })
        }
        "exec" | "x" => {
            let mut bin = None;
            let mut rest: Vec<String> = Vec::new();
            while let Some(arg) = args.next() {
                if bin.is_none() {
                    if global_option(&mut g, &arg, &mut args)? {
                        continue;
                    }
                    if arg == "--" {
                        continue;
                    }
                    if arg.starts_with('-') {
                        return Err(format!("unknown option {arg:?} for exec"));
                    }
                    bin = Some(arg);
                    continue;
                }
                rest.push(arg);
            }
            if rest.first().map(String::as_str) == Some("--") {
                rest.remove(0);
            }
            let bin = bin.ok_or("exec needs a bin name")?;
            run::exec_bin(&project_dir(&g)?, &bin, &rest)
        }
        // Differential testing hooks for harness/fuzz.mjs (not part of the CLI).
        "__debug" => {
            use std::io::{BufRead, Write};
            let mode = args.next().unwrap_or_default();
            let stdin = std::io::stdin();
            let mut out = std::io::BufWriter::new(std::io::stdout());
            let mut lines: Vec<String> = Vec::new();
            for line in stdin.lock().lines() {
                lines.push(line.map_err(|e| e.to_string())?);
            }
            match mode.as_str() {
                "satisfies" => {
                    for line in &lines {
                        let (range, version) = line.split_once('\t').unwrap_or((line, ""));
                        let result = match (semver::Range::parse(range), semver::Version::parse(version)) {
                            (Some(r), Some(v)) => {
                                if r.satisfies(&v) {
                                    "true"
                                } else {
                                    "false"
                                }
                            }
                            (None, _) => "invalid-range",
                            (_, None) => "invalid-version",
                        };
                        writeln!(out, "{result}").map_err(|e| e.to_string())?;
                    }
                }
                "collate" => {
                    lines.sort_by(|a, b| collate::compare(a, b));
                    for line in &lines {
                        writeln!(out, "{line}").map_err(|e| e.to_string())?;
                    }
                }
                other => return Err(format!("unknown __debug mode {other:?}")),
            }
            Ok(0)
        }
        other => Err(format!("unknown command {other:?}\n\n{USAGE}")),
    }
}

fn project_dir(g: &Global) -> Result<PathBuf, String> {
    match &g.prefix {
        Some(p) => std::path::absolute(p).map_err(|e| format!("--prefix {}: {e}", p.display())),
        None => std::env::current_dir().map_err(|e| e.to_string()),
    }
}

fn main() -> ExitCode {
    match real_main() {
        Ok(code) => ExitCode::from(code),
        Err(message) => {
            eprintln!("lockstep: {message}");
            ExitCode::from(1)
        }
    }
}
