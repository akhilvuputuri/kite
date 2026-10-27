# lockstep

A small, lockfile-exact JavaScript package installer and script runner, written in Rust.
It reads and writes npm's own `package-lock.json` (lockfileVersion 3) and produces the same
`node_modules` tree as `npm ci`, byte for byte.

It exists to replace npm in [Chief](https://github.com/akhilvuputuri/chief-agent): its CI, Docker
images, coding sandboxes and local development (see chief-agent issue #330). It is deliberately
narrow. It is not a general npm replacement, and anything outside its scope fails loudly rather
than falling back to npm.

## Commands

```
lockstep [--prefix DIR] ci [--omit=dev|optional|peer]... [--ignore-scripts]
lockstep [--prefix DIR] run [--silent] [--if-present] [--ignore-scripts] <script> [--] [args...]
lockstep [--prefix DIR] test [args...]
lockstep exec <bin> [--] [args...]
```

- **`ci`** installs exactly what `package-lock.json` describes. It verifies each tarball's
  `integrity` before unpacking anything, and keeps verified, extracted packages in a
  content-addressable store (`$LOCKSTEP_STORE`, or the user cache directory), so a warm install
  links files instead of unpacking tarballs: `clonefile` on macOS, hard links on Linux.
- **`run`** runs a `package.json` script through `sh -c` with npm's environment, `PATH`, banner,
  pre/post scripts, exit codes and signal handling.
- **`exec`** runs a binary from the nearest `node_modules/.bin`. It never downloads anything.

## Compatibility with npm

`harness/parity.sh` installs a fixture with `npm ci` and with `lockstep ci` and diffs every path,
file type, mode, symlink target and content hash. CI runs it for each fixture on Linux x64,
Linux arm64 and macOS arm64, with and without `--omit=dev` and `--ignore-scripts`, against the npm
bundled with Node 22.23.4 (npm 10.9.9).

Behaviour follows npm's source:

- Only file entries of a tarball are written, with mode `(mode | 0o666) & ~umask`; directories
  are `0o777 & ~umask`. `.gitignore` becomes `.npmignore` the way pacote does it.
- Bins come from the lockfile entry and are linked into the `.bin` of the `node_modules` folder
  that holds the package. The first claim wins (depth, then ICU path order), and targets are made
  executable with their CRLF shebang fixed, as bin-links does.
- Platform and engines checks apply to optional packages only; a failing optional package is
  removed with everything only it needed (arborist's `optionalSet`).
- `--omit` applies only under the root `node_modules`; a linked local package's dependencies are
  always installed, as npm does.
- `node_modules/.package-lock.json` is rebuilt with arborist's field list and key order.
- Install scripts run as in arborist's rebuild: `preinstall` for all packages, then bin links,
  then `install`, then `postinstall`; then the root's lifecycle scripts.

Known differences, by design:

- Only `https://registry.npmjs.org/` tarballs and `file:` links are supported. Bundled
  dependencies, `npm-shrinkwrap.json`, git and tarball-URL sources fail with a clear error.
- `lockstep run` passes every argument after the script name to the script; npm swallows
  dash-arguments before `--` as its own config.
- `lockstep exec` runs the binary directly, without a shell, so arguments are never globbed.
- npm's own `npm_config_*` variables are not set for scripts.

## License

MIT
