// Compares two trees of emitted JavaScript by syntax tree. Layout, comments, quote style
// and redundant parentheses are ignored, and so are three spellings that mean the same:
//   import {} from "x"        and  import "x"
//   { a: a }                  and  { a }
//   if (c) { single(); }      and  if (c) single();   (never for a declaration)
// Prints the first difference per file and exits 1 if there is any.
//
//   node harness/ast-diff.mjs <tsc-output-dir> <kite-output-dir>
//
// `typescript` is resolved from the working directory's node_modules.
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { join } from "node:path";

// An import would look beside this file; the caller's folder is where typescript is.
const ts = createRequire(join(process.cwd(), "ast-diff.cjs"))("typescript");

const [first, second] = process.argv.slice(2);
const K = ts.SyntaxKind;
const DECLARATIONS = new Set([K.VariableStatement, K.FunctionDeclaration, K.ClassDeclaration]);
const BODIES = new Set([K.IfStatement, K.ForStatement, K.ForInStatement, K.ForOfStatement, K.WhileStatement, K.DoStatement, K.LabeledStatement, K.WithStatement]);

function simplify(node, parent) {
  for (;;) {
    if (node.kind === K.ParenthesizedExpression) node = node.expression;
    else if (node.kind === K.Block && parent && BODIES.has(parent.kind) && node.statements.length === 1 && !DECLARATIONS.has(node.statements[0].kind)) node = node.statements[0];
    else return node;
  }
}
// Where a lone `;` is a statement of its own and means nothing. Elsewhere it is the body
// of an `if` or a loop, and dropping it would change the program.
const LISTS = new Set([K.SourceFile, K.Block, K.ModuleBlock, K.CaseClause, K.DefaultClause, K.ClassStaticBlockDeclaration]);
function children(node) {
  const out = [];
  ts.forEachChild(node, (child) => { out.push(child); });
  return out.filter((child) => {
    if (child.kind === K.EmptyStatement && LISTS.has(node.kind)) return false;
    if (child.kind === K.SemicolonClassElement) return false;
    // `import {} from "x"` and `import d, {} from "x"` bind nothing through the braces.
    if (child.kind === K.NamedImports && child.elements.length === 0) return false;
    if (child.kind === K.ImportClause && !child.name && (!child.namedBindings || (child.namedBindings.kind === K.NamedImports && child.namedBindings.elements.length === 0))) return false;
    return true;
  });
}
function leaf(node) {
  switch (node.kind) {
    case K.Identifier: case K.PrivateIdentifier: return node.text;
    case K.StringLiteral: return node.text;
    // A template's line endings are always read as LF, however the file stores them.
    case K.NoSubstitutionTemplateLiteral: case K.TemplateHead: case K.TemplateMiddle: case K.TemplateTail: return JSON.stringify([node.text, node.rawText]).replaceAll("\\r\\n", "\\n");
    case K.NumericLiteral: return String(Number(node.text));
    case K.BigIntLiteral: return String(BigInt(node.text.replace(/n$/, "").replaceAll("_", "")));
    case K.RegularExpressionLiteral: return node.text;
    // `import defer` is a field of the clause, not a child.
    case K.ImportClause: return node.phaseModifier ? K[node.phaseModifier] : undefined;
    // Operators and keywords these nodes hold as fields, not as child tokens.
    case K.PrefixUnaryExpression: case K.PostfixUnaryExpression: return K[node.operator];
    case K.HeritageClause: return K[node.token];
    case K.MetaProperty: return K[node.keywordToken];
    default: return undefined;
  }
}
// `{ a }` is `{ a: a }`. `{ a = 1 }` in a destructuring target has no such long form
// here, so it is compared as written, default included.
const isPlainShorthand = (node) => node.kind === K.ShorthandPropertyAssignment && !node.objectAssignmentInitializer;
const kindOf = (node) => (isPlainShorthand(node) ? K.PropertyAssignment : node.kind);
const parts = (node) => (isPlainShorthand(node) ? [node.name, node.name] : children(node));
const show = (node, file) => `${K[node.kind]} «${node.getText(file).slice(0, 90).replace(/\s+/g, " ")}»`;

function diff(x, y, fx, fy, px, py) {
  x = simplify(x, px); y = simplify(y, py);
  if (kindOf(x) !== kindOf(y)) return `kind: ${show(x, fx)}  vs  ${show(y, fy)}`;
  if (leaf(x) !== leaf(y)) return `text: ${show(x, fx)}  vs  ${show(y, fy)}`;
  if (x.kind === K.VariableDeclarationList && (x.flags & ts.NodeFlags.BlockScoped) !== (y.flags & ts.NodeFlags.BlockScoped)) return `var/let/const: ${show(x, fx)}  vs  ${show(y, fy)}`;
  const cx = parts(x), cy = parts(y);
  if (cx.length !== cy.length) return `children ${cx.length} vs ${cy.length}: ${show(x, fx)}  vs  ${show(y, fy)}`;
  for (let i = 0; i < cx.length; i++) {
    const found = diff(cx[i], cy[i], fx, fy, x, y);
    if (found) return found;
  }
  return null;
}
const walk = (dir, base = dir) => readdirSync(dir, { withFileTypes: true }).flatMap((entry) =>
  entry.isDirectory() ? walk(join(dir, entry.name), base) : /\.m?js$/.test(entry.name) ? [join(dir, entry.name).slice(base.length + 1)] : []);
const parse = (path) => ts.createSourceFile(path, readFileSync(path, "utf8"), ts.ScriptTarget.ESNext, true, ts.ScriptKind.JS);

let same = 0;
const differences = [];
const files = new Set([...walk(first), ...walk(second)]);
for (const file of [...files].sort()) {
  if (!existsSync(join(first, file)) || !existsSync(join(second, file))) { differences.push([file, "present in only one tree"]); continue; }
  const fx = parse(join(first, file)), fy = parse(join(second, file));
  if (fx.parseDiagnostics.length || fy.parseDiagnostics.length) { differences.push([file, "does not parse"]); continue; }
  // A hashbang is not in the tree.
  const hashbang = (source) => (source.text.startsWith("#!") ? source.text.split("\n", 1)[0].trimEnd() : "");
  if (hashbang(fx) !== hashbang(fy)) { differences.push([file, `hashbang: ${hashbang(fx)}  vs  ${hashbang(fy)}`]); continue; }
  const found = diff(fx, fy, fx, fy);
  if (found) differences.push([file, found]); else same++;
}
console.log(`${same} files have the same syntax tree, ${differences.length} differ`);
for (const [file, found] of differences) console.log(`- ${file}\n    ${found}`);
process.exit(differences.length ? 1 : 0);
