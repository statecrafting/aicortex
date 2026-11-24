#!/usr/bin/env bash
# The live identity fixture (spec 020 D-14): a real rauthy on loopback, run as
# a container, for every test that needs one (020 AC-2 and FR-009; 021 AC-2
# reuses it).
#
#   scripts/live-rauthy.sh up      start it, wait for health, print the
#                                  AICORTEX_TEST_RAUTHY_* variables to stdout
#   scripts/live-rauthy.sh down    remove it
#
# The image is the one rahi ships its cell with, pinned by digest (rahi
# docker/Dockerfile, ARG RAUTHY_IMAGE). Bump it here when rahi's pin moves.
#
# Nothing here is a credential of the product: the registration token, the
# bootstrap administrator, and the API key secret belong to a throwaway
# rauthy that lives for one run and are the fixture's own. The product mints
# nothing and accepts no static key (rahi://025, 020 D-14).
#
# Variables: AICORTEX_LIVE_PORT (default 8080), AICORTEX_LIVE_NAME (container
# name), AICORTEX_LIVE_IMAGE (override the pin, for a local mirror).

set -euo pipefail

PORT="${AICORTEX_LIVE_PORT:-8080}"
NAME="${AICORTEX_LIVE_NAME:-aicortex-rauthy-live}"
IMAGE="${AICORTEX_LIVE_IMAGE:-ghcr.io/bartekus/rauthy-patched:0.36.2-patched.3@sha256:d75cac0f708f3e238c458b622fea2f0b7dda9b67e9435eeafa37698d88a2a3c8}"
ORIGIN="http://localhost:${PORT}"
REG_TOKEN="aicortex-live-registration-token"
API_KEY_NAME="aicortex"
# rauthy refuses an API key secret under 64 characters.
API_KEY_SECRET="AicortexLiveApiKeySecret0123456789abcdefghijklmnopqrstuvwxyz0123456789"
ADMIN="admin@localhost"
ADMIN_PASSWORD="AicortexLiveAdminPassword123!"

up() {
  case "$IMAGE" in
    *@sha256:*) ;;
    *) echo "the rauthy image must be pinned by digest, found: $IMAGE" >&2; exit 1 ;;
  esac
  docker rm -f "$NAME" >/dev/null 2>&1 || true
  work="$(mktemp -d)"
  trap 'rm -rf "$work"' EXIT
  # A fresh 32-byte key per run: the throwaway rauthy's data dies with it.
  ENC_KEY="$(head -c 32 /dev/urandom | base64 | tr -d '\n')"
  cat > "$work/rauthy.env" <<ENV
PUB_URL=localhost:${PORT}
LISTEN_SCHEME=http
LISTEN_ADDRESS=0.0.0.0
LISTEN_PORT_HTTP=8080
COOKIE_MODE=danger-insecure
RP_ID=localhost
RP_ORIGIN=${ORIGIN}
RP_NAME=aicortex-live
HQL_NODE_ID=1
HQL_NODES=1 127.0.0.1:8100 127.0.0.1:8200
HQL_LISTEN_ADDR_API=127.0.0.1
HQL_LISTEN_ADDR_RAFT=127.0.0.1
HQL_SECRET_RAFT=AicortexLiveRaftSecret0123456789abcdefghijklmnopqrstuvwxyz0123456789
HQL_SECRET_API=AicortexLiveApiClusterSecret0123456789abcdefghijklmnopqrstuvwxyz0123456789
ENC_KEYS=live/${ENC_KEY}
ENC_KEY_ACTIVE=live
ENABLE_DYN_CLIENT_REG=true
DYN_CLIENT_REG_TOKEN=${REG_TOKEN}
DYN_CLIENT_RATE_LIMIT_SEC=0
BOOTSTRAP_ADMIN_EMAIL=${ADMIN}
BOOTSTRAP_ADMIN_PASSWORD_PLAIN=${ADMIN_PASSWORD}
BOOTSTRAP_API_KEY_SECRET=${API_KEY_SECRET}
ENV
  key='{"name":"'"$API_KEY_NAME"'","exp":1924991999,"access":[{"group":"Blacklist","access_rights":["read","create","update","delete"]},{"group":"Clients","access_rights":["read","create","update","delete"]},{"group":"Users","access_rights":["read","create","update","delete"]},{"group":"Roles","access_rights":["read","create","update","delete"]},{"group":"Scopes","access_rights":["read","create","update","delete"]},{"group":"Secrets","access_rights":["read"]},{"group":"Sessions","access_rights":["delete"]}]}'
  echo "BOOTSTRAP_API_KEY=$(printf '%s' "$key" | base64 | tr -d '\n')" >> "$work/rauthy.env"
  : > "$work/config.toml"
  # No HQL_DATA_DIR: the image prepares /app/data for its own non-root user.
  docker run -d --name "$NAME" -p "127.0.0.1:${PORT}:8080" \
    --env-file "$work/rauthy.env" \
    -v "$work/config.toml:/app/config.toml:ro" \
    "$IMAGE" >/dev/null
  for _ in $(seq 1 120); do
    if [ "$(curl -s -o /dev/null -w '%{http_code}' "$ORIGIN/auth/v1/health")" = "200" ]; then
      printf 'AICORTEX_TEST_RAUTHY_URL=%s\n' "$ORIGIN"
      printf 'AICORTEX_TEST_RAUTHY_REG_TOKEN=%s\n' "$REG_TOKEN"
      printf 'AICORTEX_TEST_RAUTHY_API_KEY=%s$%s\n' "$API_KEY_NAME" "$API_KEY_SECRET"
      printf 'AICORTEX_TEST_RAUTHY_USER=%s\n' "$ADMIN"
      printf 'AICORTEX_TEST_RAUTHY_PASSWORD=%s\n' "$ADMIN_PASSWORD"
      printf 'AICORTEX_TEST_RAUTHY_IMAGE=%s\n' "$IMAGE"
      return 0
    fi
    sleep 1
  done
  echo "rauthy did not become healthy" >&2
  docker logs "$NAME" >&2
  exit 1
}

down() {
  docker rm -f "$NAME" >/dev/null 2>&1 || true
}

case "${1:-}" in
  up) up ;;
  down) down ;;
  *) echo "usage: $0 up|down" >&2; exit 2 ;;
esac
