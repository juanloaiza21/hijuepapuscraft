#!/bin/sh
# Restore a snapshot into a fresh volume, then swap it in.
# Run ON THE HOST (uses podman), not inside a container:
#   backup/restore.sh <snapshot-id|latest>
# Follows the RUNBOOK drill; ends by telling you to re-run both
# recreate scripts so containers re-resolve the volume.
set -eu

SNAP="${1:?usage: restore.sh <snapshot-id|latest>}"
ENV_FILE="${ENV_FILE:-/opt/hijuepapuscraft/.env}"
# shellcheck disable=SC1090
. "$ENV_FILE"

echo ">> restoring snapshot $SNAP into volume mc-data-restore"
podman volume rm -f mc-data-restore 2>/dev/null || true
podman volume create mc-data-restore
podman run --rm --env-file "$ENV_FILE" \
  -v mc-data-restore:/restore \
  --entrypoint restic \
  "${BACKUP_IMAGE}" restore "$SNAP" --target /restore

echo ">> restored. To swap it in:"
echo "   podman rm -f mc mc-backup"
echo "   podman volume rm mc-data"
echo "   podman volume create mc-data"
echo "   podman run --rm -v mc-data-restore:/from:ro -v mc-data:/to docker.io/alpine:3.22 sh -c 'cp -a /from/data/. /to/'"
echo "   /opt/hijuepapuscraft/containers/mc-recreate.sh"
echo "   /opt/hijuepapuscraft/containers/backup-recreate.sh"
echo "   systemctl start mc.service"
