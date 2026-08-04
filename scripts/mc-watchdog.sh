#!/usr/bin/env bash
# Watchdog for the mc container: catches an orphaned conmon (running with
# no monitor) or a "stopping"-wedge within 5 minutes, instead of the
# ~24h / ~6h45m windows the incident actually ran. See lib-mc.sh for the
# shared primitives and the wedge-fix plan for the evidence trail.
#
# Runs every 5 minutes from mc-watchdog.timer. Every branch that changes
# the container's state alerts at both the start and the end of the
# action; a healthy server produces silent, empty runs.
set -euo pipefail

# shellcheck source=scripts/lib-mc.sh
source "$(dirname "$0")/lib-mc.sh"

HOLD_FILE="${MC_HOLD_FILE:-/var/lib/hijuepapuscraft/hold}"
HOLD_ALERT_STAMP="${MC_HOLD_ALERT_STAMP:-/run/mc-watchdog.hold-alert}"
WEDGE_COUNT_FILE="${MC_WEDGE_COUNT_FILE:-/run/mc-watchdog.wedge}"
UNHEALTHY_COUNT_FILE="${MC_UNHEALTHY_COUNT_FILE:-/run/mc-watchdog.unhealthy}"
WEDGE_MIN_GAP_SECS=600  # rung 5 needs two observations >=10 min apart

# 1. Planned-maintenance hold: never act, but page at most once an hour so
# an operator who forgot to remove the hold file still gets reminded.
if [[ -e "$HOLD_FILE" ]]; then
  now=$(date +%s)
  last=0
  [[ -f "$HOLD_ALERT_STAMP" ]] && last=$(cat "$HOLD_ALERT_STAMP" 2>/dev/null || echo 0)
  [[ "$last" =~ ^[0-9]+$ ]] || last=0
  if ((now - last >= 3600)); then
    alert ":information_source: mc-watchdog: hold file $HOLD_FILE present, standing down"
    echo "$now" > "$HOLD_ALERT_STAMP"
  fi
  exit 0
fi

# 2. Never fight the nightly backup window (04:30): mc-backup's start is
# attached (podman start -a), so "running" here means the backup is live.
if [[ "$(pod inspect -f '{{.State.Running}}' mc-backup 2>/dev/null || echo false)" == "true" ]]; then
  exit 0
fi

# 3. Never fight the daily restart or an in-flight repair.
take_lock_or_exit

clear_wedge_counter() { rm -f "$WEDGE_COUNT_FILE"; }
clear_unhealthy_counter() { rm -f "$UNHEALTHY_COUNT_FILE"; }
clear_counters() { clear_wedge_counter; clear_unhealthy_counter; }

# bump_gated_counter <file>: increments only if the previous recorded
# observation was >=WEDGE_MIN_GAP_SECS ago, so two watchdog runs 5 minutes
# apart (the normal timer cadence) count as one observation, not two --
# a legitimate `podman stop -t 120` passes through "stopping" for well
# under 10 minutes.
bump_gated_counter() {
  local file=$1 now count=0 last_ts=0
  now=$(date +%s)
  if [[ -f "$file" ]]; then
    last_ts=$(cut -d' ' -f1 "$file" 2>/dev/null || echo 0)
    count=$(cut -d' ' -f2 "$file" 2>/dev/null || echo 0)
    [[ "$last_ts" =~ ^[0-9]+$ ]] || last_ts=0
    [[ "$count" =~ ^[0-9]+$ ]] || count=0
    if ((now - last_ts < WEDGE_MIN_GAP_SECS)); then
      echo "$count"
      return 0
    fi
  fi
  count=$((count + 1))
  echo "$now $count" > "$file"
  echo "$count"
}

# bump_simple_counter <file>: plain increment-per-run, used for the
# unhealthy rung where the plan only requires "3 consecutive runs", not a
# minimum spacing between them (the 5-minute timer cadence already gives
# ~15 minutes for 3 increments).
bump_simple_counter() {
  local file=$1 count=0
  [[ -f "$file" ]] && count=$(cat "$file" 2>/dev/null || echo 0)
  [[ "$count" =~ ^[0-9]+$ ]] || count=0
  count=$((count + 1))
  echo "$count" > "$file"
  echo "$count"
}

st=$(mc_state)
case "$st" in
  exited)
    # Rung 1: a legitimate resting state. This is what makes "bot /stop
    # keeps the server stopped" true by construction -- no desired-state
    # file, no action, ever.
    clear_counters
    exit 0
    ;;
  created)
    # Rung 2: a recreate happened and nobody started it. Never auto-start
    # -- that could race a human mid-repair or mid-maintenance.
    clear_counters
    created_epoch=$(date -d "$(pod inspect -f '{{.Created}}' "$MC_CONTAINER" 2>/dev/null || echo now)" +%s 2>/dev/null || date +%s)
    now=$(date +%s)
    if ((now - created_epoch > 900)); then
      alert ":warning: mc-watchdog: $MC_CONTAINER has been 'created' but not started for over 15 minutes"
    fi
    exit 0
    ;;
  running)
    if conmon_alive; then
      # Rung 3.
      clear_wedge_counter
      assert_conmon_scope
      health=$(pod inspect -f '{{.State.Health.Status}}' "$MC_CONTAINER" 2>/dev/null || echo)
      if [[ "$health" == "unhealthy" ]]; then
        ucount=$(bump_simple_counter "$UNHEALTHY_COUNT_FILE")
        if ((ucount >= 3)); then
          if repair_budget_ok; then
            alert ":wrench: mc-watchdog: $MC_CONTAINER unhealthy for 3 consecutive checks (~15 min), restarting"
            if stop_start_verified; then
              repair_budget_record
              clear_unhealthy_counter
              alert ":white_check_mark: mc-watchdog: restart of unhealthy $MC_CONTAINER done"
            else
              alert ":rotating_light: mc-watchdog: restart of unhealthy $MC_CONTAINER FAILED, needs manual repair"
              exit 1
            fi
          else
            alert ":rotating_light: mc-watchdog: $MC_CONTAINER unhealthy for 3 consecutive checks but repair budget exhausted, standing down"
          fi
        fi
      else
        clear_unhealthy_counter
      fi
      exit 0
    fi
    # Rung 4: orphan. Act on the first observation -- deterministic, no
    # counter needed, because a dead conmon never recovers on its own.
    alert ":rotating_light: mc-watchdog: $MC_CONTAINER is running with a dead conmon (orphaned), repairing"
    drain_rcon || true
    if repair_recreate "orphaned conmon"; then
      clear_counters
      exit 0
    fi
    exit 1
    ;;
  stopping|removing|dead)
    # Rung 5: two consecutive observations >=10 min apart. The lock above
    # already excludes a legitimate in-flight `podman stop -t 120`
    # (restart-warn.sh and this script share it); the gated counter is
    # the second, belt-and-braces guard against acting on that transient
    # window if the lock is ever bypassed.
    wcount=$(bump_gated_counter "$WEDGE_COUNT_FILE")
    if ((wcount >= 2)); then
      alert ":rotating_light: mc-watchdog: $MC_CONTAINER stuck in '$st' across two checks >=10 min apart, repairing"
      if repair_recreate "wedged: $st"; then
        clear_counters
      else
        exit 1
      fi
    fi
    exit 0
    ;;
  missing)
    alert ":rotating_light: mc-watchdog: container $MC_CONTAINER does not exist"
    exit 0
    ;;
  *)
    alert ":warning: mc-watchdog: $MC_CONTAINER in unexpected state '$st'"
    exit 0
    ;;
esac
