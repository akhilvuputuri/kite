#!/usr/bin/env bash
# Times cold and warm clean installs of a fixture with npm ci, lockstep ci,
# pnpm (after `pnpm import` of the same lockfile) and Bun (which converts
# package-lock.json itself). Prints a Markdown table.
#
#   harness/bench.sh <fixture-dir> <lockstep-binary> [runs]
#
# BENCH_TOOLS limits the tools measured (default: npm lockstep pnpm bun).
set -euo pipefail
fixture=$(cd "$1" && pwd)
lockstep=$(cd "$(dirname "$2")" && pwd)/$(basename "$2")
runs=${3:-3}
work=$(mktemp -d)
now() { node -e 'console.log(Date.now())'; }

median() { sort -n | awk '{a[NR]=$1} END {print a[int((NR+1)/2)]}'; }

# time_install <tool> <cold|warm>
time_install() {
  local tool=$1 mode=$2 cache=$work/cache-$tool dir=$work/$tool
  local times=()
  for _ in $(seq "$runs"); do
    rm -rf "$dir" && cp -R "$fixture" "$dir"
    [ "$mode" = cold ] && rm -rf "$cache"
    case $tool in
      npm) cmd=(npm ci --ignore-scripts --no-audit --no-fund --loglevel=error --cache "$cache") ;;
      lockstep) cmd=("$lockstep" ci --ignore-scripts --quiet --store "$cache") ;;
      pnpm)
        (cd "$dir" && pnpm import >/dev/null 2>&1 && rm -f package-lock.json)
        cmd=(pnpm install --frozen-lockfile --ignore-scripts --store-dir "$cache" --reporter=silent) ;;
      bun) cmd=(env BUN_INSTALL_CACHE_DIR="$cache" bun install --ignore-scripts --silent) ;;
    esac
    if [ "$mode" = warm ]; then
      # Fill the cache once, then measure a clean install from it.
      (cd "$dir" && "${cmd[@]}" >/dev/null 2>&1) || true
      rm -rf "$dir/node_modules"
    fi
    local start end
    start=$(now)
    (cd "$dir" && "${cmd[@]}" >/dev/null 2>&1)
    end=$(now)
    times+=($((end - start)))
  done
  printf '%s\n' "${times[@]}" | median
}

echo "| tool | cold (ms) | warm (ms) |"
echo "| --- | ---: | ---: |"
for tool in ${BENCH_TOOLS:-npm lockstep pnpm bun}; do
  if [ "$tool" != npm ] && [ "$tool" != lockstep ] && ! command -v "$tool" >/dev/null; then
    continue
  fi
  cold=$(time_install "$tool" cold)
  warm=$(time_install "$tool" warm)
  echo "| $tool | $cold | $warm |"
done
rm -rf "$work"
