#!/usr/bin/env bash
# Recreates the pre-created mc-backup container. MUST be re-run after any
# change to .env (R2 keys, RCON password, BACKUP_IMAGE) or after a volume
# swap (restore drill), because the container freezes its env and volume
# references at creation time.
set -euo pipefail

ENV_FILE="${ENV_FILE:-/opt/hijuepapuscraft/.env}"
[[ -f "$ENV_FILE" ]] || { echo "missing $ENV_FILE" >&2; exit 1; }
set -a
# shellcheck disable=SC1090
source "$ENV_FILE"
set +a

podman rm -f mc-backup 2>/dev/null || true

podman create \
  --name mc-backup \
  --network mcnet \
  --restart=no \
  -v mc-data:/data:ro \
  --env-file "$ENV_FILE" \
  -e MODE=backup \
  -e SNAPSHOT_TAG=scheduled \
  "${BACKUP_IMAGE}"

echo "created. nightly timer and bot /backup now both start this container."
