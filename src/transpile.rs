//! TypeScript to JavaScript, one file at a time and without type checking: what `tsc`
//! emits for a project where no file needs another to be compiled (`isolatedModules`).
//!
//! Only syntax that erases to the JavaScript underneath is compiled, plus constructor
//! parameter properties. Anything `tsc` would rewrite into different JavaScript (enums,
//! namespaces, decorators, `import =`) is refused, not approximated: the promise is that
//! the output has the same syntax tree as `tsc`'s, apart from layout and comments.

use std::path::Path;

use base64::Engine as _;
use oxc::allocator::Allocator;
use oxc::ast::ast::{
    AccessorProperty, Decorator, ExportAllDeclaration, ExportDefaultDeclarationKind, ExportFromDeclaration, Expression,
    ImportDeclaration, ImportExpression, ImportMeta, ModuleExportName, Program, Statement, TSEnumDeclaration,
    TSExportAssignment, TSExternalModuleDeclaration, TSGlobalDeclaration, TSImportEqualsDeclaration, TSImportType,
    TSNamespaceDeclaration, VariableDeclaration, VariableDeclarationKind,
};
use oxc::ast_visit::{Visit, walk};
use oxc::codegen::{Codegen, CodegenOptions, CommentOptions, IndentChar};
use oxc::parser::Parser;
use oxc::semantic::{Scoping, SemanticBuilder, SymbolFlags};
use oxc::span::{GetSpan, SourceType, Span};
use oxc::transformer::{TransformOptions, Transformer};
use serde_json::{Map, Value};

/// Where a file's source map goes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SourceMaps {
    #[default]
    None,
    /// `x.js.map` beside `x.js` (`sourceMap`).
    File,
    /// A data URL in the last line of `x.js` (`inlineSourceMap`).
    Inline,
}

/// How the project's `tsconfig.json` says its files compile.
#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    /// `verbatimModuleSyntax`: imports and exports are kept exactly as written unless
    /// marked `type`. Otherwise an import that is only used as a type is dropped.
    pub verbatim_module_syntax: bool,
    /// `target` is `ESNext`, so `tsc` leaves `using` and `accessor` as they are.
    pub esnext: bool,
    /// `alwaysStrict`, which `strict` turns on: a file that is not a module starts with
    /// `"use strict"`.
    pub always_strict: bool,
    /// Every file is a module whatever it contains: `moduleDetection` is `force`, or it
    /// is `auto` and the file is an ES module by its package and extension.
    pub force_module: bool,
    /// `removeComments`.
    pub remove_comments: bool,
    pub source_maps: SourceMaps,
    /// `inlineSources`: the map carries the TypeScript source.
    pub inline_sources: bool,
}

/// One compiled file, before it knows where it will be written.
#[derive(Debug)]
pub struct Output {
    code: String,
    /// The `names` and `mappings` of its source map, when one was asked for.
    map: Option<(Value, String)>,
    /// Every module this file names: imports, re-exports, `import()` and import types,
    /// type-only ones included. `tsc` compiles the project files they lead to.
    pub specifiers: Vec<String>,
    /// The targets of `/// <reference path="..." />` lines.
    pub references: Vec<String>,
}

/// The names a compiled file and its map refer to each other by.
pub struct Names<'a> {
    /// The output file's name, e.g. `a.js`.
    pub file: &'a str,
    /// The source, relative to the output file's folder, e.g. `../src/a.ts`.
    pub source: &'a str,
}

/// What gets written: the JavaScript, and the `.map` file for [`SourceMaps::File`].
#[derive(Debug)]
pub struct Written {
    pub code: String,
    pub map: Option<String>,
}

impl Output {
    /// Names the output and its source in the map, and points the code at the map.
    pub fn finish(self, source_text: &str, options: Options, names: &Names) -> Result<Written, String> {
        let mut code = self.code;
        let Some((map_names, mappings)) = self.map else {
            return Ok(Written { code, map: None });
        };
        // tsc's field order.
        let mut ordered = Map::new();
        ordered.insert("version".into(), Value::from(3));
        ordered.insert("file".into(), Value::from(names.file));
        ordered.insert("sourceRoot".into(), Value::from(""));
        ordered.insert("sources".into(), Value::from(vec![names.source]));
        ordered.insert("names".into(), map_names);
        ordered.insert("mappings".into(), Value::from(mappings));
        if options.inline_sources {
            ordered.insert("sourcesContent".into(), Value::from(vec![source_text]));
        }
        let json = serde_json::to_string(&Value::Object(ordered)).map_err(|e| format!("{}: {e}", names.file))?;
        if !code.ends_with('\n') {
            code.push('\n');
        }
        if options.source_maps == SourceMaps::Inline {
            let encoded = base64::engine::general_purpose::STANDARD.encode(json.as_bytes());
            code.push_str(&format!("//# sourceMappingURL=data:application/json;base64,{encoded}"));
            return Ok(Written { code, map: None });
        }
        code.push_str(&format!("//# sourceMappingURL={}.map", names.file));
        Ok(Written { code, map: Some(json) })
    }
}

struct Scan {
    esnext: bool,
    /// Inside `declare namespace`, `declare module` or `declare global`, where nothing is emitted.
    ambient: u32,
    refused: Option<(Span, &'static str)>,
    specifiers: Vec<String>,
    import_meta: bool,
}

impl Scan {
    fn refuse(&mut self, span: Span, what: &'static str) {
        if self.refused.is_none_or(|(seen, _)| span.start < seen.start) {
            self.refused = Some((span, what));
        }
    }

    /// Records a module specifier; a JSON module loaded at run time is refused.
    fn names(&mut self, span: Span, specifier: Option<&str>, loaded: bool) {
        let Some(specifier) = specifier else { return };
        if loaded && specifier.ends_with(".json") {
            self.refuse(span, "a JSON import, which tsc copies into outDir");
        }
        self.specifiers.push(specifier.to_string());
    }
}

impl<'a> Visit<'a> for Scan {
    fn visit_ts_enum_declaration(&mut self, it: &TSEnumDeclaration<'a>) {
        if !it.declare && self.ambient == 0 {
            self.refuse(it.span, "an enum");
        }
    }

    fn visit_ts_namespace_declaration(&mut self, it: &TSNamespaceDeclaration<'a>) {
        if !it.declare && self.ambient == 0 {
            self.refuse(it.span, "a namespace");
            return;
        }
        self.ambient += 1;
        walk::walk_ts_namespace_declaration(self, it);
        self.ambient -= 1;
    }

    fn visit_ts_external_module_declaration(&mut self, it: &TSExternalModuleDeclaration<'a>) {
        self.ambient += 1;
        walk::walk_ts_external_module_declaration(self, it);
        self.ambient -= 1;
    }

    fn visit_ts_global_declaration(&mut self, it: &TSGlobalDeclaration<'a>) {
        self.ambient += 1;
        walk::walk_ts_global_declaration(self, it);
        self.ambient -= 1;
    }

    fn visit_decorator(&mut self, it: &Decorator<'a>) {
        self.refuse(it.span, "a decorator");
    }

    fn visit_ts_import_equals_declaration(&mut self, it: &TSImportEqualsDeclaration<'a>) {
        self.refuse(it.span, "an `import =` declaration");
    }

    fn visit_ts_export_assignment(&mut self, it: &TSExportAssignment<'a>) {
        self.refuse(it.span, "an `export =` assignment");
    }

    fn visit_variable_declaration(&mut self, it: &VariableDeclaration<'a>) {
        if !self.esnext
            && matches!(
                it.kind,
                VariableDeclarationKind::Using | VariableDeclarationKind::AwaitUsing
            )
        {
            self.refuse(it.span, "a `using` declaration, which tsc rewrites below target ESNext");
        }
        walk::walk_variable_declaration(self, it);
    }

    fn visit_accessor_property(&mut self, it: &AccessorProperty<'a>) {
        if !self.esnext {
            self.refuse(it.span, "an `accessor` field, which tsc rewrites below target ESNext");
        }
        walk::walk_accessor_property(self, it);
    }

    fn visit_import_meta(&mut self, _: &ImportMeta) {
        self.import_meta = true;
    }

    fn visit_import_declaration(&mut self, it: &ImportDeclaration<'a>) {
        self.names(it.span, it.source.value.as_str(), !it.import_kind.is_type());
        walk::walk_import_declaration(self, it);
    }

    fn visit_export_from_declaration(&mut self, it: &ExportFromDeclaration<'a>) {
        self.names(it.span, it.source.value.as_str(), !it.export_kind.is_type());
        walk::walk_export_from_declaration(self, it);
    }

    fn visit_export_all_declaration(&mut self, it: &ExportAllDeclaration<'a>) {
        self.names(it.span, it.source.value.as_str(), !it.export_kind.is_type());
        walk::walk_export_all_declaration(self, it);
    }

    fn visit_import_expression(&mut self, it: &ImportExpression<'a>) {
        // tsc follows a plain string however it is quoted, a template with no holes included.
        match &it.source {
            Expression::StringLiteral(source) => self.names(it.span(), source.value.as_str(), true),
            Expression::TemplateLiteral(template) if template.expressions.is_empty() => {
                let cooked = template.quasis.first().and_then(|quasi| quasi.value.cooked.as_ref());
                self.names(it.span(), cooked.and_then(|text| text.as_str()), true);
            }
            _ => {}
        }
        walk::walk_import_expression(self, it);
    }

    fn visit_ts_import_type(&mut self, it: &TSImportType<'a>) {
        self.names(it.span, it.source.value.as_str(), false);
        walk::walk_ts_import_type(self, it);
    }
}

fn line_of(source: &str, offset: u32) -> usize {
    source.as_bytes()[..(offset as usize).min(source.len())]
        .iter()
        .filter(|b| **b == b'\n')
        .count()
        + 1
}

/// An import, an export, or an `export` on a declaration: what makes a file a module.
fn is_module_statement(statement: &Statement) -> bool {
    matches!(
        statement,
        Statement::ImportDeclaration(_)
            | Statement::ExportAllDeclaration(_)
            | Statement::ExportDefaultDeclaration(_)
            | Statement::ExportDeclaration(_)
            | Statement::ExportNamedDeclaration(_)
            | Statement::ExportFromDeclaration(_)
            | Statement::TSExportAssignment(_)
            | Statement::TSNamespaceExportDeclaration(_)
    )
}

/// tsc keeps or drops these two forms differently from the transform underneath, so they
/// are settled here first.
fn settle_empty_module_statements(program: &mut Program, verbatim: bool) {
    if verbatim {
        // `export { type T } from "x"` still loads "x": tsc writes `export {} from "x"`.
        for statement in program.body.iter_mut() {
            if let Statement::ExportFromDeclaration(export) = statement
                && !export.export_kind.is_type()
                && !export.specifiers.is_empty()
                && export
                    .specifiers
                    .iter()
                    .all(|specifier| specifier.export_kind.is_type())
            {
                export.specifiers.clear();
            }
        }
    } else {
        // Without it, an import or re-export that names nothing is dropped, load and all.
        program.body.retain(|statement| match statement {
            Statement::ImportDeclaration(import) => !import.specifiers.as_ref().is_some_and(|list| list.is_empty()),
            Statement::ExportFromDeclaration(export) => !export.specifiers.is_empty(),
            _ => true,
        });
    }
}

/// `declare const a: T; export { a };` tsc keeps the export of a name that has no
/// JavaScript behind it. The transform drops it, so the file is refused.
fn exported_declared_value(program: &Program, scoping: &Scoping) -> Option<Span> {
    let declared = |name: &str| {
        scoping
            .get_root_binding(name.into())
            .map(|symbol| scoping.symbol_flags(symbol))
            .is_some_and(|flags| flags.contains(SymbolFlags::Ambient) && flags.intersects(SymbolFlags::Value))
    };
    for statement in &program.body {
        match statement {
            Statement::ExportNamedDeclaration(export) if !export.export_kind.is_type() => {
                for specifier in &export.specifiers {
                    if !specifier.export_kind.is_type()
                        && let ModuleExportName::IdentifierReference(local) = &specifier.local
                        && declared(local.name.as_str())
                    {
                        return Some(specifier.span);
                    }
                }
            }
            Statement::ExportDefaultDeclaration(export) => {
                if let ExportDefaultDeclarationKind::Identifier(local) = &export.declaration
                    && declared(local.name.as_str())
                {
                    return Some(export.span);
                }
            }
            _ => {}
        }
    }
    None
}

/// The targets of the `/// <reference path="..." />` lines that open a file.
fn reference_paths(source: &str) -> Vec<String> {
    let mut found = Vec::new();
    for line in source.trim_start_matches('\u{feff}').lines() {
        let line = line.trim();
        if line.is_empty() || (line.starts_with("//") && !line.starts_with("///")) {
            continue;
        }
        let Some(rest) = line.strip_prefix("///") else { break };
        if let Some(at) = rest.find("path=")
            && rest.trim_start().starts_with("<reference")
        {
            let value = &rest[at + 5..];
            if let Some(quote) = value.chars().next().filter(|c| *c == '"' || *c == '\'')
                && let Some(end) = value[1..].find(quote)
            {
                found.push(value[1..=end].to_string());
            }
        }
    }
    found
}

/// What a declaration file names. It emits nothing itself, but tsc compiles the project
/// files it imports like any others.
pub fn scan_declaration(path: &Path, source: &str) -> Result<(Vec<String>, Vec<String>), String> {
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, SourceType::d_ts()).parse();
    if parsed.fatal_error {
        let detail = parsed
            .diagnostics
            .first()
            .map(|d| d.to_string())
            .unwrap_or_else(|| "syntax error".to_string());
        return Err(format!("{}: {detail}", path.display()));
    }
    let mut scan = Scan {
        esnext: true,
        ambient: 1,
        refused: None,
        specifiers: Vec::new(),
        import_meta: false,
    };
    scan.visit_program(&parsed.program);
    Ok((scan.specifiers, reference_paths(source)))
}

/// Compiles one TypeScript source file. `path` is used for messages and to tell `.mts`
/// from `.ts`.
pub fn transpile(path: &Path, source: &str, options: Options) -> Result<Output, String> {
    let shown = path.display();
    let refusal = |span: Span, what: &str| {
        format!(
            "{shown}:{}: {what}. kite build only compiles syntax it can show matches tsc; use tsc for this project",
            line_of(source, span.start)
        )
    };
    let source_type = SourceType::from_path(path).map_err(|e| format!("{shown}: {e}"))?;
    if source_type.is_jsx() {
        return Err(format!(
            "{shown}: kite build does not compile JSX yet; use tsc for this project"
        ));
    }
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, source_type).parse();
    if parsed.fatal_error || !parsed.diagnostics.is_empty() {
        let detail = parsed
            .diagnostics
            .first()
            .map(|d| d.to_string())
            .unwrap_or_else(|| "syntax error".to_string());
        return Err(format!("{shown}: {detail}"));
    }
    let mut program = parsed.program;

    let mut scan = Scan {
        esnext: options.esnext,
        ambient: 0,
        refused: None,
        specifiers: Vec::new(),
        import_meta: false,
    };
    scan.visit_program(&program);
    if let Some((span, what)) = scan.refused {
        return Err(refusal(span, what));
    }
    // What tsc goes by, read before anything is removed.
    let is_module = options.force_module || scan.import_meta || program.body.iter().any(is_module_statement);
    // tsc goes by what the directive says once its escapes are read.
    let strict_already = program
        .directives
        .iter()
        .any(|directive| directive.expression.value.as_str() == Some("use strict"));
    let hashbang = program.hashbang.is_some();

    settle_empty_module_statements(&mut program, options.verbatim_module_syntax);
    let scoping = SemanticBuilder::new().build(&program).semantic.into_scoping();
    if let Some(span) = exported_declared_value(&program, &scoping) {
        return Err(refusal(
            span,
            "an export of a `declare`d name, which tsc keeps although nothing defines it",
        ));
    }
    // Nothing is lowered: only targets where tsc lowers nothing either are accepted.
    let mut transform = TransformOptions::from_target("esnext")?;
    transform.typescript.only_remove_type_imports = options.verbatim_module_syntax;
    let transformed = Transformer::new(&allocator, path, &transform).build_with_scoping(scoping, &mut program);
    if let Some(error) = transformed.diagnostics.first() {
        return Err(format!("{shown}: {error}"));
    }
    let still_module_syntax = program.body.iter().any(is_module_statement);

    let print = CodegenOptions {
        indent_char: IndentChar::Space,
        indent_width: 4,
        comments: if options.remove_comments {
            CommentOptions::disabled()
        } else {
            CommentOptions::default()
        },
        source_map_path: (options.source_maps != SourceMaps::None).then(|| path.to_path_buf()),
        ..CodegenOptions::default()
    };
    let printed = Codegen::new()
        .with_options(print)
        .with_source_text(source)
        .build(&program);
    let mut code = printed.code;
    let mut map = match printed.map {
        None => None,
        Some(generated) => {
            let raw: Value = serde_json::from_str(&generated.to_json_string()).map_err(|e| format!("{shown}: {e}"))?;
            let names = raw.get("names").cloned().unwrap_or_else(|| Value::Array(Vec::new()));
            let mappings = raw
                .get("mappings")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            Some((names, mappings))
        }
    };

    if !code.is_empty() && !code.ends_with('\n') {
        code.push('\n');
    }
    if is_module && !still_module_syntax {
        // A module with nothing left to import or export stays a module.
        code.push_str("export {};\n");
    }
    if !is_module && options.always_strict && !strict_already {
        // A script is put in strict mode, after a hashbang line if there is one.
        let at = if hashbang {
            code.find('\n').map_or(code.len(), |i| i + 1)
        } else {
            0
        };
        code.insert_str(at, "\"use strict\";\n");
        if let Some((_, mappings)) = &mut map {
            // One more generated line, which maps to nothing.
            let at = if hashbang {
                mappings.find(';').map_or(mappings.len(), |i| i + 1)
            } else {
                0
            };
            mappings.insert(at, ';');
        }
    }
    Ok(Output {
        code,
        map,
        specifiers: scan.specifiers,
        references: reference_paths(source),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAMES: Names = Names {
        file: "a.js",
        source: "../src/a.ts",
    };
    const VERBATIM: Options = Options {
        verbatim_module_syntax: true,
        esnext: false,
        always_strict: true,
        force_module: false,
        remove_comments: false,
        source_maps: SourceMaps::None,
        inline_sources: false,
    };
    const LOOSE: Options = Options {
        verbatim_module_syntax: false,
        ..VERBATIM
    };

    fn compile(source: &str, options: Options) -> Result<Written, String> {
        transpile(Path::new("src/a.ts"), source, options)?.finish(source, options, &NAMES)
    }

    fn js(source: &str) -> String {
        compile(source, VERBATIM).unwrap().code
    }

    #[test]
    fn types_are_erased_and_values_kept() {
        let out = js("import { type A, b } from \"./b.js\";\nexport const c: A = b as A;\n");
        assert!(out.contains("b } from \"./b.js\""), "{out}");
        assert!(out.contains("export const c = b;"), "{out}");
        assert!(!out.contains("A"), "{out}");
    }

    #[test]
    fn parameter_properties_become_declared_fields() {
        let out = js("export class P { constructor(private readonly x: number, y: string) { void y; } }\n");
        assert!(out.contains("x;"), "{out}");
        assert!(out.contains("this.x = x;"), "{out}");
    }

    #[test]
    fn an_import_used_only_as_a_type_is_dropped_without_verbatim_syntax() {
        let source = "import { A } from \"./a.js\";\nimport { b } from \"./b.js\";\nexport const c: A = b;\n";
        let loose = compile(source, LOOSE).unwrap().code;
        assert!(!loose.contains("./a.js"), "{loose}");
        assert!(loose.contains("./b.js"), "{loose}");
        let verbatim = js(source);
        assert!(verbatim.contains("./a.js"), "{verbatim}");
    }

    #[test]
    fn imports_and_re_exports_that_name_nothing_follow_tsc() {
        let source = "import {} from \"./a.js\";\nexport {} from \"./b.js\";\nexport { type T } from \"./c.js\";\nexport const x = 1;\n";
        // Verbatim: all three modules are still loaded.
        let verbatim = js(source);
        for module in ["./a.js", "./b.js", "./c.js"] {
            assert!(verbatim.contains(module), "{module}: {verbatim}");
        }
        assert!(verbatim.contains("export {} from \"./c.js\""), "{verbatim}");
        // Otherwise none is.
        let loose = compile(source, LOOSE).unwrap().code;
        for module in ["./a.js", "./b.js", "./c.js"] {
            assert!(!loose.contains(module), "{module}: {loose}");
        }
    }

    #[test]
    fn a_module_stays_a_module_and_a_script_becomes_strict() {
        // Everything was a type: the module marker is added.
        assert_eq!(js("export type T = number;\n").trim(), "export {};");
        assert_eq!(
            compile("import { A } from \"./a.js\";\nlet x: A;\n", LOOSE)
                .unwrap()
                .code,
            "let x;\nexport {};\n"
        );
        // `import.meta` makes a module too.
        assert!(js("console.log(import.meta.url);\n").ends_with("export {};\n"));
        // No imports or exports: a script, which alwaysStrict puts in strict mode.
        assert_eq!(js("const x: number = 1;\n"), "\"use strict\";\nconst x = 1;\n");
        assert_eq!(
            js("#!/usr/bin/env node\nconst x: number = 1;\n"),
            "#!/usr/bin/env node\n\"use strict\";\nconst x = 1;\n"
        );
        assert_eq!(
            js("\"use strict\";\nconst x: number = 1;\n")
                .matches("use strict")
                .count(),
            1
        );
        let sloppy = Options {
            always_strict: false,
            ..VERBATIM
        };
        assert_eq!(
            compile("const x: number = 1;\n", sloppy).unwrap().code,
            "const x = 1;\n"
        );
        // Forced to be a module: the marker, and no directive.
        let forced = Options {
            force_module: true,
            ..VERBATIM
        };
        assert_eq!(
            compile("const x: number = 1;\n", forced).unwrap().code,
            "const x = 1;\nexport {};\n"
        );
        assert_eq!(
            compile("export const x: number = 1;\n", forced).unwrap().code,
            "export const x = 1;\n"
        );
    }

    #[test]
    fn rewritten_syntax_is_refused_with_its_line() {
        for (source, what) in [
            ("export const a = 1;\nenum E { A }\n", "an enum"),
            ("namespace N { export const a = 1; }\n", "a namespace"),
            (
                "function d(_: unknown) {}\n@d class C {}\nexport { C };\n",
                "a decorator",
            ),
            (
                "import fs = require(\"node:fs\");\nexport { fs };\n",
                "an `import =` declaration",
            ),
            ("const a = 1;\nexport = a;\n", "an `export =` assignment"),
            (
                "import data from \"./data.json\" with { type: \"json\" };\nexport { data };\n",
                "a JSON import",
            ),
            ("export const load = () => import(\"./data.json\");\n", "a JSON import"),
            (
                "export function f() { using a = null; return a; }\n",
                "a `using` declaration",
            ),
            ("export class C { accessor x = 1; }\n", "an `accessor` field"),
            (
                "declare const d: number;\nexport { d };\n",
                "an export of a `declare`d name",
            ),
            (
                "declare class D {}\nexport default D;\n",
                "an export of a `declare`d name",
            ),
        ] {
            let error = compile(source, LOOSE).unwrap_err();
            assert!(error.contains(what), "{source}: {error}");
            assert!(error.contains("use tsc"), "{error}");
        }
        let error = compile("export const a = 1;\nenum E { A }\n", LOOSE).unwrap_err();
        assert!(error.starts_with("src/a.ts:2: "), "{error}");
    }

    #[test]
    fn ambient_declarations_and_type_only_json_are_fine() {
        js(
            "declare enum E { A }\ndeclare namespace N { const a: number; enum Inner { A } namespace Deep { const b: number; } }\n\
            declare global { namespace NodeJS { interface ProcessEnv { X: string } } enum G { A } }\n\
            declare module \"other\" { enum M { A } namespace Q { const c: number; } }\n\
            import type data from \"./data.json\";\nexport type T = typeof data | E | typeof N.a;\n\
            declare const local: number;\nexport type { local };\nexport type L = typeof local;\n",
        );
    }

    #[test]
    fn every_named_module_and_reference_is_reported() {
        let source = "/// <reference path=\"./globals.d.ts\" />\n/// <reference types=\"node\" />\n\
            import type { A } from \"./a.js\";\nimport b from \"./b.js\";\nexport * from \"./c.js\";\n\
            export { d } from \"./d.js\";\nexport type E = import(\"./e.js\").E;\nexport const f = () => import(\"./f.js\");\n\
            export const g: A = b;\n";
        let output = transpile(Path::new("src/a.ts"), source, VERBATIM).unwrap();
        assert_eq!(
            output.specifiers,
            ["./a.js", "./b.js", "./c.js", "./d.js", "./e.js", "./f.js"]
        );
        assert_eq!(output.references, ["./globals.d.ts"]);
    }

    #[test]
    fn esnext_keeps_what_tsc_keeps() {
        let options = Options { esnext: true, ..LOOSE };
        let out = compile(
            "export class C { accessor x = 1; }\nexport function f() { using a = null; return a; }\n",
            options,
        )
        .unwrap()
        .code;
        assert!(
            out.contains("accessor x = 1;") && out.contains("using a = null;"),
            "{out}"
        );
    }

    #[test]
    fn syntax_errors_and_jsx_are_reported() {
        assert!(compile("export const = ;\n", LOOSE).is_err());
        let error = transpile(Path::new("a.tsx"), "export const a = 1;\n", LOOSE).unwrap_err();
        assert!(error.contains("JSX"), "{error}");
    }

    #[test]
    fn a_byte_order_mark_and_windows_line_endings_compile() {
        let out = js("\u{feff}export const a: number = 1;\r\nexport const b: string = `x\r\ny`;\r\n");
        assert!(
            out.contains("export const a = 1;") && out.contains("export const b ="),
            "{out}"
        );
        assert!(!out.starts_with('\u{feff}'), "{out:?}");
    }

    #[test]
    fn source_maps_use_tscs_shape() {
        let source = "export const a: number = 1;\n";
        let file = compile(
            source,
            Options {
                source_maps: SourceMaps::File,
                ..LOOSE
            },
        )
        .unwrap();
        assert!(file.code.ends_with("//# sourceMappingURL=a.js.map"), "{}", file.code);
        let map: Value = serde_json::from_str(&file.map.unwrap()).unwrap();
        assert_eq!(map["version"], 3);
        assert_eq!(map["file"], "a.js");
        assert_eq!(map["sourceRoot"], "");
        assert_eq!(map["sources"], serde_json::json!(["../src/a.ts"]));
        assert!(map["mappings"].as_str().is_some_and(|m| !m.is_empty()));
        assert!(map.get("sourcesContent").is_none());

        let inline = compile(
            source,
            Options {
                source_maps: SourceMaps::Inline,
                inline_sources: true,
                ..LOOSE
            },
        )
        .unwrap();
        assert!(inline.map.is_none());
        let url = inline.code.rsplit("base64,").next().unwrap();
        let decoded = base64::engine::general_purpose::STANDARD.decode(url).unwrap();
        let map: Value = serde_json::from_slice(&decoded).unwrap();
        assert_eq!(map["sourcesContent"], serde_json::json!([source]));

        // The line added for strict mode shifts every mapping down by one.
        let script = "const a: number = 1;\n";
        let plain = compile(
            script,
            Options {
                source_maps: SourceMaps::File,
                always_strict: false,
                ..LOOSE
            },
        )
        .unwrap();
        let strict = compile(
            script,
            Options {
                source_maps: SourceMaps::File,
                ..LOOSE
            },
        )
        .unwrap();
        let mappings = |written: &Written| {
            serde_json::from_str::<Value>(written.map.as_ref().unwrap()).unwrap()["mappings"]
                .as_str()
                .unwrap()
                .to_string()
        };
        assert_eq!(mappings(&strict), format!(";{}", mappings(&plain)));
    }

    #[test]
    fn template_imports_declaration_files_and_escaped_directives_follow_tsc() {
        // A template with no holes is a plain string to tsc; one with holes names nothing.
        let dynamic =
            "export const a = () => import(`./tpl.js`);\nexport const b = (n: string) => import(`./${n}.js`);\n";
        assert_eq!(
            transpile(Path::new("src/a.ts"), dynamic, VERBATIM).unwrap().specifiers,
            ["./tpl.js"]
        );

        // A declaration file's imports count, and nothing in it is refused.
        let declared = "import type { G } from \"./from-dts.js\";\ndeclare global { enum E { A } }\nexport type H = G | import(\"./other.js\").O;\n";
        let (specifiers, references) = scan_declaration(Path::new("src/globals.d.ts"), declared).unwrap();
        assert_eq!(specifiers, ["./from-dts.js", "./other.js"]);
        assert!(references.is_empty());

        // An escaped spelling counts as the directive, as tsc reads it: nothing is added.
        assert!(!js("\"use\\x20strict\";\nconst x: number = 1;\n").contains("\"use strict\""));
    }
}
