# Changelog

## 0.1.3

- Multi-step Docker builds work on Docker's containerd image store (the default for a fresh
  Docker 29 install). `kite ci` in one build step followed by `kite ci --omit=dev` in the next
  failed there: image layers keep file times only to the second, so Kite treated its store as
  modified and then could not rename the store folder, because overlayfs without `redirect_dir`
  refuses to rename a folder from a lower layer. Kite now accepts times kept to the second, and
  removes a store entry in place when it cannot be renamed.
- The later step reuses the store instead of downloading every package again.
- New CI check: a two-step image build on the containerd image store with the legacy builder.

## 0.1.2

- `kite install`, `uninstall` and `update` now work like `npm install`: they compare the installed
  tree with the lockfile and only add, replace or remove the packages that differ. Unchanged
  packages and anything else in `node_modules` (such as `.cache`) are left alone, and install
  scripts run only for added or changed packages. The result stays byte-identical to npm's.
- The installed tree comes from `node_modules/.package-lock.json` when npm's freshness rules
  allow (nothing newer, every package listed and present, dot folders ignored). Otherwise each
  package's `package.json` is read, with integrity taken from `package-lock.json` where the
  versions match, as npm does. Deleting one package by hand reinstalls just that package.
- A repeated install with nothing to change takes about 40 ms on a 300-package project, against
  about 430 ms for npm.
- Replacing a linked package with a registry package never touches the link target's folder.
  A linked folder's own `node_modules` is only checked while the link to it exists, as in npm.
- On case-insensitive filesystems (macOS), a package whose folder name changed case is kept.
- `kite ci` is unchanged: a clean install.

## 0.1.1

lockstep is now **Kite**: the repository is `akhilvuputuri/kite`, the command is `kite` and the
crate is `kite-pm` (npm and Homebrew packages to follow). Old repository and release links
redirect.

- Renamed environment variables: `KITE_STORE` (`LOCKSTEP_STORE` is still read in this release),
  `KITE_NETWORK_CONCURRENCY`, `KITE_TIMING`. The default store moves to `~/.cache/kite`
  (`~/Library/Caches/kite` on macOS); the old `lockstep` cache directory can be deleted.
- Downloads keep 64 requests in flight regardless of core count (it was 4 per CPU, so 16 on
  4-CPU CI runners).
- `node --version` is asked once per run and only when an engines check needs it.
- An install script verifies the release checksum.
- Release binaries carry GitHub build provenance attestations, and workflow actions are pinned
  to commits.

## 0.1.0

Released as lockstep.

The first release: a drop-in replacement for npm's install, dependency and script commands,
with byte-identical results.

- `lockstep ci`: clean install from `package-lock.json` (lockfileVersion 3) into the same
  `node_modules` tree as `npm ci`, through a verified content-addressable store. It supports
  `--omit`, `--ignore-scripts`, install scripts, bins, `file:` links and platform-specific
  optional packages, and refuses out-of-sync lockfiles the way npm does.
- `lockstep install`, `uninstall` and `update`: npm's resolver (arborist's ideal tree, peer
  sets and dependency flags) writing the same `package-lock.json` and `package.json` as npm.
- `lockstep run`, `test`, `start` and `exec`: package scripts and local binaries with npm's
  environment, `PATH`, pre/post scripts, exit codes and signals.
- Static Linux binaries (x64, arm64) and a macOS arm64 binary, built reproducibly, with
  SHA-256 checksums.
