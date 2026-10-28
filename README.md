# lockstep

**A fast JavaScript package manager written in Rust that is a drop-in replacement for npm.**

lockstep installs, resolves and runs JavaScript projects using npm's own `package-lock.json`.
Unlike other npm alternatives, it does not convert your lockfile or build a "similar" tree.
It produces **exactly** what npm 10 produces, byte for byte, and gets there faster.

```sh
lockstep ci                      # npm ci
lockstep install                 # npm install
lockstep install zod@^3 -D       # npm install zod@^3 --save-dev
lockstep uninstall left-pad      # npm uninstall left-pad
lockstep update [pkg...]         # npm update
lockstep run build -- --watch    # npm run build -- --watch
lockstep test                    # npm test
lockstep exec tsc --noEmit       # npx tsc --noEmit
```

## Why lockstep

- **Exactly npm's results.** The same `node_modules` tree, file for file and mode for mode.
  The same `package-lock.json`, byte for byte. The same `package.json` edits. You can switch
  a project, a CI pipeline or a single developer without touching the lockfile, and switch back
  at any time.
- **Fast.** Verified packages are kept extracted in a content-addressable store, so a warm
  install clones (`clonefile` on macOS) or hard-links (Linux) files instead of unpacking
  tarballs. Registry metadata is fetched in parallel and cached with HTTP revalidation.
- **Verifiable releases.** Every release binary is built twice and must be byte-identical, is
  checked against npm before publishing, and carries a SHA-256 checksum and, from 0.1.1, a GitHub build
  provenance attestation (`gh attestation verify lockstep-linux-x64 -R akhilvuputuri/lockstep`).
- **One self-contained binary.** No Node.js is needed to install lockstep itself, and it has no
  runtime dependencies beyond the system C library. Release builds ship for Linux x64,
  Linux arm64 and macOS arm64, each with a SHA-256 checksum.
- **Strict integrity.** Every tarball is hashed against the lockfile's `integrity` as it streams
  in, and its files only become a store entry once the whole tarball matched. Store entries that
  were ever hard-linked into a project are checked (mode, size and modification time of every
  file) before each reuse and re-downloaded if anything changed. This catches ordinary in-place
  edits through `node_modules`; it is not a defence against someone deliberately rewriting a file
  and restoring its timestamp. On macOS, packages are cloned copy-on-write instead, so edits in
  `node_modules` cannot reach the store at all.

## Proven compatibility

Every change is checked against npm itself (npm 10.9.9, the npm in Node 22.23.4) on Linux x64,
Linux arm64 and macOS:

- **Installs.** `harness/parity.sh` installs a project with `npm ci` and with `lockstep ci`,
  then diffs every path, file type, permission, symlink target and content hash, including
  `node_modules/.package-lock.json`. The checks run with and without `--omit=dev` and
  `--ignore-scripts`. They cover real projects built on Next.js, NestJS, Angular, Storybook,
  Vite/React, webpack, Jest/Babel, Prisma, Express and ESLint, plus local `file:` packages and
  CRLF/tab-formatted manifests. The result is zero differences.
- **Resolution.** `harness/resolve-parity.sh` runs the same command with
  `npm ... --package-lock-only` and with lockstep, then diffs `package-lock.json` and
  `package.json`. It covers fresh resolutions, no-op installs, adds (prod, dev, exact, tags,
  ranges, peer-heavy packages), uninstalls and updates. The result is identical output, and
  identical `ERESOLVE` refusals where npm refuses.
- **Semantics.** `harness/fuzz.mjs` compares lockstep's semver engine with node-semver and its
  key ordering with Node's ICU collator, over tens of thousands of random cases.

lockstep follows npm down to its quirks: arborist's placement and peer-set rules, the
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

Global options: `--prefix DIR` (or `-C DIR`), `--store DIR` (default `$LOCKSTEP_STORE` or the
user cache directory), `--jobs N`, `--quiet`. `install` also takes `--before DATE`.

## Roadmap

These are not supported yet. When a project needs one, lockstep says so clearly instead of
guessing:

- workspaces and `overrides`;
- registries other than registry.npmjs.org, and `.npmrc` settings;
- git, tarball-URL and `file:` tarball dependencies;
- packages that ship `bundleDependencies` or `npm-shrinkwrap.json`;
- `publish`, `audit` and other npm commands outside install and run.

## Notes

- `lockstep exec` runs the binary directly instead of through a shell, so arguments are passed
  exactly as given and never globbed.
- `lockstep run x --flag` passes `--flag` to the script. npm would treat it as its own option.

## License

MIT
