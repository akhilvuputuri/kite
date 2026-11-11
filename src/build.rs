//! `kite build`: compiles a TypeScript project to JavaScript from its `tsconfig.json`,
//! without type checking.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::Value;

use crate::pool;
use crate::transpile::{self, Names};
use crate::tsconfig::{self, Project};

pub struct BuildOptions {
    /// Where the command was run.
    pub start: PathBuf,
    /// `--project`: a `tsconfig.json`, or a folder holding one.
    pub project: Option<PathBuf>,
    pub skip_declarations: bool,
    pub jobs: usize,
    pub quiet: bool,
}

/// `tsconfig.json` in `start` or the nearest folder above it, as `tsc` looks for it.
fn find_config(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .map(|dir| dir.join("tsconfig.json"))
        .find(|path| path.is_file())
}

/// Whether the nearest `package.json` at or above `dir` says `"type": "module"`.
fn package_is_module(dir: &Path, seen: &mut HashMap<PathBuf, bool>) -> bool {
    if let Some(known) = seen.get(dir) {
        return *known;
    }
    let manifest = dir.join("package.json");
    let answer = if manifest.is_file() {
        fs::read_to_string(&manifest)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .is_some_and(|value| value.get("type").and_then(Value::as_str) == Some("module"))
    } else {
        dir.parent().is_some_and(|parent| package_is_module(parent, seen))
    };
    seen.insert(dir.to_path_buf(), answer);
    answer
}

struct Job {
    source: PathBuf,
    target: PathBuf,
}

/// Where each file goes, or why the project cannot be built.
fn plan(project: &Project) -> Result<Vec<Job>, String> {
    let mut modules = HashMap::new();
    let mut jobs = Vec::with_capacity(project.files.len());
    for source in &project.files {
        let name = source
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let shown = source.display();
        let relative = source.strip_prefix(&project.root_dir).map_err(|_| {
            format!(
                "{shown} is not under rootDir {}; rootDir must contain every source file",
                project.root_dir.display()
            )
        })?;
        let extension = if name.ends_with(".mts") {
            "mjs"
        } else if name.ends_with(".cts") {
            return Err(format!(
                "{shown}: kite build writes ES modules only, not .cts files; use tsc for this project"
            ));
        } else if name.ends_with(".tsx") {
            return Err(format!(
                "{shown}: kite build does not compile JSX yet; use tsc for this project"
            ));
        } else {
            if project.node_modules_format
                && !package_is_module(source.parent().unwrap_or(Path::new("/")), &mut modules)
            {
                return Err(format!(
                    "{shown}: tsc compiles this file to CommonJS, because its package.json does not say \
                     \"type\": \"module\". kite build writes ES modules only; use tsc for this project"
                ));
            }
            "js"
        };
        jobs.push(Job {
            source: source.clone(),
            target: project.out_dir.join(relative).with_extension(extension),
        });
    }
    Ok(jobs)
}

/// `to` as seen from the folder `from`, with `/` separators, as tsc writes map sources.
fn relative_to(from: &Path, to: &Path) -> String {
    let from: Vec<_> = from.components().collect();
    let to: Vec<_> = to.components().collect();
    let shared = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> = from[shared..].iter().map(|_| "..".to_string()).collect();
    parts.extend(
        to[shared..]
            .iter()
            .map(|part| part.as_os_str().to_string_lossy().into_owned()),
    );
    parts.join("/")
}

/// Writes `contents` unless the file already holds exactly that, so an unchanged build
/// leaves file times alone.
fn write_if_changed(path: &Path, contents: &str) -> Result<bool, String> {
    if fs::read(path).is_ok_and(|existing| existing == contents.as_bytes()) {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    fs::write(path, contents).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(true)
}

pub fn build(options: &BuildOptions) -> Result<u8, String> {
    let started = Instant::now();
    let config = match &options.project {
        Some(given) => {
            let path = if given.is_absolute() {
                given.clone()
            } else {
                options.start.join(given)
            };
            if path.is_dir() {
                path.join("tsconfig.json")
            } else {
                path
            }
        }
        None => find_config(&options.start)
            .ok_or_else(|| format!("no tsconfig.json found in {} or above", options.start.display()))?,
    };
    let project = tsconfig::load_project(&config, options.skip_declarations)?;
    let jobs = plan(&project)?;
    let written = pool::map(&jobs, options.jobs, |job| -> Result<usize, String> {
        let source = fs::read_to_string(&job.source).map_err(|e| format!("{}: {e}", job.source.display()))?;
        let file = job
            .target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let from = relative_to(job.target.parent().unwrap_or(Path::new("/")), &job.source);
        let names = Names {
            file: &file,
            source: &from,
        };
        let output = transpile::transpile(&job.source, &source, project.options, &names)?;
        let mut changed = usize::from(write_if_changed(&job.target, &output.code)?);
        if let Some(map) = output.map {
            let mut path = job.target.clone().into_os_string();
            path.push(".map");
            changed += usize::from(write_if_changed(Path::new(&path), &map)?);
        }
        Ok(changed)
    })?;
    if !options.quiet {
        println!(
            "kite: built {} files to {} ({} written) in {:.2}s",
            jobs.len(),
            project
                .out_dir
                .strip_prefix(&options.start)
                .unwrap_or(&project.out_dir)
                .display(),
            written.iter().sum::<usize>(),
            started.elapsed().as_secs_f64()
        );
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kite-build-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for (path, text) in files {
            let path = dir.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        fs::canonicalize(dir).unwrap()
    }

    fn run(dir: &Path, project: Option<&str>) -> Result<u8, String> {
        build(&BuildOptions {
            start: dir.to_path_buf(),
            project: project.map(PathBuf::from),
            skip_declarations: false,
            jobs: 2,
            quiet: true,
        })
    }

    const CONFIG: &str = r#"{ "compilerOptions": { "target": "ES2023", "module": "NodeNext",
        "verbatimModuleSyntax": true, "rootDir": "src", "outDir": "dist", "sourceMap": true },
        "include": ["src/**/*"] }"#;

    #[test]
    fn a_project_builds_into_out_dir_and_an_unchanged_build_rewrites_nothing() {
        let dir = project(
            "ok",
            &[
                ("package.json", "{ \"type\": \"module\" }"),
                ("tsconfig.json", CONFIG),
                ("src/a.ts", "export const a: number = 1;\n"),
                (
                    "src/deep/b.mts",
                    "import { a } from \"../a.js\";\nexport const b: number = a;\n",
                ),
                ("src/types.d.ts", "export type T = number;\n"),
            ],
        );
        assert_eq!(run(&dir.join("src/deep"), None), Ok(0));
        let a = fs::read_to_string(dir.join("dist/a.js")).unwrap();
        assert!(
            a.contains("export const a = 1;") && a.ends_with("//# sourceMappingURL=a.js.map"),
            "{a}"
        );
        assert!(
            fs::read_to_string(dir.join("dist/a.js.map"))
                .unwrap()
                .contains("\"sources\":[\"../src/a.ts\"]")
        );
        assert!(
            fs::read_to_string(dir.join("dist/deep/b.mjs.map"))
                .unwrap()
                .contains("\"../../src/deep/b.mts\"")
        );
        assert!(!dir.join("dist/types.js").exists() && !dir.join("dist/types.d.js").exists());

        let before = fs::metadata(dir.join("dist/a.js")).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert_eq!(run(&dir, Some("tsconfig.json")), Ok(0));
        assert_eq!(fs::metadata(dir.join("dist/a.js")).unwrap().modified().unwrap(), before);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn files_tsc_would_compile_differently_stop_the_build() {
        let common = project(
            "cjs",
            &[("tsconfig.json", CONFIG), ("src/a.ts", "export const a = 1;\n")],
        );
        assert!(run(&common, None).unwrap_err().contains("CommonJS"));
        fs::remove_dir_all(common).unwrap();

        let outside = project(
            "outside",
            &[
                ("package.json", "{ \"type\": \"module\" }"),
                (
                    "tsconfig.json",
                    &CONFIG.replace("\"src/**/*\"", "\"src/**/*\", \"other/**/*\""),
                ),
                ("src/a.ts", "export const a = 1;\n"),
                ("other/b.ts", "export const b = 1;\n"),
            ],
        );
        assert!(run(&outside, None).unwrap_err().contains("not under rootDir"));
        fs::remove_dir_all(outside).unwrap();

        let cts = project(
            "cts",
            &[
                ("package.json", "{ \"type\": \"module\" }"),
                ("tsconfig.json", CONFIG),
                ("src/a.cts", "export const a = 1;\n"),
            ],
        );
        assert!(run(&cts, None).unwrap_err().contains(".cts"));
        fs::remove_dir_all(cts).unwrap();

        let missing = project("none", &[("src/a.ts", "export const a = 1;\n")]);
        assert!(run(&missing, Some(".")).is_err());
        fs::remove_dir_all(missing).unwrap();
    }

    #[test]
    fn map_sources_are_relative_with_forward_slashes() {
        assert_eq!(
            relative_to(Path::new("/p/dist/deep"), Path::new("/p/src/deep/b.ts")),
            "../../src/deep/b.ts"
        );
        assert_eq!(relative_to(Path::new("/p/dist"), Path::new("/p/dist/a.ts")), "a.ts");
    }
}
