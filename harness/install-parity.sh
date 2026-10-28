#!/usr/bin/env bash
# Starts from the same installed tree (npm ci), then runs `npm install ...`
# and `kite install ...` on separate copies and diffs node_modules (with
# every link target's node_modules), package.json and package-lock.json.
#
#   harness/install-parity.sh <fixture-dir> <kite-binary> <scenario> [install args...]
#
# CMD selects the command (install, uninstall, update; default install).
# PREPARE is a shell snippet run in each copy before the command.
set -euo pipefail
fixture=$(cd "$1" && pwd)
kite=$(cd "$(dirname "$2")" && pwd)/$(basename "$2")
scenario=$3
shift 3
here=$(cd "$(dirname "$0")" && pwd)
work=${PARITY_WORK:-$(mktemp -d)}
mkdir -p "$work"
base=$work/$scenario-base
rm -rf "$base" && cp -R "$fixture" "$base"
(cd "$base" && npm ci --no-audit --no-fund --loglevel=error >/dev/null)
roots() {
  node -e '
    const lock = require(process.argv[1]);
    const roots = ["node_modules"];
    for (const k of Object.keys(lock.packages)) if (k && !k.includes("node_modules/") && !lock.packages[k].link) roots.push(k + "/node_modules");
    console.log(roots.join(" "));
  ' "$1/package-lock.json"
}
for tool in npm kite; do
  dest=$work/$scenario-$tool
  rm -rf "$dest" && cp -Rp "$base" "$dest"
  start=$(node -e 'console.log(Date.now())')
  if [ "$tool" = npm ]; then
    (cd "$dest" && eval "${PREPARE:-true}" && npm "${CMD:-install}" --no-audit --no-fund --loglevel=error "$@" >/dev/null)
  else
    (cd "$dest" && eval "${PREPARE:-true}" && "$kite" "${CMD:-install}" --quiet "$@")
  fi
  end=$(node -e 'console.log(Date.now())')
  echo "$tool install: $((end - start)) ms" >&2
  # shellcheck disable=SC2046
  node "$here/snapshot.mjs" "$dest" $(roots "$dest") >"$work/$scenario-$tool.snap"
  for f in package.json package-lock.json; do cp "$dest/$f" "$work/$scenario-$tool.$f"; done
done
status=0
for f in snap package.json package-lock.json; do
  if ! diff -u "$work/$scenario-npm.$f" "$work/$scenario-kite.$f" >"$work/$scenario.$f.diff"; then
    echo "install parity: $scenario $f — DIFFERENT, see $work/$scenario.$f.diff"
    status=1
  fi
done
[ $status = 0 ] && echo "install parity: $scenario ($*) — identical"
exit $status
