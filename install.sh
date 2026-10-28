#!/bin/sh
# Installs the latest kite release (or KITE_VERSION) after checking its
# SHA-256 against the published checksum.
#
#   curl -fsSL https://raw.githubusercontent.com/akhilvuputuri/kite/main/install.sh | sh
#
# KITE_INSTALL_DIR defaults to ~/.local/bin.
set -eu

main() {
repo=akhilvuputuri/kite
dir=${KITE_INSTALL_DIR:-$HOME/.local/bin}
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) asset=kite-linux-x64 ;;
  Linux-aarch64 | Linux-arm64) asset=kite-linux-arm64 ;;
  Darwin-arm64) asset=kite-darwin-arm64 ;;
  *) echo "kite: no release build for $(uname -s) $(uname -m)" >&2; exit 1 ;;
esac
if [ -n "${KITE_VERSION:-}" ]; then
  base="https://github.com/$repo/releases/download/v${KITE_VERSION#v}"
else
  base="https://github.com/$repo/releases/latest/download"
fi
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
curl -fsSL -o "$tmp/$asset" "$base/$asset"
curl -fsSL -o "$tmp/$asset.sha256" "$base/$asset.sha256"
want=$(cut -d' ' -f1 "$tmp/$asset.sha256")
case "$want" in
  *[!0-9a-f]* | "") echo "kite: malformed checksum file for $asset" >&2; exit 1 ;;
esac
if [ "${#want}" -ne 64 ]; then echo "kite: malformed checksum file for $asset" >&2; exit 1; fi
if command -v sha256sum >/dev/null; then
  got=$(sha256sum "$tmp/$asset" | cut -d' ' -f1)
else
  got=$(shasum -a 256 "$tmp/$asset" | cut -d' ' -f1)
fi
if [ "$want" != "$got" ]; then
  echo "kite: checksum mismatch for $asset (expected $want, got $got)" >&2
  exit 1
fi
mkdir -p "$dir"
chmod 755 "$tmp/$asset"
mv "$tmp/$asset" "$dir/kite"
echo "kite $("$dir/kite" --version) installed to $dir/kite"
echo "To check where it was built: gh attestation verify $dir/kite -R $repo"
case ":$PATH:" in *":$dir:"*) ;; *) echo "Add $dir to your PATH." ;; esac
}

# Defined first and called last, so a truncated `curl | sh` runs nothing.
main "$@"
