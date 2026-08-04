#!/usr/bin/env bash
# Shared lifecycle helpers for the mc container. Sourced by restart-warn.sh
# and mc-watchdog.sh so the daily timer and the watchdog can never drift:
# every dangerous operation (start/stop/rm/exec) has exactly one
# implementation, here.
#
# Background (see docs/superpowers or the wedge-fix incident writeup for
# the full evidence trail): podman 4.9.3 wedges a container that has lost
# its conmon monitor -- the next `podman start`/`stop` against it fails or
# no-ops forever, and only `podman rm -f` + recreate clears it. This file
# exists to (a) stop conmon from being killed in the first place and
# (b) detect and repair it automatically when it happens anyway.
set -euo pipefail

# Overridable so the reproduction/fix drills in the wedge-fix plan can
# point every helper at a disposable container instead of production "mc".
# NEVER override MC_CONTAINER when operating on the real server.
MC_CONTAINER="${MC_CONTAINER:-mc}"

MC_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MC_REPO_DIR="${MC_REPO_DIR:-$(cd "$MC_LIB_DIR/.." && pwd)}"
MC_LOCK_FILE="${MC_LOCK_FILE:-/run/lock/mc-lifecycle.lock}"
MC_WATCHDOG_STATE="${MC_WATCHDOG_STATE:-/run/mc-watchdog.state}"
MC_ALERT_SCRIPT="${MC_ALERT_SCRIPT:-$MC_REPO_DIR/scripts/mc-alert.sh}"
MC_REPAIR_BUDGET="${MC_REPAIR_BUDGET:-2}"    # max repairs per rolling hour
MC_REPAIR_WINDOW_SECS="${MC_REPAIR_WINDOW_SECS:-3600}"

# pod(): every lifecycle call MUST go through this, never bare `podman`.
# podman 4.9.3 deliberately skips moving a freshly spawned conmon into its
# own libpod-conmon-<id>.scope when $INVOCATION_ID is set, i.e. whenever
# the command runs inside a systemd unit (libpod/oci_conmon_linux.go).
# Left inside the unit's cgroup, conmon dies when the unit's cgroup is
# torn down -- verified live: mc-restart.service SIGKILLed conmon 90s
# after its ExecStart returned, and the container kept serving players
# with no monitor at all until the next lifecycle call wedged it in
# "stopping". Unsetting INVOCATION_ID makes a unit-run command behave
# exactly like a known-good interactive `sudo podman ...` invocation
# (verified: the latter reliably creates a persistent conmon scope).
# KillMode=process on every unit that calls this is the second,
# independent half of the fix (belt-and-braces if podman's behavior ever
# changes) -- do NOT remove either half on its own.
pod() {
  env -u INVOCATION_ID podman "$@"
}

mc_state() {
  pod inspect -f '{{.State.Status}}' "$MC_CONTAINER" 2>/dev/null || echo missing
}

# conmon_alive(): the only reliable orphan detector. A container can read
# "running" to every existing check while its monitor is dead (this is
# exactly the ~24h invisible window in the incident) -- ConmonPid is a
# libpod-only field, not exposed over the docker-compat socket, so this
# has to run on the host. Checks pid>0, /proc/<pid>/comm == conmon, and
# that the container id appears in the process's cmdline, which guards
# against the pid having been reused by an unrelated process.
conmon_alive() {
  local pid
  pid=$(pod inspect -f '{{.State.ConmonPid}}' "$MC_CONTAINER" 2>/dev/null || echo 0)
  [[ "$pid" =~ ^[0-9]+$ ]] && ((pid > 0)) || return 1
  [[ -r "/proc/$pid/comm" ]] || return 1
  local comm
  comm=$(cat "/proc/$pid/comm" 2>/dev/null || echo)
  [[ "$comm" == "conmon" ]] || return 1
  local cid
  cid=$(pod inspect -f '{{.Id}}' "$MC_CONTAINER" 2>/dev/null || echo)
  [[ -n "$cid" ]] || return 1
  tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null | grep -qF "$cid"
}

# assert_conmon_scope(): warns (never fails) when a live conmon was NOT
# placed in its own libpod-conmon-<id>.scope. This is the early signal
# that the INVOCATION_ID workaround above has stopped taking effect (e.g.
# after a podman upgrade) even though KillMode=process is still protecting
# the container.
assert_conmon_scope() {
  local pid
  pid=$(pod inspect -f '{{.State.ConmonPid}}' "$MC_CONTAINER" 2>/dev/null || echo 0)
  [[ "$pid" =~ ^[0-9]+$ ]] && ((pid > 0)) || return 0
  if ! grep -q 'libpod-conmon-' "/proc/$pid/cgroup" 2>/dev/null; then
    alert ":warning: conmon for $MC_CONTAINER (pid $pid) is not in a libpod-conmon-*.scope -- the INVOCATION_ID workaround may not be taking effect, investigate before relying on KillMode alone"
  fi
}

# wait_state <regex> <secs>: poll mc_state() until it matches, or time out.
wait_state() {
  local regex=$1 secs=$2 waited=0 st
  while ((waited < secs)); do
    st=$(mc_state)
    [[ "$st" =~ $regex ]] && return 0
    sleep 5
    waited=$((waited + 5))
  done
  return 1
}

# wait_running_healthy <secs>: poll until running AND (healthy or the
# container has no healthcheck configured), or time out.
wait_running_healthy() {
  local secs=$1 waited=0 st health
  while ((waited < secs)); do
    st=$(mc_state)
    if [[ "$st" == "running" ]]; then
      health=$(pod inspect -f '{{.State.Health.Status}}' "$MC_CONTAINER" 2>/dev/null || echo)
      [[ -z "$health" || "$health" == "none" || "$health" == "healthy" ]] && return 0
    fi
    sleep 5
    waited=$((waited + 5))
  done
  return 1
}

rcon() {
  pod exec "$MC_CONTAINER" rcon-cli "$@"
}

# drain_rcon(): best-effort flush + stop of the game process itself (NOT
# the container). Used ahead of repair_recreate so an orphaned-but-live
# JVM gets a clean save instead of being rm -f'd with unsaved chunks.
drain_rcon() {
  rcon save-all flush >/dev/null 2>&1 || true
  rcon stop >/dev/null 2>&1 || true
  local pid waited=0
  pid=$(pod inspect -f '{{.State.Pid}}' "$MC_CONTAINER" 2>/dev/null || echo 0)
  [[ "$pid" =~ ^[0-9]+$ ]] && ((pid > 0)) || return 0
  while kill -0 "$pid" 2>/dev/null; do
    ((waited >= 180)) && return 1
    sleep 5
    waited=$((waited + 5))
  done
  return 0
}

# stop_start_verified(): the ONLY safe replacement for `podman restart`.
# `podman stop` does NOT unwedge a container stuck in "stopping" (proven
# empirically in the incident -- only rm -f + recreate does), so this
# refuses to start on top of an unsettled stop instead of turning a
# `crun start` on a half-stopped container into an opaque exit 125.
stop_start_verified() {
  rcon save-all flush >/dev/null 2>&1 || true
  pod stop -t 120 "$MC_CONTAINER" || true
  if ! wait_state '^(exited|created)$' 180; then
    alert ":rotating_light: $MC_CONTAINER did not settle to exited/created within 180s of stop (state: $(mc_state)) -- refusing to start on top of it"
    return 1
  fi
  pod start "$MC_CONTAINER"
  if ! wait_running_healthy 600; then
    alert ":rotating_light: $MC_CONTAINER failed to reach running+healthy within 600s of start"
    return 1
  fi
  assert_conmon_scope
  return 0
}

# repair_recreate <reason>: the only action proven to unwedge a "stopping"
# container or replace one with a dead conmon -- podman stop/start do
# neither on podman 4.9.3. Guarded by a rolling-hour budget so a
# persistent fault alerts instead of rm -f'ing the container in a loop.
repair_recreate() {
  local reason=$1
  if ! repair_budget_ok; then
    alert ":rotating_light: $MC_CONTAINER needs repair ($reason) but the repair budget (${MC_REPAIR_BUDGET}/hour) is exhausted -- standing down, human needed"
    return 1
  fi
  repair_budget_record
  alert ":wrench: repairing $MC_CONTAINER ($reason): rm -f + recreate"
  if ! timeout 180 pod rm -f "$MC_CONTAINER"; then
    alert ":rotating_light: repair of $MC_CONTAINER failed: rm -f did not complete within 180s"
    return 1
  fi
  if ! "$MC_REPO_DIR/containers/mc-recreate.sh"; then
    alert ":rotating_light: repair of $MC_CONTAINER failed: mc-recreate.sh errored"
    return 1
  fi
  pod start "$MC_CONTAINER"
  if ! wait_running_healthy 600; then
    alert ":rotating_light: repair of $MC_CONTAINER: recreated and started but never reached running+healthy within 600s"
    return 1
  fi
  assert_conmon_scope
  alert ":white_check_mark: repair of $MC_CONTAINER done ($reason): running and healthy"
  return 0
}

# take_lock_or_exit(): shared between the daily restart and the watchdog
# so neither can act while the other is mid-operation (e.g. the watchdog
# firing during the 06:00 restart's own 120s stop). Silent no-op exit --
# the other holder will finish the job.
take_lock_or_exit() {
  exec 9>"$MC_LOCK_FILE"
  flock -n 9 || exit 0
}

alert() {
  "$MC_ALERT_SCRIPT" "$@"
}

# --- repair budget: at most MC_REPAIR_BUDGET repairs per rolling
# MC_REPAIR_WINDOW_SECS window, tracked as one epoch-seconds timestamp per
# line in MC_WATCHDOG_STATE. Shared state so the timer and the watchdog
# draw from the same budget.
_repair_budget_prune() {
  local now=$1 line
  local cutoff=$((now - MC_REPAIR_WINDOW_SECS))
  : > "$MC_WATCHDOG_STATE.tmp"
  if [[ -f "$MC_WATCHDOG_STATE" ]]; then
    while IFS= read -r line; do
      [[ "$line" =~ ^[0-9]+$ ]] && ((line >= cutoff)) && echo "$line" >> "$MC_WATCHDOG_STATE.tmp"
    done < "$MC_WATCHDOG_STATE"
  fi
  mv "$MC_WATCHDOG_STATE.tmp" "$MC_WATCHDOG_STATE"
}

repair_budget_ok() {
  _repair_budget_prune "$(date +%s)"
  local count=0
  [[ -f "$MC_WATCHDOG_STATE" ]] && count=$(wc -l < "$MC_WATCHDOG_STATE")
  ((count < MC_REPAIR_BUDGET))
}

repair_budget_record() {
  date +%s >> "$MC_WATCHDOG_STATE"
}
