#!/usr/bin/env bash
# Host-side Discord alerting, independent of the bot. Used by every unit's
# OnFailure=, by lib-mc.sh's repair/orphan/wedge paths, and by the
# watchdog. Deliberately does NOT depend on the bot: it must still work
# when the bot is down, wedged, or rate-limited, and it shares no failure
# domain with the repair path it reports on.
#
# Always logs first (survives a webhook outage) and never fails its
# caller -- an alert that cannot be delivered must not abort the
# lifecycle op or repair it is reporting on.
set -euo pipefail

ENV_FILE="${ENV_FILE:-/opt/hijuepapuscraft/.env}"

logger -t mc-alert -p daemon.warning -- "$*" || true

DISCORD_WEBHOOK_URL=""
if [[ -r "$ENV_FILE" ]]; then
  DISCORD_WEBHOOK_URL=$(grep -E '^DISCORD_WEBHOOK_URL=' "$ENV_FILE" | tail -n1 | cut -d= -f2-)
fi

if [[ -z "$DISCORD_WEBHOOK_URL" ]]; then
  logger -t mc-alert -p daemon.warning -- "DISCORD_WEBHOOK_URL not set in $ENV_FILE, alert not posted to Discord"
  exit 0
fi

jq -Rn --arg c "$*" '{content: $c}' \
  | curl -fsS -m 10 -H 'Content-Type: application/json' -d @- "$DISCORD_WEBHOOK_URL" >/dev/null \
  || logger -t mc-alert -p daemon.warning -- "webhook POST failed"

exit 0
