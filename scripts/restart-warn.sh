#!/usr/bin/env bash
# Daily restart with in-game warnings. Runs on the host (systemd timer:
# mc-restart.service / mc-restart.timer).
#
# This is the wedge factory the whole fix defuses (see the wedge-fix
# plan): every lifecycle call goes through lib-mc.sh's pod() wrapper
# (env -u INVOCATION_ID) and stop_start_verified() -- never a bare
# `podman restart` -- and the pre-flight branches on the container's
# actual .State.Status instead of just .State.Running, so a container
# wedged in "stopping" or orphaned with a dead conmon gets repaired
# instead of silently skipped. The old guard's "mc not running, nothing
# to restart" + exit 0 on a wedged container is exactly what let the
# outage go unnoticed for a full day in the incident.
set -euo pipefail

# shellcheck source=scripts/lib-mc.sh
source "$(dirname "$0")/lib-mc.sh"

take_lock_or_exit

say() { rcon say "$1" >/dev/null 2>&1 || true; }

players_online() {
  rcon list 2>/dev/null \
    | grep -oE 'There are [0-9]+' | grep -oE '[0-9]+' || echo 0
}

st=$(mc_state)
case "$st" in
  missing)
    alert ":rotating_light: mc-restart: container $MC_CONTAINER does not exist"
    exit 1
    ;;
  exited|created)
    echo "mc not running ($st), nothing to restart"
    exit 0
    ;;
  stopping|removing|dead|paused)
    alert ":rotating_light: mc-restart: $MC_CONTAINER wedged in '$st' at restart time, repairing"
    repair_recreate "wedged: $st" && exit 0
    exit 1
    ;;
  running)
    if ! conmon_alive; then
      alert ":rotating_light: mc-restart: $MC_CONTAINER is running with a dead conmon (orphaned), repairing"
      # NEVER `podman stop` an orphan: that is precisely what manufactures
      # the "stopping"-wedge (stage 2 of the incident). Drain the game
      # process by hand instead, then rm -f + recreate.
      drain_rcon || true
      repair_recreate "orphaned conmon" && exit 0
      exit 1
    fi
    ;;
  *)
    alert ":rotating_light: mc-restart: $MC_CONTAINER in unexpected state '$st'"
    exit 1
    ;;
esac

if [[ "$(players_online)" -eq 0 ]]; then
  echo "empty server, restarting immediately"
  stop_start_verified && exit 0
  exit 1
fi

say "Server restarts in 5 minutes"
sleep 240
say "Server restarts in 1 minute"
sleep 50
say "Server restarts in 10 seconds"
sleep 10
stop_start_verified && { echo "restarted with warnings"; exit 0; }
exit 1
