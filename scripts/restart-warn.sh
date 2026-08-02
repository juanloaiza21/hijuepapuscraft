#!/usr/bin/env bash
# Daily restart with in-game warnings. Runs on the host (systemd timer).
# rcon-cli inside the mc container reads RCON creds from container env,
# so this script never touches the password.
set -euo pipefail

say() { podman exec mc rcon-cli say "$1" >/dev/null 2>&1 || true; }

players_online() {
  podman exec mc rcon-cli list 2>/dev/null \
    | grep -oE 'There are [0-9]+' | grep -oE '[0-9]+' || echo 0
}

if ! podman container exists mc || [[ "$(podman inspect -f '{{.State.Running}}' mc)" != "true" ]]; then
  echo "mc not running, nothing to restart"
  exit 0
fi

if [[ "$(players_online)" -eq 0 ]]; then
  echo "empty server, restarting immediately"
  podman restart mc
  exit 0
fi

say "Server restarts in 5 minutes"
sleep 240
say "Server restarts in 1 minute"
sleep 50
say "Server restarts in 10 seconds"
sleep 10
podman restart mc
echo "restarted with warnings"
