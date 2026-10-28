# Changelog

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
