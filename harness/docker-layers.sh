#!/usr/bin/env bash
# An image build that installs in one step and prunes to production in the
# next, like Chief's Dockerfile: `kite ci`, then `kite ci --omit=dev` in a
# new layer. The second step must succeed and reuse the store from the layer
# below without downloading.
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
RUN kite ci --omit=dev 2>&1 | tee /tmp/omit.out && grep -Eq ', 0 downloaded\)' /tmp/omit.out
DOCKERFILE
docker build -t kite-docker-layers "$ctx"
docker rmi kite-docker-layers >/dev/null
echo "docker layers: the second step reused the store"
