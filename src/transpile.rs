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
    AccessorProperty, Decorator, ExportAllDeclaration, ExportFromDeclaration, Expression, ImportDeclaration,
    ImportExpression, TSEnumDeclaration, TSExportAssignment, TSImportEqualsDeclaration, TSNamespaceDeclaration,
    VariableDeclaration, VariableDeclarationKind,
};
use oxc::ast_visit::{Visit, walk};
use oxc::codegen::{Codegen, CodegenOptions, CommentOptions, IndentChar};
use oxc::parser::Parser;
use oxc::semantic::SemanticBuilder;
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
    /// `removeComments`.
    pub remove_comments: bool,
    pub source_maps: SourceMaps,
    /// `inlineSources`: the map carries the TypeScript source.
    pub inline_sources: bool,
}

/// One compiled file.
#[derive(Debug)]
pub struct Output {
    pub code: String,
    /// The contents of the `.map` file, for [`SourceMaps::File`].
    pub map: Option<String>,
}

/// The names a compiled file and its map refer to each other by.
pub struct Names<'a> {
    /// The output file's name, e.g. `a.js`.
    pub file: &'a str,
    /// The source, relative to the output file's folder, e.g. `../src/a.ts`.
    pub source: &'a str,
}

struct Refusals<'s> {
    source: &'s str,
    esnext: bool,
    first: Option<(Span, &'static str)>,
}

impl Refusals<'_> {
    fn refuse(&mut self, span: Span, what: &'static str) {
        if self.first.is_none_or(|(seen, _)| span.start < seen.start) {
            self.first = Some((span, what));
        }
    }

    fn json(&mut self, span: Span, specifier: Option<&str>) {
        if specifier.is_some_and(|s| s.ends_with(".json")) {
            self.refuse(span, "a JSON import, which tsc copies into outDir");
        }
    }
}

impl<'a> Visit<'a> for Refusals<'_> {
    fn visit_ts_enum_declaration(&mut self, it: &TSEnumDeclaration<'a>) {
        if !it.declare {
            self.refuse(it.span, "an enum");
        }
    }

    fn visit_ts_namespace_declaration(&mut self, it: &TSNamespaceDeclaration<'a>) {
        if !it.declare {
            self.refuse(it.span, "a namespace");
        }
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

    fn visit_import_declaration(&mut self, it: &ImportDeclaration<'a>) {
        if !it.import_kind.is_type() {
            self.json(it.span, it.source.value.as_str());
        }
        walk::walk_import_declaration(self, it);
    }

    fn visit_export_from_declaration(&mut self, it: &ExportFromDeclaration<'a>) {
        if !it.export_kind.is_type() {
            self.json(it.span, it.source.value.as_str());
        }
        walk::walk_export_from_declaration(self, it);
    }

    fn visit_export_all_declaration(&mut self, it: &ExportAllDeclaration<'a>) {
        if !it.export_kind.is_type() {
            self.json(it.span, it.source.value.as_str());
        }
        walk::walk_export_all_declaration(self, it);
    }

    fn visit_import_expression(&mut self, it: &ImportExpression<'a>) {
        if let Expression::StringLiteral(source) = &it.source {
            self.json(it.span(), source.value.as_str());
        }
        walk::walk_import_expression(self, it);
    }
}

fn line_of(source: &str, offset: u32) -> usize {
    source.as_bytes()[..(offset as usize).min(source.len())]
        .iter()
        .filter(|b| **b == b'\n')
        .count()
        + 1
}

/// Compiles one TypeScript source file. `path` is used for messages and to tell `.mts`
/// from `.ts`.
pub fn transpile(path: &Path, source: &str, options: Options, names: &Names) -> Result<Output, String> {
    let shown = path.display();
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

    let mut refusals = Refusals {
        source,
        esnext: options.esnext,
        first: None,
    };
    refusals.visit_program(&program);
    if let Some((span, what)) = refusals.first {
        return Err(format!(
            "{shown}:{}: {what}. kite build only compiles syntax it can show matches tsc; use tsc for this project",
            line_of(refusals.source, span.start)
        ));
    }

    let scoping = SemanticBuilder::new().build(&program).semantic.into_scoping();
    // Nothing is lowered: only targets where tsc lowers nothing either are accepted.
    let mut transform = TransformOptions::from_target("esnext")?;
    transform.typescript.only_remove_type_imports = options.verbatim_module_syntax;
    let transformed = Transformer::new(&allocator, path, &transform).build_with_scoping(scoping, &mut program);
    if let Some(error) = transformed.diagnostics.first() {
        return Err(format!("{shown}: {error}"));
    }

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
    let mut map = None;
    if let Some(generated) = printed.map {
        // tsc's field order, with the names the two files know each other by.
        let raw: Value = serde_json::from_str(&generated.to_json_string()).map_err(|e| format!("{shown}: {e}"))?;
        let mut ordered = Map::new();
        ordered.insert("version".into(), Value::from(3));
        ordered.insert("file".into(), Value::from(names.file));
        ordered.insert("sourceRoot".into(), Value::from(""));
        ordered.insert("sources".into(), Value::from(vec![names.source]));
        ordered.insert(
            "names".into(),
            raw.get("names").cloned().unwrap_or_else(|| Value::Array(Vec::new())),
        );
        ordered.insert(
            "mappings".into(),
            raw.get("mappings").cloned().unwrap_or_else(|| Value::from("")),
        );
        if options.inline_sources {
            ordered.insert("sourcesContent".into(), Value::from(vec![source]));
        }
        let json = serde_json::to_string(&Value::Object(ordered)).map_err(|e| format!("{shown}: {e}"))?;
        if !code.ends_with('\n') {
            code.push('\n');
        }
        match options.source_maps {
            SourceMaps::Inline => {
                let encoded = base64::engine::general_purpose::STANDARD.encode(json.as_bytes());
                code.push_str(&format!("//# sourceMappingURL=data:application/json;base64,{encoded}"));
            }
            _ => {
                code.push_str(&format!("//# sourceMappingURL={}.map", names.file));
                map = Some(json);
            }
        }
    }
    Ok(Output { code, map })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAMES: Names = Names {
        file: "a.js",
        source: "../src/a.ts",
    };

    fn compile(source: &str, options: Options) -> Result<Output, String> {
        transpile(Path::new("src/a.ts"), source, options, &NAMES)
    }

    fn js(source: &str) -> String {
        compile(
            source,
            Options {
                verbatim_module_syntax: true,
                ..Options::default()
            },
        )
        .unwrap()
        .code
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
        let loose = compile(source, Options::default()).unwrap().code;
        assert!(!loose.contains("./a.js"), "{loose}");
        assert!(loose.contains("./b.js"), "{loose}");
        let verbatim = js(source);
        assert!(verbatim.contains("./a.js"), "{verbatim}");
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
        ] {
            let error = compile(source, Options::default()).unwrap_err();
            assert!(error.contains(what), "{source}: {error}");
            assert!(error.contains("use tsc"), "{error}");
        }
        let error = compile("export const a = 1;\nenum E { A }\n", Options::default()).unwrap_err();
        assert!(error.starts_with("src/a.ts:2: "), "{error}");
    }

    #[test]
    fn ambient_declarations_and_type_only_json_are_fine() {
        js(
            "declare enum E { A }\ndeclare namespace N { const a: number; }\nimport type data from \"./data.json\";\nexport type T = typeof data | E | typeof N.a;\n",
        );
    }

    #[test]
    fn esnext_keeps_what_tsc_keeps() {
        let options = Options {
            esnext: true,
            ..Options::default()
        };
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
        assert!(compile("export const = ;\n", Options::default()).is_err());
        let error = transpile(Path::new("a.tsx"), "export const a = 1;\n", Options::default(), &NAMES).unwrap_err();
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
        let file = compile(
            "export const a: number = 1;\n",
            Options {
                source_maps: SourceMaps::File,
                ..Options::default()
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
            "export const a: number = 1;\n",
            Options {
                source_maps: SourceMaps::Inline,
                inline_sources: true,
                ..Options::default()
            },
        )
        .unwrap();
        assert!(inline.map.is_none());
        let url = inline.code.rsplit("base64,").next().unwrap();
        let decoded = base64::engine::general_purpose::STANDARD.decode(url).unwrap();
        let map: Value = serde_json::from_slice(&decoded).unwrap();
        assert_eq!(
            map["sourcesContent"],
            serde_json::json!(["export const a: number = 1;\n"])
        );
    }
}
