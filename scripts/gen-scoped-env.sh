#!/usr/bin/env bash
# Generates scoped env subsets from the master .env so each container sees
# only what it needs. Re-run after any .env change (bootstrap runs it, and
# both recreate scripts call it).
set -euo pipefail
ENV_FILE="${ENV_FILE:-/opt/hijuepapuscraft/.env}"
DIR="$(dirname "$ENV_FILE")"
[[ -f "$ENV_FILE" ]] || { echo "missing $ENV_FILE" >&2; exit 1; }
grep -E '^(DISCORD_TOKEN|DISCORD_GUILD_ID|DISCORD_ADMIN_ROLE_ID|DISCORD_NOTIFY_CHANNEL_ID|DOCKER_API_URL|RCON_ADDR|RCON_PASSWORD|SERVER_ADDRESS|TZ)=' "$ENV_FILE" > "$DIR/.env.bot"
grep -E '^(RESTIC_REPOSITORY|RESTIC_PASSWORD|AWS_ACCESS_KEY_ID|AWS_SECRET_ACCESS_KEY|RCON_ADDR|RCON_PASSWORD|TZ)=' "$ENV_FILE" > "$DIR/.env.backup"
chmod 600 "$DIR/.env.bot" "$DIR/.env.backup"
echo "wrote $DIR/.env.bot and $DIR/.env.backup"
