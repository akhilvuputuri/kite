#!/usr/bin/env bash
# Runs the same dependency command with npm and with kite in lockfile-only
# mode and diffs the resulting package-lock.json and package.json.
#
#   harness/resolve-parity.sh <fixture-dir> <kite-binary> <scenario> <command> [args...]
#
# <command> is install, uninstall or update. The fixture may omit
# package-lock.json to test a fresh resolution. Set RESOLVE_DROP_LOCK=1 to
# delete the lockfile before running.
set -euo pipefail

fixture=$(cd "$1" && pwd)
kite=$(cd "$(dirname "$2")" && pwd)/$(basename "$2")
scenario=$3
shift 3
work=${PARITY_WORK:-$(mktemp -d)}
mkdir -p "$work"

prepare() {
  local dest=$1
  rm -rf "$dest"
  cp -R "$fixture" "$dest"
  rm -rf "$dest/node_modules"
  if [ "${RESOLVE_DROP_LOCK:-}" = 1 ]; then
    rm -f "$dest/package-lock.json"
  fi
}

npm_dir=$work/$scenario-npm
ls_dir=$work/$scenario-kite
prepare "$npm_dir"
prepare "$ls_dir"

start=$(node -e 'console.log(Date.now())')
(cd "$npm_dir" && npm "$@" --package-lock-only --ignore-scripts --no-audit --no-fund --loglevel=error >/dev/null)
mid=$(node -e 'console.log(Date.now())')
(cd "$ls_dir" && "$kite" --quiet "$@" --lockfile-only)
end=$(node -e 'console.log(Date.now())')
echo "npm: $((mid - start)) ms, kite: $((end - mid)) ms" >&2

status=0
for f in package-lock.json package.json; do
  if ! diff -u "$npm_dir/$f" "$ls_dir/$f" >"$work/$scenario.$f.diff"; then
    echo "resolve parity: $scenario $f — DIFFERENT ($(grep -c '^[-+][^-+]' "$work/$scenario.$f.diff") lines), see $work/$scenario.$f.diff"
    status=1
  fi
done
if [ $status = 0 ]; then
  echo "resolve parity: $scenario ($*) — identical"
fi
exit $status
