//! `kite build`: compiles a TypeScript project to JavaScript from its `tsconfig.json`,
//! without type checking.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::Value;

use crate::pool;
use crate::transpile::{self, Names, Output};
use crate::tsconfig::{self, Project, normalize};

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

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn is_declaration(name: &str) -> bool {
    [".d.ts", ".d.mts", ".d.cts"].iter().any(|ext| name.ends_with(ext))
}

/// The output extension for a source file, or why tsc would compile it to something
/// kite build does not write.
fn output_extension(
    project: &Project,
    source: &Path,
    modules: &mut HashMap<PathBuf, bool>,
) -> Result<&'static str, String> {
    let name = file_name(source);
    let shown = source.display();
    if name.ends_with(".mts") {
        return Ok("mjs");
    }
    if name.ends_with(".cts") {
        return Err(format!(
            "{shown}: kite build writes ES modules only, not .cts files; use tsc for this project"
        ));
    }
    if name.ends_with(".tsx") {
        return Err(format!(
            "{shown}: kite build does not compile JSX yet; use tsc for this project"
        ));
    }
    if project.node_modules_format && !package_is_module(source.parent().unwrap_or(Path::new("/")), modules) {
        return Err(format!(
            "{shown}: tsc compiles this file to CommonJS, because its package.json does not say \
             \"type\": \"module\". kite build writes ES modules only; use tsc for this project"
        ));
    }
    Ok("js")
}

/// The files a module specifier could mean, in the order tsc tries them.
fn candidates(base: &Path) -> Vec<PathBuf> {
    let text = base.to_string_lossy();
    let swap = |from: &str, to: &[&str]| -> Option<Vec<PathBuf>> {
        let stem = text.strip_suffix(from)?;
        Some(to.iter().map(|ext| PathBuf::from(format!("{stem}{ext}"))).collect())
    };
    if [".ts", ".tsx", ".mts", ".cts"].iter().any(|ext| text.ends_with(ext)) {
        return vec![base.to_path_buf()];
    }
    swap(".js", &[".ts", ".tsx", ".d.ts"])
        .or_else(|| swap(".jsx", &[".tsx", ".d.ts"]))
        .or_else(|| swap(".mjs", &[".mts", ".d.mts"]))
        .or_else(|| swap(".cjs", &[".cts", ".d.cts"]))
        .unwrap_or_else(|| {
            [
                ".ts",
                ".tsx",
                ".d.ts",
                ".mts",
                ".cts",
                "/index.ts",
                "/index.tsx",
                "/index.d.ts",
            ]
            .iter()
            .map(|ext| PathBuf::from(format!("{text}{ext}")))
            .collect()
        })
}

/// The project source file a specifier in `from` leads to, if it leads to one: a relative
/// path, or a bare name that `paths` or `baseUrl` maps into the project. Packages in
/// `node_modules` are someone else's output and are not followed.
fn resolve(project: &Project, from: &Path, specifier: &str) -> Option<PathBuf> {
    let dir = from.parent().unwrap_or(Path::new("/"));
    let mut bases = Vec::new();
    if specifier.starts_with("./") || specifier.starts_with("../") || specifier == "." || specifier == ".." {
        bases.push(normalize(&dir.join(specifier)));
    } else if Path::new(specifier).is_absolute() {
        bases.push(PathBuf::from(specifier));
    } else {
        for (pattern, targets) in &project.paths {
            let captured = match pattern.split_once('*') {
                Some((before, after)) => specifier
                    .strip_prefix(before)
                    .and_then(|rest| rest.strip_suffix(after))
                    .filter(|_| specifier.len() >= before.len() + after.len()),
                None => (pattern == specifier).then_some(""),
            };
            if let Some(captured) = captured {
                bases.extend(
                    targets
                        .iter()
                        .map(|t| PathBuf::from(t.to_string_lossy().replacen('*', captured, 1))),
                );
            }
        }
        if let Some(base_url) = &project.base_url {
            bases.push(base_url.join(specifier));
        }
    }
    bases
        .iter()
        .flat_map(|base| candidates(base))
        .find(|candidate| candidate.is_file())
        .filter(|found| !found.components().any(|part| part.as_os_str() == "node_modules"))
}

/// The folder every path shares.
fn common_dir(paths: &[PathBuf]) -> PathBuf {
    let mut shared: Vec<_> = paths[0].parent().unwrap_or(Path::new("/")).components().collect();
    for path in &paths[1..] {
        let parent: Vec<_> = path.parent().unwrap_or(Path::new("/")).components().collect();
        let same = shared.iter().zip(&parent).take_while(|(a, b)| a == b).count();
        shared.truncate(same);
    }
    shared.iter().collect()
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

struct Compiled {
    text: String,
    output: Output,
    extension: &'static str,
}

/// Compiles the selected files and, as tsc does, every project file they import, however
/// far that leads. Nothing is written here, so a refusal leaves the old build untouched.
fn compile_all(project: &Project, jobs: usize) -> Result<BTreeMap<PathBuf, Compiled>, String> {
    let mut modules = HashMap::new();
    let mut known: BTreeSet<PathBuf> = project.files.iter().cloned().collect();
    let mut round: Vec<PathBuf> = project.files.clone();
    let mut compiled = BTreeMap::new();
    while !round.is_empty() {
        let mut batch = Vec::with_capacity(round.len());
        for source in round.drain(..) {
            let extension = output_extension(project, &source, &mut modules)?;
            batch.push((source, extension));
        }
        let outputs = pool::map(&batch, jobs, |(source, extension)| -> Result<Compiled, String> {
            let text = fs::read_to_string(source).map_err(|e| format!("{}: {e}", source.display()))?;
            let output = transpile::transpile(source, &text, project.options)?;
            Ok(Compiled {
                text,
                output,
                extension,
            })
        })?;
        for ((source, _), result) in batch.into_iter().zip(outputs) {
            let dir = source.parent().unwrap_or(Path::new("/")).to_path_buf();
            let found = result
                .output
                .specifiers
                .iter()
                .filter_map(|specifier| resolve(project, &source, specifier))
                .chain(
                    result
                        .output
                        .references
                        .iter()
                        .map(|path| normalize(&dir.join(path)))
                        .filter(|p| p.is_file()),
                );
            for next in found {
                if !is_declaration(&file_name(&next)) && known.insert(next.clone()) {
                    round.push(next);
                }
            }
            compiled.insert(source, result);
        }
    }
    Ok(compiled)
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
    let compiled = compile_all(&project, options.jobs)?;

    let sources: Vec<PathBuf> = compiled.keys().cloned().collect();
    let root_dir = project.root_dir.clone().unwrap_or_else(|| common_dir(&sources));
    let mut files = Vec::with_capacity(compiled.len());
    for (source, result) in compiled {
        let relative = source.strip_prefix(&root_dir).map_err(|_| {
            format!(
                "{} is not under rootDir {}; rootDir must contain every source file",
                source.display(),
                root_dir.display()
            )
        })?;
        let target = project.out_dir.join(relative).with_extension(result.extension);
        let name = file_name(&target);
        let from = relative_to(target.parent().unwrap_or(Path::new("/")), &source);
        let written = result.output.finish(
            &result.text,
            project.options,
            &Names {
                file: &name,
                source: &from,
            },
        )?;
        files.push((target, written));
    }
    let changed: usize = pool::map(&files, options.jobs, |(target, written)| -> Result<usize, String> {
        let mut changed = usize::from(write_if_changed(target, &written.code)?);
        if let Some(map) = &written.map {
            let mut path = target.clone().into_os_string();
            path.push(".map");
            changed += usize::from(write_if_changed(Path::new(&path), map)?);
        }
        Ok(changed)
    })?
    .into_iter()
    .sum();
    if !options.quiet {
        println!(
            "kite: built {} files to {} ({changed} written) in {:.2}s",
            files.len(),
            project
                .out_dir
                .strip_prefix(&options.start)
                .unwrap_or(&project.out_dir)
                .display(),
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

    const MODULE: (&str, &str) = ("package.json", "{ \"type\": \"module\" }");
    const CONFIG: &str = r#"{ "compilerOptions": { "target": "ES2023", "module": "NodeNext",
        "verbatimModuleSyntax": true, "rootDir": "src", "outDir": "dist", "sourceMap": true },
        "include": ["src/**/*"] }"#;

    #[test]
    fn a_project_builds_into_out_dir_and_an_unchanged_build_rewrites_nothing() {
        let dir = project(
            "ok",
            &[
                MODULE,
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
    fn imported_project_files_are_compiled_even_when_include_leaves_them_out() {
        let dir = project(
            "follow",
            &[
                MODULE,
                (
                    "tsconfig.json",
                    r#"{ "compilerOptions": { "target": "ES2022", "module": "NodeNext", "isolatedModules": true,
                        "outDir": "dist", "baseUrl": ".", "paths": { "@lib/*": ["lib/*"] } },
                        "files": ["src/index.ts"], "exclude": ["src/excluded.ts"] }"#,
                ),
                (
                    "src/index.ts",
                    "/// <reference path=\"./referenced.ts\" />\nimport { a } from \"./util.js\";\nimport type { T } from \"./only-type.js\";\n\
                     import { m } from \"@lib/mapped\";\nimport { b } from \"lib/by-base-url.js\";\nimport { e } from \"./excluded.js\";\n\
                     import fs from \"node:fs\";\nimport { d } from \"./declared.js\";\nexport const x: T = [a, m, b, e, fs, d];\n",
                ),
                (
                    "src/util.ts",
                    "import { c } from \"./chain.js\";\nexport const a = c;\n",
                ),
                ("src/chain.ts", "export const c = 1;\n"),
                ("src/only-type.ts", "export type T = unknown[];\n"),
                ("src/excluded.ts", "export const e = 1;\n"),
                ("src/referenced.ts", "var referenced = 1;\n"),
                ("src/declared.d.ts", "export declare const d: number;\n"),
                ("src/unused.ts", "export const unused = 1;\n"),
                ("lib/mapped.ts", "export const m = 1;\n"),
                ("lib/by-base-url.ts", "export const b = 1;\n"),
            ],
        );
        assert_eq!(run(&dir, None), Ok(0));
        // rootDir is the folder the compiled sources share, which is the project here.
        for built in [
            "src/index",
            "src/util",
            "src/chain",
            "src/only-type",
            "src/excluded",
            "src/referenced",
            "lib/mapped",
            "lib/by-base-url",
        ] {
            assert!(dir.join(format!("dist/{built}.js")).is_file(), "{built}");
        }
        assert!(!dir.join("dist/src/unused.js").exists());
        assert!(!dir.join("dist/src/declared.js").exists() && !dir.join("dist/src/declared.d.js").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_refused_file_leaves_the_previous_build_alone() {
        let dir = project(
            "atomic",
            &[
                MODULE,
                ("tsconfig.json", CONFIG),
                ("src/a.ts", "export const a = 1;\n"),
                ("src/b.ts", "export const b = 1;\n"),
            ],
        );
        assert_eq!(run(&dir, None), Ok(0));
        fs::write(dir.join("src/a.ts"), "export const a = 2;\n").unwrap();
        fs::write(dir.join("src/b.ts"), "export enum E { A }\n").unwrap();
        assert!(run(&dir, None).unwrap_err().contains("an enum"));
        assert!(fs::read_to_string(dir.join("dist/a.js")).unwrap().contains("a = 1"));
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
                MODULE,
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

        // An import can lead outside rootDir too.
        let led = project(
            "led",
            &[
                MODULE,
                ("tsconfig.json", CONFIG),
                ("src/a.ts", "export { b } from \"../other/b.js\";\n"),
                ("other/b.ts", "export const b = 1;\n"),
            ],
        );
        assert!(run(&led, None).unwrap_err().contains("not under rootDir"));
        assert!(!led.join("dist").exists());
        fs::remove_dir_all(led).unwrap();

        let cts = project(
            "cts",
            &[
                MODULE,
                ("tsconfig.json", CONFIG),
                ("src/a.cts", "export const a = 1;\n"),
            ],
        );
        assert!(run(&cts, None).unwrap_err().contains(".cts"));
        fs::remove_dir_all(cts).unwrap();

        let empty = project("empty", &[MODULE, ("tsconfig.json", CONFIG), ("src/note.txt", "")]);
        assert!(run(&empty, None).unwrap_err().contains("no TypeScript files"));
        fs::remove_dir_all(empty).unwrap();

        let missing = project("none", &[("src/a.ts", "export const a = 1;\n")]);
        assert!(run(&missing, Some(".")).is_err());
        fs::remove_dir_all(missing).unwrap();
    }

    #[test]
    fn specifiers_resolve_the_way_tsc_tries_them() {
        let names = |base: &str| {
            candidates(Path::new(base))
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(names("/p/a.js"), ["/p/a.ts", "/p/a.tsx", "/p/a.d.ts"]);
        assert_eq!(names("/p/a.mjs"), ["/p/a.mts", "/p/a.d.mts"]);
        assert_eq!(names("/p/a.ts"), ["/p/a.ts"]);
        assert_eq!(names("/p/a")[..3], ["/p/a.ts", "/p/a.tsx", "/p/a.d.ts"]);
        assert!(names("/p/a").contains(&"/p/a/index.ts".to_string()));
        assert_eq!(
            relative_to(Path::new("/p/dist/deep"), Path::new("/p/src/deep/b.ts")),
            "../../src/deep/b.ts"
        );
        assert_eq!(relative_to(Path::new("/p/dist"), Path::new("/p/dist/a.ts")), "a.ts");
        assert_eq!(
            common_dir(&[PathBuf::from("/p/src/a.ts"), PathBuf::from("/p/src/deep/b.ts")]),
            Path::new("/p/src")
        );
        assert_eq!(
            common_dir(&[PathBuf::from("/p/src/a.ts"), PathBuf::from("/p/lib/b.ts")]),
            Path::new("/p")
        );
    }
}
