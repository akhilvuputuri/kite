#!/usr/bin/env bash
# Installs a fixture with `npm ci` and with `kite ci`, snapshots both
# trees and diffs them. Exits non-zero on any difference.
#
#   harness/parity.sh <fixture-dir> <kite-binary> [ci flags...]
#
# The fixture holds package.json, package-lock.json and any linked local
# packages. Snapshot roots: node_modules plus every <dir>/node_modules of a
# local (non-node_modules) lockfile entry.
set -euo pipefail

fixture=$(cd "$1" && pwd)
kite=$(cd "$(dirname "$2")" && pwd)/$(basename "$2")
shift 2
here=$(cd "$(dirname "$0")" && pwd)
work=${PARITY_WORK:-$(mktemp -d)}
mkdir -p "$work"
name=$(basename "$fixture")

roots=$(node -e '
  const lock = require(process.argv[1]);
  const roots = ["node_modules"];
  for (const k of Object.keys(lock.packages)) {
    if (k && !k.includes("node_modules/") && !lock.packages[k].link) roots.push(k + "/node_modules");
  }
  console.log(roots.join(" "));
' "$fixture/package-lock.json")

install() {
  local tool=$1
  shift
  local dest=$work/$name-$tool
  rm -rf "$dest"
  cp -R "$fixture" "$dest"
  local start end
  start=$(node -e 'console.log(Date.now())')
  local status=0
  if [ "$tool" = npm ]; then
    (cd "$dest" && npm ci --no-audit --no-fund --loglevel=error "$@" >/dev/null 2>"$work/$name-npm.err") || status=$?
  else
    (cd "$dest" && "$kite" ci --quiet "$@" 2>"$work/$name-kite.err") || status=$?
  fi
  echo "$status" >"$work/$name-$tool.status"
  if [ "$status" != 0 ]; then
    return 0
  fi
  end=$(node -e 'console.log(Date.now())')
  echo "$tool: $((end - start)) ms" >&2
  # shellcheck disable=SC2086
  node "$here/snapshot.mjs" "$dest" $roots >"$work/$name-$tool.snap"
}

install npm "$@"
install kite "$@"

npm_status=$(cat "$work/$name-npm.status")
kite_status=$(cat "$work/$name-kite.status")
if [ "$npm_status" != 0 ] || [ "$kite_status" != 0 ]; then
  # Refusing the same lockfile for the same reasons is parity too.
  problems() { grep -oE '(Invalid|Missing): .*' "$1" | sed 's/^npm error //' | sort; }
  # Both must fail with the same, non-empty list of lockfile problems; two
  # unrelated failures (a network error, say) are not parity.
  if [ "$npm_status" != 0 ] && [ "$kite_status" != 0 ] &&
    [ -n "$(problems "$work/$name-npm.err")" ] &&
    diff <(problems "$work/$name-npm.err") <(problems "$work/$name-kite.err") >/dev/null; then
    echo "parity: $name $* — both refuse the lockfile with the same problems"
    exit 0
  fi
  echo "parity: $name $* — DIFFERENT outcome (npm exit $npm_status, kite exit $kite_status)"
  tail -5 "$work/$name-npm.err" "$work/$name-kite.err" || true
  exit 1
fi

if diff -u "$work/$name-npm.snap" "$work/$name-kite.snap" >"$work/$name.diff"; then
  echo "parity: $name $* — identical ($(wc -l <"$work/$name-npm.snap" | tr -d ' ') entries)"
else
  echo "parity: $name $* — DIFFERENT ($(grep -c '^[-+][^-+]' "$work/$name.diff") lines), see $work/$name.diff"
  exit 1
fi
