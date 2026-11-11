#!/usr/bin/env bash
# Compiles each TypeScript fixture project with `tsc` and with `kite build`, then compares
# the JavaScript by syntax tree and the source maps by the names they carry. Exits
# non-zero on any difference.
#
#   harness/build-parity.sh <kite-binary> [fixture-dir...]
#
# Fixtures default to every folder in tests/fixtures/typescript. tsc is the pinned
# version below, installed with npm into the work folder.
set -euo pipefail

TYPESCRIPT_VERSION=5.9.3

kite=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
shift
here=$(cd "$(dirname "$0")" && pwd)
work=${PARITY_WORK:-$(mktemp -d)}
mkdir -p "$work"
work=$(cd "$work" && pwd)
if [ $# -eq 0 ]; then
  set -- "$here"/../tests/fixtures/typescript/*/
fi

if [ "$(node -e 'try { console.log(require(process.argv[1]).version) } catch { console.log("") }' "$work/node_modules/typescript/package.json")" != "$TYPESCRIPT_VERSION" ]; then
  (cd "$work" && npm install --no-save --no-audit --no-fund --loglevel=error "typescript@$TYPESCRIPT_VERSION" >/dev/null)
fi

# The comparison must notice a changed operator, or it proves nothing.
check=$work/self-check
rm -rf "$check" && mkdir -p "$check/a" "$check/b"
echo 'export const a = -1 + (2);' > "$check/a/x.js"
echo 'export const a = -1 + 2;' > "$check/b/x.js"
(cd "$work" && node "$here/ast-diff.mjs" "$check/a" "$check/b" >/dev/null) || { echo "ast-diff rejects equal code" >&2; exit 1; }
echo 'export const a = +1 + 2;' > "$check/b/x.js"
if (cd "$work" && node "$here/ast-diff.mjs" "$check/a" "$check/b" >/dev/null); then
  echo "ast-diff misses a changed operator" >&2
  exit 1
fi

failed=0
for fixture in "$@"; do
  name=$(basename "$fixture")
  for tool in tsc kite; do
    rm -rf "$work/$name-$tool"
    cp -R "$fixture" "$work/$name-$tool"
    rm -rf "$work/$name-$tool/dist"
  done
  # tsc type-checks too, so a fixture that is not valid TypeScript fails here.
  (cd "$work/$name-tsc" && "$work/node_modules/.bin/tsc" -p .)
  (cd "$work/$name-kite" && "$kite" --quiet build)
  if ! (cd "$work" && node "$here/ast-diff.mjs" "$name-tsc/dist" "$name-kite/dist"); then
    failed=1
  fi
  # Both must write the same files, and their maps must name the same output and source.
  if ! diff <(cd "$work/$name-tsc/dist" && find . -type f | sort) <(cd "$work/$name-kite/dist" && find . -type f | sort); then
    echo "$name: the two builds wrote different files" >&2
    failed=1
  fi
  node -e '
    const fs = require("node:fs"), path = require("node:path");
    const [a, b] = process.argv.slice(1);
    const maps = (dir) => fs.readdirSync(dir, { recursive: true }).map(String).filter((f) => f.endsWith(".map")).sort();
    let bad = 0;
    for (const file of maps(a)) {
      const pick = (dir) => { const m = JSON.parse(fs.readFileSync(path.join(dir, file), "utf8")); return JSON.stringify([m.version, m.file, m.sourceRoot, m.sources, typeof m.mappings, "sourcesContent" in m]); };
      if (pick(a) !== pick(b)) { console.error(`${file}: ${pick(a)} vs ${pick(b)}`); bad = 1; }
    }
    process.exit(bad);
  ' "$work/$name-tsc/dist" "$work/$name-kite/dist" || failed=1
  echo "$name: compared"
done
exit $failed
