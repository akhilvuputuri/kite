# Changelog

## 0.1.0

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
