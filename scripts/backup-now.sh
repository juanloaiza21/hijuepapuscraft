#!/usr/bin/env bash
# Manual pre-change snapshot. Run before touching the mod pack (MODS.md rule).
# Fresh podman run because the pre-created mc-backup container has a frozen
# env and cannot take a per-invocation tag.
set -euo pipefail

ENV_FILE="${ENV_FILE:-/opt/hijuepapuscraft/.env}"
set -a
# shellcheck disable=SC1090
source "$ENV_FILE"
set +a

DIR="$(dirname "$ENV_FILE")"

exec podman run --rm \
  --network mcnet \
  -v mc-data:/data:ro \
  --env-file "$DIR/.env.backup" \
  -e MODE=backup \
  -e SNAPSHOT_TAG=pre-change \
  "${BACKUP_IMAGE}"
