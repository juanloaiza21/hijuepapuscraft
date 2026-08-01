#!/bin/sh
# MODE dispatch: backup (default) | forget | check.
# backup: save-off, save-all flush, restic snapshot of /data, save-on.
# save-on runs via trap so a failed snapshot never leaves saving disabled.
# If RCON is unreachable the server is down; cold backup proceeds.
set -eu

MODE="${MODE:-backup}"
RCON_ADDR="${RCON_ADDR:-mc:25575}"
RCON_HOST="${RCON_ADDR%%:*}"
RCON_PORT="${RCON_ADDR##*:}"

rcon() {
  rcon-cli --host "$RCON_HOST" --port "$RCON_PORT" --password "$RCON_PASSWORD" "$@"
}

do_backup() {
  tag="${SNAPSHOT_TAG:-scheduled}"
  if rcon save-off >/dev/null 2>&1; then
    echo "save-off ok, flushing"
    trap 'rcon save-on >/dev/null 2>&1 || echo "WARN: save-on failed, run manually" >&2' EXIT
    rcon save-all flush >/dev/null 2>&1 || true
    sleep 5
  else
    echo "RCON unreachable, cold backup of /data"
  fi
  restic backup /data --tag "$tag" --host mc
  echo "snapshot done (tag=$tag)"
}

case "$MODE" in
  backup) do_backup ;;
  forget) restic forget --keep-daily 7 --keep-weekly 4 --keep-monthly 6 --prune ;;
  check)  restic check ;;
  *) echo "unknown MODE=$MODE" >&2; exit 2 ;;
esac
