# Kite

**A fast JavaScript package manager written in Rust that is a drop-in replacement for npm.**

kite installs, resolves and runs JavaScript projects using npm's own `package-lock.json`.
Unlike other npm alternatives, it does not convert your lockfile or build a "similar" tree.
It produces **exactly** what npm 10 produces, byte for byte, and gets there faster.

```sh
kite ci                      # npm ci
kite install                 # npm install
kite install zod@^3 -D       # npm install zod@^3 --save-dev
kite uninstall left-pad      # npm uninstall left-pad
kite update [pkg...]         # npm update
kite run build -- --watch    # npm run build -- --watch
kite test                    # npm test
kite exec tsc --noEmit       # npx tsc --noEmit
```

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/akhilvuputuri/kite/main/install.sh | sh
```

The script downloads the release binary for your platform (Linux x64, Linux arm64 or macOS
arm64) and checks its SHA-256 before installing it to `~/.local/bin`. Binaries and checksums are
also on the [releases page](https://github.com/akhilvuputuri/kite/releases). npm
(`npm i -g kite-pm`), Homebrew and crates.io packages are on the way.

## Why Kite

- **Exactly npm's results.** The same `node_modules` tree, file for file and mode for mode.
  The same `package-lock.json`, byte for byte. The same `package.json` edits. You can switch
  a project, a CI pipeline or a single developer without touching the lockfile, and switch back
  at any time.
- **Fast.** Verified packages are kept extracted in a content-addressable store, so a warm
  install clones (`clonefile` on macOS) or hard-links (Linux) files instead of unpacking
  tarballs. Registry metadata is fetched in parallel and cached with HTTP revalidation.
- **Verifiable releases.** Every release binary is built twice and must be byte-identical, is
  checked against npm before publishing, and carries a SHA-256 checksum and, from 0.1.1, a GitHub build
  provenance attestation (`gh attestation verify kite-linux-x64 -R akhilvuputuri/kite`).
- **One self-contained binary.** No Node.js is needed to install kite itself, and it has no
  runtime dependencies beyond the system C library. Release builds ship for Linux x64,
  Linux arm64 and macOS arm64, each with a SHA-256 checksum.
- **Strict integrity.** Every tarball is hashed against the lockfile's `integrity` as it streams
  in, and its files only become a store entry once the whole tarball matched. Store entries that
  were ever hard-linked into a project are checked (mode, size and modification time of every
  file) before each reuse and re-downloaded if anything changed. This catches ordinary in-place
  edits through `node_modules`; it is not a defence against someone deliberately rewriting a file
  and restoring its timestamp. On macOS, packages are cloned copy-on-write instead, so edits in
  `node_modules` cannot reach the store at all.

## Kite or Bun?

Both are fast, native package managers. They make different promises.

**Choose Kite** when you want npm's exact results, only faster:
- It keeps `package-lock.json` and produces npm's `node_modules` byte for byte. Bun converts to
  `bun.lock` and builds a slightly different tree. Teammates, CI and anything else that reads
  `package-lock.json` keep working, and you can switch back to npm at any time.
- Warm installs, which CI caches and Docker rebuilds mostly hit, are faster than Bun's.
- Dependency install scripts run exactly as npm runs them. Bun skips them for packages that
  are not on its trusted list.
- Anything it cannot reproduce exactly is refused with a clear error instead of approximated.

**Choose Bun** when you want its wider toolkit: it is also a runtime, test runner and bundler,
it supports workspaces, overrides and Windows, and its cold installs are still faster.

Clean install of a 367-package lockfile on GitHub's Linux x64 runners (4 CPUs, median of 5,
scripts off, [bench workflow](.github/workflows/bench.yml)):

| | cold | warm |
| --- | ---: | ---: |
| npm 10.9 | ~9 s | ~7 s |
| **Kite** | 1.35 s | **0.29 s** |
| Bun 1.2 | **0.96 s** | 0.41 s |

## Proven compatibility

Every change is checked against npm itself (npm 10.9.9, the npm in Node 22.23.4) on Linux x64,
Linux arm64 and macOS:

- **Installs.** `harness/parity.sh` installs a project with `npm ci` and with `kite ci`,
  then diffs every path, file type, permission, symlink target and content hash, including
  `node_modules/.package-lock.json`. The checks run with and without `--omit=dev` and
  `--ignore-scripts`. They cover real projects built on Next.js, NestJS, Angular, Storybook,
  Vite/React, webpack, Jest/Babel, Prisma, Express and ESLint, plus local `file:` packages and
  CRLF/tab-formatted manifests. The result is zero differences.
- **Resolution.** `harness/resolve-parity.sh` runs the same command with
  `npm ... --package-lock-only` and with kite, then diffs `package-lock.json` and
  `package.json`. It covers fresh resolutions, no-op installs, adds (prod, dev, exact, tags,
  ranges, peer-heavy packages), uninstalls and updates. The result is identical output, and
  identical `ERESOLVE` refusals where npm refuses.
- **Semantics.** `harness/fuzz.mjs` compares kite's semver engine with node-semver and its
  key ordering with Node's ICU collator, over tens of thousands of random cases.

kite follows npm down to its quirks: arborist's placement and peer-set rules, the
order-dependent dev/optional flag calculation, pacote's file modes and `.gitignore` renaming,
bin-links' conflict order and CRLF shebang fix, the lifecycle script phases, and the validation
`npm ci` uses to reject an out-of-sync lockfile.

## Commands

| Command | Does |
| --- | --- |
| `ci [--omit=dev\|optional\|peer] [--ignore-scripts]` | Clean install from `package-lock.json`, refusing if it is out of sync with `package.json` |
| `install [pkg[@spec]...] [-D\|-O\|-P\|--save-peer] [-E] [--lockfile-only]` | Resolve, update `package.json` and `package-lock.json`, then install |
| `uninstall <pkg>... [--lockfile-only]` | Remove dependencies and prune what only they needed |
| `update [pkg...] [--lockfile-only]` | Re-resolve within the ranges in `package.json` |
| `run <script> [--] [args...]` | Run a `package.json` script with npm's environment, `PATH`, pre/post scripts and exit codes |
| `test`, `start` | Shortcuts for `run test` and `run start` |
| `exec <bin> [--] [args...]` | Run a binary from `node_modules/.bin` |

Global options: `--prefix DIR` (or `-C DIR`), `--store DIR` (default `$KITE_STORE` or the
user cache directory), `--jobs N`, `--quiet`. `install` also takes `--before DATE`.

## Roadmap

These are not supported yet. When a project needs one, kite says so clearly instead of
guessing:

- workspaces and `overrides`;
- registries other than registry.npmjs.org, and `.npmrc` settings;
- git, tarball-URL and `file:` tarball dependencies;
- packages that ship `bundleDependencies` or `npm-shrinkwrap.json`;
- `publish`, `audit` and other npm commands outside install and run.

## Notes

- `kite exec` runs the binary directly instead of through a shell, so arguments are passed
  exactly as given and never globbed.
- `kite run x --flag` passes `--flag` to the script. npm would treat it as its own option.

## License

MIT
