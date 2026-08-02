#!/usr/bin/env bash
# Recreates the mc container from .env. The single source of truth for its
# definition. Re-run after any change to .env or to this file.
# The container is plain podman (not Quadlet) so the bot can stop/start it
# through the docker-compat API. See the design spec, section 3.
set -euo pipefail

ENV_FILE="${ENV_FILE:-/opt/hijuepapuscraft/.env}"
[[ -f "$ENV_FILE" ]] || { echo "missing $ENV_FILE" >&2; exit 1; }
set -a
# shellcheck disable=SC1090
source "$ENV_FILE"
set +a

ENV_FILE="$ENV_FILE" "$(dirname "$0")/../scripts/gen-scoped-env.sh"

podman rm -f mc 2>/dev/null || true
podman volume exists mc-data || podman volume create mc-data
podman network exists mcnet || systemctl start mcnet-network.service

# --stop-timeout 120: the default 10s SIGKILLs a mid-save JVM.
podman create \
  --name mc \
  --network mcnet \
  --restart=on-failure \
  --stop-timeout 120 \
  -p 25565:25565 \
  -v mc-data:/data \
  -e EULA=TRUE \
  -e TYPE=FABRIC \
  -e VERSION=26.2 \
  -e DATAPACKS=https://cdn.modrinth.com/data/QI0EmgZ1/versions/2OaIqKKy/Matcha_Flavoured_1_03.zip \
  -e RESOURCE_PACK=https://cdn.modrinth.com/data/QI0EmgZ1/versions/2OaIqKKy/Matcha_Flavoured_1_03.zip \
  -e RESOURCE_PACK_SHA1=4d40b820203b51a91473f74dd961c858889fcdb8 \
  -e RESOURCE_PACK_ENFORCE=TRUE \
  -e ONLINE_MODE=TRUE \
  -e ENABLE_WHITELIST=TRUE \
  -e ENFORCE_WHITELIST=TRUE \
  -e USE_AIKAR_FLAGS=true \
  -e INIT_MEMORY="${MEMORY}" \
  -e MAX_MEMORY="${MEMORY}" \
  -e ENABLE_RCON=true \
  -e RCON_PASSWORD="${RCON_PASSWORD}" \
  -e ENABLE_AUTOPAUSE=FALSE \
  -e ENFORCE_SECURE_PROFILE=FALSE \
  -e DIFFICULTY="${DIFFICULTY}" \
  -e PACKWIZ_URL=https://raw.githubusercontent.com/juanloaiza21/hijuepapuscraft/main/pack/pack.toml \
  -e TZ="${TZ}" \
  --health-cmd mc-health \
  --health-interval 30s \
  --health-start-period 300s \
  "${MC_IMAGE}"

echo "created. start with: systemctl start mc.service (or podman start mc)"
