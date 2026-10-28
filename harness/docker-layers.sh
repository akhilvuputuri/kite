#!/usr/bin/env bash
# An image build that installs in one step and prunes to production in the
# next, like Chief's Dockerfile: `kite ci`, then `kite ci --omit=dev` in a
# new layer. The second step must succeed and reuse the store from the layer
# below without downloading. Later steps damage a store entry and replace a
# package with nested node_modules, both from lower layers.
#
#   harness/docker-layers.sh <kite-binary> <fixture-dir>
#
# Set DOCKER_BUILDKIT=0 for the legacy builder (Ubuntu's docker.io has no
# buildx).
set -euo pipefail
kite=$1
fixture=$2
ctx=$(mktemp -d)
trap 'rm -rf "$ctx"' EXIT
cp -R "$fixture"/. "$ctx/"
cp "$kite" "$ctx/kite"
cat >"$ctx/Dockerfile" <<'DOCKERFILE'
FROM node:22.23.4-bookworm-slim
COPY kite /usr/local/bin/kite
WORKDIR /app
COPY . .
RUN kite ci
# The store from the layer below is reused as it is.
RUN kite ci --omit=dev 2>&1 | tee /tmp/omit.out && grep -q '(0 downloaded)' /tmp/omit.out
# A damaged entry from a lower layer is discarded in place and fetched again.
RUN e=$(dirname "$(ls -d /root/.cache/kite/v2/*/*/linked | head -1)") \
 && printf x >> "$e/package/package.json" \
 && kite ci 2>&1 | tee /tmp/damaged.out && grep -q '(1 downloaded)' /tmp/damaged.out
# Replacing a package keeps its nested node_modules from the layer below.
RUN sed -i 's/"version": "[^"]*"/"version": "0.0.0-changed"/' node_modules/express/package.json \
 && ls node_modules/express/node_modules >/tmp/nested.before \
 && kite install 2>&1 | tee /tmp/install.out && grep -q ', 1 placed, 0 removed (0 downloaded)' /tmp/install.out \
 && ls node_modules/express/node_modules | diff /tmp/nested.before - \
 && ! grep -q 0.0.0-changed node_modules/express/package.json
DOCKERFILE
docker build -t kite-docker-layers "$ctx"
docker rmi kite-docker-layers >/dev/null
echo "docker layers: store reused, damaged entry replaced, nested node_modules kept"
