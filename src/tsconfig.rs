//! Reads a `tsconfig.json` the way `tsc` does, as far as `kite build` needs: comments and
//! trailing commas, `extends`, the options that decide what is emitted, and the
//! `files`/`include`/`exclude` rules that decide which files.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde_json::{Map, Value};

use crate::transpile::{Options, SourceMaps};

/// A project ready to compile.
#[derive(Debug)]
pub struct Project {
    /// `rootDir`, when the config sets it. Otherwise tsc uses the folder the sources share.
    pub root_dir: Option<PathBuf>,
    pub out_dir: PathBuf,
    pub options: Options,
    /// `module` is a Node mode, where a file's package decides whether it is an ES module.
    pub node_modules_format: bool,
    /// `baseUrl`, which bare module names may be relative to.
    pub base_url: Option<PathBuf>,
    /// `paths`: each pattern with the absolute patterns it maps to. Both may hold one `*`.
    pub paths: Vec<(String, Vec<PathBuf>)>,
    /// The files `files` and `include` select, absolute and sorted, declaration files
    /// left out. tsc also compiles the project files these import.
    pub files: Vec<PathBuf>,
}

/// A pattern or path together with the folder of the config that wrote it.
type Located = (PathBuf, String);

#[derive(Default)]
struct Raw {
    options: Map<String, Value>,
    /// Folder of the config each path-valued option came from.
    option_dirs: Map<String, Value>,
    files: Option<Vec<Located>>,
    include: Option<Vec<Located>>,
    exclude: Option<Vec<Located>>,
}

/// The index after any whitespace and comments starting at `i`.
fn skip_trivia(bytes: &[u8], mut i: usize) -> usize {
    loop {
        match bytes.get(i) {
            Some(byte) if byte.is_ascii_whitespace() => i += 1,
            Some(b'/') if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            Some(b'/') if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < bytes.len() && !(bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
            }
            _ => return i,
        }
    }
}

/// Turns JSON with comments and trailing commas into JSON.
pub fn strip_jsonc(text: &str) -> String {
    let bytes = text.strip_prefix('\u{feff}').unwrap_or(text).as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                let start = i;
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                i = (i + 1).min(bytes.len());
                out.extend_from_slice(&bytes[start..i]);
            }
            b'/' if matches!(bytes.get(i + 1), Some(b'/' | b'*')) => {
                // A space keeps the tokens on either side apart.
                out.push(b' ');
                i = skip_trivia(bytes, i);
            }
            b',' => {
                // A comma with only whitespace and comments before a closer is dropped.
                if !matches!(bytes.get(skip_trivia(bytes, i + 1)), Some(b'}' | b']')) {
                    out.push(b',');
                }
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8(out).expect("input was UTF-8")
}

/// Resolves `.` and `..` without touching the file system.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

fn extends_path(dir: &Path, spec: &str, from: &Path) -> Result<PathBuf, String> {
    let candidates = |base: PathBuf| {
        let named = base
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        [
            base.clone(),
            base.with_file_name(format!("{named}.json")),
            base.join("tsconfig.json"),
        ]
    };
    let found = |base: PathBuf| candidates(base).into_iter().find(|p| p.is_file());
    if spec.starts_with("./") || spec.starts_with("../") || Path::new(spec).is_absolute() {
        return found(normalize(&dir.join(spec)))
            .ok_or_else(|| format!("{}: cannot find extended config {spec:?}", from.display()));
    }
    for ancestor in dir.ancestors() {
        if let Some(path) = found(ancestor.join("node_modules").join(spec)) {
            return Ok(path);
        }
    }
    Err(format!(
        "{}: cannot find extended config {spec:?} in node_modules",
        from.display()
    ))
}

fn located(dir: &Path, value: &Value, key: &str, from: &Path) -> Result<Vec<Located>, String> {
    value
        .as_array()
        .and_then(|list| {
            list.iter()
                .map(|v| v.as_str().map(|s| (dir.to_path_buf(), s.to_string())))
                .collect()
        })
        .ok_or_else(|| format!("{}: {key:?} must be a list of strings", from.display()))
}

/// `${configDir}` at the start of a path means the folder of the config being built,
/// whichever config in the `extends` chain wrote it.
fn expand(value: &str, config_dir: &Path) -> PathBuf {
    match value.strip_prefix("${configDir}") {
        Some(rest) => config_dir.join(rest.trim_start_matches('/')),
        None => PathBuf::from(value),
    }
}

fn load(path: &Path, depth: usize) -> Result<Raw, String> {
    if depth > 16 {
        return Err(format!(
            "{}: \"extends\" is nested too deeply or circular",
            path.display()
        ));
    }
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let value: Value = serde_json::from_str(&strip_jsonc(&text)).map_err(|e| format!("{}: {e}", path.display()))?;
    let object = value
        .as_object()
        .ok_or_else(|| format!("{}: not a JSON object", path.display()))?;
    let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();

    let mut raw = Raw::default();
    let bases: Vec<String> = match object.get("extends") {
        None => Vec::new(),
        Some(Value::String(one)) => vec![one.clone()],
        Some(Value::Array(many)) => many
            .iter()
            .map(|v| v.as_str().map(str::to_string))
            .collect::<Option<_>>()
            .ok_or_else(|| format!("{}: \"extends\" must be a string or a list of strings", path.display()))?,
        Some(_) => {
            return Err(format!(
                "{}: \"extends\" must be a string or a list of strings",
                path.display()
            ));
        }
    };
    for spec in bases {
        let base = load(&extends_path(&dir, &spec, path)?, depth + 1)?;
        raw.options.extend(base.options);
        raw.option_dirs.extend(base.option_dirs);
        raw.files = base.files.or(raw.files);
        raw.include = base.include.or(raw.include);
        raw.exclude = base.exclude.or(raw.exclude);
    }
    match object.get("compilerOptions") {
        None => {}
        Some(Value::Object(options)) => {
            for (key, value) in options {
                raw.options.insert(key.clone(), value.clone());
                raw.option_dirs
                    .insert(key.clone(), Value::from(dir.to_string_lossy().into_owned()));
            }
        }
        Some(_) => return Err(format!("{}: \"compilerOptions\" must be an object", path.display())),
    }
    for (key, slot) in [
        ("files", &mut raw.files),
        ("include", &mut raw.include),
        ("exclude", &mut raw.exclude),
    ] {
        if let Some(value) = object.get(key) {
            *slot = Some(located(&dir, value, key, path)?);
        }
    }
    Ok(raw)
}

const SOURCE_EXTENSIONS: [&str; 4] = [".ts", ".tsx", ".mts", ".cts"];
/// Folders `**` never descends into, as in tsc.
const NEVER_WALKED: [&str; 3] = ["node_modules", "bower_components", "jspm_packages"];

fn is_source(name: &str) -> bool {
    SOURCE_EXTENSIONS.iter().any(|ext| name.ends_with(ext))
}

fn is_declaration(name: &str) -> bool {
    [".d.ts", ".d.mts", ".d.cts"].iter().any(|ext| name.ends_with(ext))
}

fn has_wildcard(part: &str) -> bool {
    part.contains(['*', '?'])
}

/// `*` and `?` within one path component. As in tsc, a leading `*` matches nothing or a
/// run that does not begin with a dot, and a leading `?` matches one character that is not
/// a dot, so `*.ts` skips `.hidden.ts` while `*.*ts` still reaches `.e.ts`. `dots` lifts
/// that rule, for exclude patterns.
fn component_matches(pattern: &str, name: &str, dots: bool) -> bool {
    fn go(p: &[u8], n: &[u8]) -> bool {
        match p.split_first() {
            None => n.is_empty(),
            Some((b'*', rest)) => (0..=n.len()).any(|i| go(rest, &n[i..])),
            Some((b'?', rest)) => !n.is_empty() && go(rest, &n[1..]),
            Some((c, rest)) => n.first() == Some(c) && go(rest, &n[1..]),
        }
    }
    let (p, n) = (pattern.as_bytes(), name.as_bytes());
    if dots {
        return go(p, n);
    }
    match p.split_first() {
        Some((b'*', rest)) => go(rest, n) || (n.first().is_some_and(|c| *c != b'.') && go(p, n)),
        Some((b'?', _)) => n.first().is_some_and(|c| *c != b'.') && go(p, n),
        _ => go(p, n),
    }
}

fn parts(path: &Path) -> Vec<String> {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect()
}

fn entries(dir: &Path) -> Vec<(String, bool)> {
    let mut found: Vec<(String, bool)> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| {
            let is_dir = fs::metadata(entry.path()).map(|m| m.is_dir()).unwrap_or(false);
            (entry.file_name().to_string_lossy().into_owned(), is_dir)
        })
        .collect();
    found.sort();
    found
}

/// `seen` holds the real paths `**` has already descended into, so a symlink that points
/// back up the tree is walked once, as in tsc.
fn walk_include(dir: &Path, pattern: &[String], out: &mut BTreeSet<PathBuf>, seen: &mut BTreeSet<PathBuf>) {
    match pattern {
        [] => {}
        [first, rest @ ..] if first == "**" => {
            walk_include(dir, rest, out, seen);
            for (name, is_dir) in entries(dir) {
                if is_dir && !name.starts_with('.') && !NEVER_WALKED.contains(&name.as_str()) {
                    let below = dir.join(name);
                    if fs::canonicalize(&below).is_ok_and(|real| seen.insert(real)) {
                        walk_include(&below, pattern, out, seen);
                    }
                }
            }
        }
        [last] => {
            if has_wildcard(last) {
                for (name, is_dir) in entries(dir) {
                    if !is_dir && is_source(&name) && component_matches(last, &name, false) {
                        out.insert(dir.join(name));
                    }
                }
            } else if is_source(last) && dir.join(last).is_file() {
                out.insert(dir.join(last));
            }
        }
        [first, rest @ ..] => {
            if has_wildcard(first) {
                for (name, is_dir) in entries(dir) {
                    if is_dir && component_matches(first, &name, false) {
                        walk_include(&dir.join(name), rest, out, seen);
                    }
                }
            } else {
                walk_include(&dir.join(first), rest, out, seen);
            }
        }
    }
}

/// Whether an exclude pattern matches `path` or one of the folders above it.
fn excluded(pattern: &[String], path: &[String]) -> bool {
    match pattern {
        [] => true,
        [first, rest @ ..] if first == "**" => (0..=path.len()).any(|i| excluded(rest, &path[i..])),
        [first, rest @ ..] => {
            path.first().is_some_and(|name| component_matches(first, name, true)) && excluded(rest, &path[1..])
        }
    }
}

fn select_files(raw: &Raw, config_dir: &Path, out_dir: &Path) -> Vec<PathBuf> {
    let place = |(dir, pattern): &Located| normalize(&dir.join(expand(pattern, config_dir)));
    let mut chosen = BTreeSet::new();
    let default_include = [(config_dir.to_path_buf(), "**/*".to_string())];
    let include: &[Located] = match (&raw.include, &raw.files) {
        (Some(include), _) => include,
        (None, Some(_)) => &[],
        (None, None) => &default_include,
    };
    for entry in include {
        let mut pattern = parts(&place(entry));
        // A last component with no extension and no wildcard names a folder.
        if pattern.last().is_some_and(|last| !last.contains(['.', '*', '?'])) {
            pattern.extend(["**".to_string(), "*".to_string()]);
        }
        let literal = pattern
            .iter()
            .take_while(|part| !has_wildcard(part))
            .count()
            .min(pattern.len() - 1);
        let base: PathBuf = pattern[..literal].iter().collect();
        walk_include(&base, &pattern[literal..], &mut chosen, &mut BTreeSet::new());
    }
    let exclude: Vec<Vec<String>> = match &raw.exclude {
        Some(list) => list.iter().map(|entry| parts(&place(entry))).collect(),
        None => vec![parts(out_dir)],
    };
    chosen.retain(|file| {
        let path = parts(file);
        !exclude.iter().any(|pattern| excluded(pattern, &path))
    });
    for entry in raw.files.iter().flatten() {
        chosen.insert(place(entry));
    }
    chosen
        .into_iter()
        .filter(|file| {
            file.file_name()
                .is_some_and(|name| !is_declaration(&name.to_string_lossy()))
        })
        .collect()
}

/// Loads `config` and decides what `kite build` would write, or says why it cannot.
pub fn load_project(config: &Path, skip_declarations: bool) -> Result<Project, String> {
    let config = std::path::absolute(config).map_err(|e| format!("{}: {e}", config.display()))?;
    let raw = load(&config, 0)?;
    let config_dir = config.parent().unwrap_or(Path::new("/")).to_path_buf();
    let shown = config.display();
    let refuse = |what: &str| format!("{shown}: {what}; use tsc for this project");

    let flag = |key: &str| raw.options.get(key).and_then(Value::as_bool).unwrap_or(false);
    let text = |key: &str| {
        raw.options
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_ascii_lowercase)
    };
    let defined_in = |key: &str| {
        raw.option_dirs
            .get(key)
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .unwrap_or_else(|| config_dir.clone())
    };
    let dir_of = |key: &str| -> Option<PathBuf> {
        let value = raw.options.get(key).and_then(Value::as_str)?;
        Some(normalize(&defined_in(key).join(expand(value, &config_dir))))
    };

    if flag("noEmit") || flag("emitDeclarationOnly") {
        return Err(format!(
            "{shown}: this project emits no JavaScript (noEmit or emitDeclarationOnly)"
        ));
    }
    if (flag("declaration") || flag("composite")) && !skip_declarations {
        return Err(format!(
            "{shown}: this project emits declaration files, which need tsc. \
             Pass --skip-declarations to write only the JavaScript"
        ));
    }
    for (key, why) in [
        ("outFile", "outFile is not supported"),
        ("allowJs", "allowJs is not supported"),
        ("emitBOM", "emitBOM is not supported"),
        (
            "rewriteRelativeImportExtensions",
            "rewriteRelativeImportExtensions is not supported",
        ),
    ] {
        if raw.options.get(key).is_some_and(|v| v.as_bool() != Some(false)) {
            return Err(refuse(why));
        }
    }
    for key in [
        "sourceRoot",
        "mapRoot",
        "importsNotUsedAsValues",
        "preserveValueImports",
    ] {
        if raw.options.contains_key(key) {
            return Err(refuse(&format!("{key} is not supported")));
        }
    }
    if raw.options.get("useDefineForClassFields").and_then(Value::as_bool) == Some(false) {
        return Err(refuse("useDefineForClassFields: false is not supported"));
    }
    if text("newLine").is_some_and(|v| v != "lf") {
        return Err(refuse("only newLine \"lf\" is supported"));
    }
    let target = text("target").unwrap_or_else(|| "es5".to_string());
    if !["es2022", "es2023", "es2024", "esnext"].contains(&target.as_str()) {
        return Err(refuse(
            "kite build lowers no syntax, so compilerOptions.target must be ES2022, ES2023, ES2024 or ESNext",
        ));
    }
    let module = text("module").unwrap_or_else(|| "es2015".to_string());
    let node_modules_format = ["node16", "node18", "node20", "nodenext"].contains(&module.as_str());
    if !node_modules_format && !["es2015", "es6", "es2020", "es2022", "esnext", "preserve"].contains(&module.as_str()) {
        return Err(refuse(&format!(
            "kite build writes ES modules only, not module {module:?}"
        )));
    }
    let verbatim = flag("verbatimModuleSyntax");
    if !verbatim && !flag("isolatedModules") {
        return Err(refuse(
            "kite build compiles each file on its own, so isolatedModules or verbatimModuleSyntax must be on",
        ));
    }
    let source_maps = match (flag("inlineSourceMap"), flag("sourceMap")) {
        (true, _) => SourceMaps::Inline,
        (false, true) => SourceMaps::File,
        (false, false) => SourceMaps::None,
    };

    // `strict` turns alwaysStrict on unless it is set itself.
    let always_strict = raw
        .options
        .get("alwaysStrict")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| flag("strict"));
    let detection = text("moduleDetection").unwrap_or_else(|| "auto".to_string());
    if !["auto", "legacy", "force"].contains(&detection.as_str()) {
        return Err(format!("{shown}: unknown moduleDetection {detection:?}"));
    }
    // Under a Node module setting every file kite build accepts is an ES module by its
    // package or extension, which `auto` counts as a module.
    let force_module = detection == "force" || (detection == "auto" && node_modules_format);

    let out_dir =
        dir_of("outDir").ok_or_else(|| format!("{shown}: compilerOptions.outDir must be set for kite build"))?;
    let base_url = dir_of("baseUrl");
    let mut paths = Vec::new();
    if let Some(Value::Object(map)) = raw.options.get("paths") {
        let base = base_url.clone().unwrap_or_else(|| defined_in("paths"));
        for (pattern, targets) in map {
            let targets = targets
                .as_array()
                .and_then(|list| {
                    list.iter()
                        .map(|v| v.as_str().map(|t| normalize(&base.join(expand(t, &config_dir)))))
                        .collect()
                })
                .ok_or_else(|| format!("{shown}: compilerOptions.paths[{pattern:?}] must be a list of strings"))?;
            paths.push((pattern.clone(), targets));
        }
    }
    let files = select_files(&raw, &config_dir, &out_dir);
    if files.is_empty() {
        return Err(format!(
            "{shown}: no TypeScript files match this project's files and include"
        ));
    }
    Ok(Project {
        root_dir: dir_of("rootDir"),
        out_dir,
        options: Options {
            verbatim_module_syntax: verbatim,
            esnext: target == "esnext",
            always_strict,
            force_module,
            remove_comments: flag("removeComments"),
            source_maps,
            inline_sources: flag("inlineSources"),
        },
        node_modules_format,
        base_url,
        paths,
        files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Temp(PathBuf);

    impl Temp {
        fn new(name: &str) -> Temp {
            let dir = std::env::temp_dir().join(format!("kite-tsconfig-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Temp(fs::canonicalize(dir).unwrap())
        }

        fn write(&self, path: &str, text: &str) {
            let path = self.0.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }

        fn files(&self, config: &str) -> Vec<String> {
            let project = load_project(&self.0.join(config), false).unwrap();
            project
                .files
                .iter()
                .map(|f| f.strip_prefix(&self.0).unwrap().to_string_lossy().into_owned())
                .collect()
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const BASE: &str = r#"{ "compilerOptions": { "target": "ES2023", "module": "NodeNext",
        "isolatedModules": true, "rootDir": "src", "outDir": "dist" }"#;

    #[test]
    fn comments_and_trailing_commas_are_json() {
        let text = "{ // one\n \"a\": \"//not, a comment\", /* two */ \"b\": [1, 2, ], \"c\": \"\\\"}\", }";
        let value: Value = serde_json::from_str(&strip_jsonc(text)).unwrap();
        assert_eq!(
            value,
            serde_json::json!({ "a": "//not, a comment", "b": [1, 2], "c": "\"}" })
        );
    }

    #[test]
    fn include_follows_tscs_wildcard_rules() {
        let t = Temp::new("include");
        for file in [
            "src/a.ts",
            "src/deep/b.mts",
            "src/types.d.ts",
            "src/.hidden/c.ts",
            "src/.dot.ts",
            "src/.e.ts",
            "src/node_modules/x/d.ts",
            "src/note.txt",
            "dist/e.ts",
            "other/f.ts",
        ] {
            t.write(file, "export {};\n");
        }
        t.write("tsconfig.json", &format!("{BASE}, \"include\": [\"src/**/*.ts\"] }}"));
        // `*.ts` does not reach `.mts`, dot names or node_modules; declarations emit nothing.
        assert_eq!(t.files("tsconfig.json"), ["src/a.ts"]);

        // A bare folder means everything under it; the default exclude is outDir.
        t.write("folder.json", &format!("{BASE}, \"include\": [\"src\", \"dist\"] }}"));
        assert_eq!(t.files("folder.json"), ["src/a.ts", "src/deep/b.mts"]);

        // No include and no files: the whole project, minus outDir.
        t.write("all.json", &format!("{BASE} }}"));
        assert_eq!(t.files("all.json"), ["other/f.ts", "src/a.ts", "src/deep/b.mts"]);

        // `files` is exact, is not filtered by exclude, and turns the default include off.
        t.write(
            "files.json",
            &format!("{BASE}, \"files\": [\"src/a.ts\", \"dist/e.ts\"], \"exclude\": [\"dist\"] }}"),
        );
        assert_eq!(t.files("files.json"), ["dist/e.ts", "src/a.ts"]);

        // An exclude pattern covers a folder and everything below it.
        t.write(
            "exclude.json",
            &format!("{BASE}, \"include\": [\"**/*\"], \"exclude\": [\"**/deep\", \"dist\", \"oth*\"] }}"),
        );
        assert_eq!(t.files("exclude.json"), ["src/a.ts"]);

        // A leading `*` may match nothing, so `*.*ts` reaches `.e.ts`, which `*.ts` skips.
        t.write("dots.json", &format!("{BASE}, \"include\": [\"src/*.*ts\"] }}"));
        assert_eq!(t.files("dots.json"), ["src/.dot.ts", "src/.e.ts", "src/a.ts"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_back_up_the_tree_is_walked_once() {
        let t = Temp::new("loop");
        t.write("src/a.ts", "export {};\n");
        std::os::unix::fs::symlink(&t.0, t.0.join("src/again")).unwrap();
        t.write("tsconfig.json", &format!("{BASE}, \"include\": [\"src/**/*\"] }}"));
        let files = t.files("tsconfig.json");
        assert!(files.contains(&"src/a.ts".to_string()), "{files:?}");
        assert!(files.len() < 4, "{files:?}");
    }

    #[test]
    fn extends_merges_options_and_keeps_each_configs_own_folder() {
        let t = Temp::new("extends");
        t.write(
            "base/tsconfig.base.json",
            &format!("{BASE}, \"include\": [\"lib/**/*\"] }}"),
        );
        t.write("base/lib/a.ts", "export {};\n");
        t.write("app/src/b.ts", "export {};\n");
        t.write(
            "app/node_modules/@scope/cfg/tsconfig.json",
            "{ \"compilerOptions\": { \"removeComments\": true } }",
        );
        t.write(
            "app/tsconfig.json",
            "{ \"extends\": [\"@scope/cfg\", \"../base/tsconfig.base\"], \"compilerOptions\": { \"outDir\": \"out\", \"verbatimModuleSyntax\": true } }",
        );
        let project = load_project(&t.0.join("app/tsconfig.json"), false).unwrap();
        // rootDir and include come from the base and stay relative to it; outDir is the app's.
        assert_eq!(project.root_dir, Some(t.0.join("base/src")));
        assert_eq!(project.out_dir, t.0.join("app/out"));
        assert_eq!(project.files, [t.0.join("base/lib/a.ts")]);
        assert!(project.options.remove_comments && project.options.verbatim_module_syntax);
        assert!(project.node_modules_format);

        t.write("loop.json", "{ \"extends\": \"./loop.json\" }");
        assert!(
            load_project(&t.0.join("loop.json"), false)
                .unwrap_err()
                .contains("circular")
        );
        t.write("missing.json", "{ \"extends\": \"nope\" }");
        assert!(
            load_project(&t.0.join("missing.json"), false)
                .unwrap_err()
                .contains("cannot find")
        );
    }

    #[test]
    fn strictness_module_detection_and_config_dir_are_read_as_tsc_reads_them() {
        let t = Temp::new("modes");
        t.write("src/a.ts", "export {};\n");
        let load = |options: &str| {
            t.write("tsconfig.json", &format!("{{ \"compilerOptions\": {{ \"target\": \"es2022\", \"isolatedModules\": true, \"outDir\": \"dist\"{options} }} }}"));
            load_project(&t.0.join("tsconfig.json"), false).unwrap()
        };
        // Defaults: not strict, and under module ES2015 a file is a module only by its contents.
        let plain = load("");
        assert!(!plain.options.always_strict && !plain.options.force_module);
        assert_eq!(plain.root_dir, None);
        assert!(load(", \"strict\": true").options.always_strict);
        assert!(
            !load(", \"strict\": true, \"alwaysStrict\": false")
                .options
                .always_strict
        );
        assert!(load(", \"alwaysStrict\": true").options.always_strict);
        assert!(load(", \"moduleDetection\": \"force\"").options.force_module);
        assert!(load(", \"module\": \"NodeNext\"").options.force_module);
        assert!(
            !load(", \"module\": \"NodeNext\", \"moduleDetection\": \"legacy\"")
                .options
                .force_module
        );

        // ${configDir} is the folder of the config being built, wherever it is written.
        t.write("shared/base.json", "{ \"compilerOptions\": { \"target\": \"es2022\", \"isolatedModules\": true, \"rootDir\": \"${configDir}/src\", \"outDir\": \"${configDir}/out\", \"paths\": { \"@/*\": [\"${configDir}/src/*\"] } }, \"include\": [\"${configDir}/src\"] }");
        t.write("app/src/b.ts", "export {};\n");
        t.write("app/tsconfig.json", "{ \"extends\": \"../shared/base.json\" }");
        let app = load_project(&t.0.join("app/tsconfig.json"), false).unwrap();
        assert_eq!(app.root_dir, Some(t.0.join("app/src")));
        assert_eq!(app.out_dir, t.0.join("app/out"));
        assert_eq!(app.files, [t.0.join("app/src/b.ts")]);
        assert_eq!(app.paths, [("@/*".to_string(), vec![t.0.join("app/src/*")])]);
    }

    #[test]
    fn what_cannot_be_reproduced_is_refused() {
        let t = Temp::new("refuse");
        t.write("src/a.ts", "export {};\n");
        let refusal = |options: &str| {
            t.write("tsconfig.json", &format!("{{ \"compilerOptions\": {{ {options} }} }}"));
            load_project(&t.0.join("tsconfig.json"), false).unwrap_err()
        };
        let ok = "\"target\": \"es2022\", \"module\": \"esnext\", \"isolatedModules\": true, \"rootDir\": \"src\", \"outDir\": \"dist\"";
        t.write("tsconfig.json", &format!("{{ \"compilerOptions\": {{ {ok} }} }}"));
        load_project(&t.0.join("tsconfig.json"), false).unwrap();

        for (options, expected) in [
            (
                "\"module\": \"esnext\", \"isolatedModules\": true, \"rootDir\": \"src\", \"outDir\": \"dist\"",
                "target must be",
            ),
            (&ok.replace("es2022", "es2020"), "target must be"),
            (&ok.replace("esnext", "commonjs"), "ES modules only"),
            (
                &ok.replace("\"isolatedModules\": true, ", ""),
                "isolatedModules or verbatimModuleSyntax",
            ),
            (&ok.replace(", \"outDir\": \"dist\"", ""), "outDir must be set"),
            (
                &format!("{ok}, \"moduleDetection\": \"sometimes\""),
                "unknown moduleDetection",
            ),
            (&format!("{ok}, \"noEmit\": true"), "emits no JavaScript"),
            (&format!("{ok}, \"declaration\": true"), "--skip-declarations"),
            (&format!("{ok}, \"composite\": true"), "--skip-declarations"),
            (&format!("{ok}, \"allowJs\": true"), "allowJs"),
            (&format!("{ok}, \"outFile\": \"a.js\""), "outFile"),
            (
                &format!("{ok}, \"useDefineForClassFields\": false"),
                "useDefineForClassFields",
            ),
            (&format!("{ok}, \"newLine\": \"crlf\""), "newLine"),
            (&format!("{ok}, \"sourceRoot\": \"/\""), "sourceRoot"),
            (
                &format!("{ok}, \"rewriteRelativeImportExtensions\": true"),
                "rewriteRelativeImportExtensions",
            ),
        ] {
            let error = refusal(options);
            assert!(error.contains(expected), "{options}: {error}");
        }

        t.write(
            "tsconfig.json",
            &format!("{{ \"compilerOptions\": {{ {ok}, \"declaration\": true, \"sourceMap\": true }} }}"),
        );
        let project = load_project(&t.0.join("tsconfig.json"), true).unwrap();
        assert_eq!(project.options.source_maps, SourceMaps::File);
    }
}
