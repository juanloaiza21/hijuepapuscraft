# HijuepapusCraft Infrastructure Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the complete infrastructure repo for a 24/7 Fabric 1.21.1 Minecraft server on Oracle Cloud ARM64: Podman container stack, packwiz mod pack, Rust Discord bot, restic-to-R2 backups, host bootstrap, and docs.

**Architecture:** Podman rootful hybrid: Quadlet units for always-on services (bot, socket-proxy), plain containers for API-controlled ones (mc, mc-backup), systemd timers for schedules. The bot talks RCON for in-game actions and a filtered docker-compat API for lifecycle. Spec: `docs/superpowers/specs/2026-08-01-minecraft-infra-design.md` (authoritative for all design rationale).

**Tech Stack:** Podman 4.9 (host) / 6.0 (dev machine), Quadlet, systemd timers, packwiz, Rust (poise 0.6.2, serenity 0.12.5, mc-query 2.0.0, bollard 0.21.0, tokio), restic, Cloudflare R2, GitHub Actions `ubuntu-24.04-arm`, GHCR.

## Global Constraints

- Every image pinned; never `latest`. Verified pins: `docker.io/itzg/minecraft-server:2026.7.2-java21` (arm64 confirmed), `lscr.io/linuxserver/socket-proxy:3.4.3-r0-ls90` (arm64 confirmed), `docker.io/alpine:3.22` (backup base), rcon-cli 1.7.6, fabric-loader 0.19.3.
- Crate pins (crates.io, verified 2026-08-01): `poise = "0.6.2"`, `mc-query = "2.0.0"`, `bollard = "0.21.0"`, `tokio = "1"`, `anyhow = "1"`, `thiserror = "2"`, `tracing = "0.1"`, `tracing-subscriber = "0.3"`. MSRV 1.74. NOTE: the spec's original `rcon` crate suggestion was rejected after verification (unmaintained since 2021); `mc-query` replaces it.
- All secrets via env. `.env.example` committed with dummy values; `.env` gitignored. On the host, `.env` lives at `/opt/hijuepapuscraft/.env`.
- Container names are fixed API contracts: `mc`, `bot`, `socket-proxy`, `mc-backup`. Network name: `mcnet`. Volume: `mc-data`.
- RCON reachable only inside `mcnet` at `mc:25575`. Only published port anywhere: `25565/tcp`.
- All timers carry explicit `America/Bogota` in `OnCalendar`.
- Bot lifecycle calls restricted to names `mc` and `mc-backup` in bot code.
- Docs and code comments: concise prose, no em dashes.
- Commit after every task with the trailer: `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`
- Working directory: `/home/juloaizar/Documents/personal/hijuepapuscraft` (git repo on `main`).
- shellcheck runs via `podman run --rm -v "$PWD:/mnt:ro" docker.io/koalaman/shellcheck:stable` (not installed on host). All shell scripts must pass with zero warnings.
- GitHub repo: `juanloaiza21/hijuepapuscraft`, public. GHCR images: `ghcr.io/juanloaiza21/hijuepapuscraft-bot` and `-backup`, tagged with short git SHA.

---

### Task 1: Repo scaffold, env contract, GitHub remote

**Files:**
- Create: `.gitignore`
- Create: `.env.example`

**Interfaces:**
- Produces: the env var names every later task consumes. They are exactly the keys in `.env.example` below; later tasks must not invent new names without adding them here.

- [ ] **Step 1: Write `.gitignore`**

```gitignore
.env
target/
*.tar.gz
.DS_Store
```

- [ ] **Step 2: Write `.env.example`**

```bash
# ---- Discord bot ----
DISCORD_TOKEN=changeme
DISCORD_GUILD_ID=000000000000000000
DISCORD_ADMIN_ROLE_ID=000000000000000000
DISCORD_NOTIFY_CHANNEL_ID=000000000000000000
DOCKER_API_URL=http://socket-proxy:2375
RCON_ADDR=mc:25575
SERVER_ADDRESS=mc.hijuepapus.pro

# ---- Minecraft server ----
RCON_PASSWORD=changeme
MEMORY=6G
DIFFICULTY=hard

# ---- Images (bump deliberately, never latest) ----
MC_IMAGE=docker.io/itzg/minecraft-server:2026.7.2-java21
PROXY_IMAGE=lscr.io/linuxserver/socket-proxy:3.4.3-r0-ls90
BOT_IMAGE=ghcr.io/juanloaiza21/hijuepapuscraft-bot:changeme
BACKUP_IMAGE=ghcr.io/juanloaiza21/hijuepapuscraft-backup:changeme

# ---- Backups (Cloudflare R2 via S3 API) ----
RESTIC_REPOSITORY=s3:https://ACCOUNT_ID.r2.cloudflarestorage.com/hijuepapus-backups
RESTIC_PASSWORD=changeme
AWS_ACCESS_KEY_ID=changeme
AWS_SECRET_ACCESS_KEY=changeme

# ---- Host ----
TZ=America/Bogota
```

- [ ] **Step 3: Verify no live secrets and commit**

Run: `git add -A && git status --short` then `grep -rE '(ghp_|dckr_|r2_)' .env.example || echo CLEAN`
Expected: `CLEAN`

```bash
git commit -m "Scaffold repo: gitignore and env contract"
```

- [ ] **Step 4: Create the public GitHub repo and push**

Run:
```bash
gh repo create juanloaiza21/hijuepapuscraft --public --source . --push
```
Expected: repo visible, `git push` succeeds. `PACKWIZ_URL` (raw.githubusercontent.com) depends on this being public.

---

### Task 2: containers/ (Quadlet units, systemd units, recreate scripts)

**Files:**
- Create: `containers/quadlet/mcnet.network`
- Create: `containers/quadlet/socket-proxy.container`
- Create: `containers/quadlet/bot.container`
- Create: `containers/systemd/mc.service`
- Create: `containers/systemd/mc-backup.service`, `containers/systemd/mc-backup.timer`
- Create: `containers/systemd/mc-restart.service`, `containers/systemd/mc-restart.timer`
- Create: `containers/systemd/restic-forget.service`, `containers/systemd/restic-forget.timer`
- Create: `containers/systemd/restic-check.service`, `containers/systemd/restic-check.timer`
- Create: `containers/mc-recreate.sh`
- Create: `containers/backup-recreate.sh`

**Interfaces:**
- Consumes: env names from Task 1.
- Produces: network `mcnet`; containers `mc`, `mc-backup` (plain), `bot`, `socket-proxy` (Quadlet); systemd unit names used by bootstrap (Task 11): everything in `containers/systemd/` plus the Quadlet-generated `bot.service`, `socket-proxy.service`, `mcnet-network.service`.

- [ ] **Step 1: Write `containers/quadlet/mcnet.network`**

```ini
[Network]
NetworkName=mcnet

[Install]
WantedBy=multi-user.target
```

- [ ] **Step 2: Write `containers/quadlet/socket-proxy.container`**

```ini
[Unit]
Description=Filtered docker-compat API proxy for the bot

[Container]
ContainerName=socket-proxy
Image=lscr.io/linuxserver/socket-proxy:3.4.3-r0-ls90
Network=mcnet.network
Volume=/run/podman/podman.sock:/var/run/docker.sock:ro
# CONTAINERS=1: GET list/inspect/stats/logs. ALLOW_*=1 work with POST=0.
# EXEC stays 0. Nothing else is reachable.
Environment=CONTAINERS=1
Environment=ALLOW_START=1
Environment=ALLOW_STOP=1
Environment=ALLOW_RESTARTS=1
ReadOnly=true
Tmpfs=/run

[Service]
Restart=always

[Install]
WantedBy=multi-user.target default.target
```

- [ ] **Step 3: Write `containers/quadlet/bot.container`**

```ini
[Unit]
Description=HijuepapusCraft Discord bot
Wants=socket-proxy.service
After=socket-proxy.service

[Container]
ContainerName=bot
# Image tag is bumped by editing this file (versioned pin).
Image=ghcr.io/juanloaiza21/hijuepapuscraft-bot:changeme
Network=mcnet.network
EnvironmentFile=/opt/hijuepapuscraft/.env

[Service]
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target default.target
```

- [ ] **Step 4: Write `containers/mc-recreate.sh`**

```bash
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

podman rm -f mc 2>/dev/null || true
podman volume exists mc-data || podman volume create mc-data

podman create \
  --name mc \
  --network mcnet \
  --restart=on-failure \
  -p 25565:25565 \
  -v mc-data:/data \
  -e EULA=TRUE \
  -e TYPE=FABRIC \
  -e VERSION=1.21.1 \
  -e ONLINE_MODE=TRUE \
  -e ENABLE_WHITELIST=TRUE \
  -e ENFORCE_WHITELIST=TRUE \
  -e USE_AIKAR_FLAGS=true \
  -e INIT_MEMORY="${MEMORY}" \
  -e MAX_MEMORY="${MEMORY}" \
  -e ENABLE_RCON=true \
  -e RCON_PASSWORD="${RCON_PASSWORD}" \
  -e ENABLE_AUTOPAUSE=FALSE \
  -e DIFFICULTY="${DIFFICULTY}" \
  -e PACKWIZ_URL=https://raw.githubusercontent.com/juanloaiza21/hijuepapuscraft/main/pack/pack.toml \
  -e TZ="${TZ}" \
  --health-cmd mc-health \
  --health-interval 30s \
  --health-start-period 300s \
  "${MC_IMAGE}"

echo "created. start with: systemctl start mc.service (or podman start mc)"
```

- [ ] **Step 5: Write `containers/backup-recreate.sh`**

```bash
#!/usr/bin/env bash
# Recreates the pre-created mc-backup container. MUST be re-run after any
# change to .env (R2 keys, RCON password, BACKUP_IMAGE) or after a volume
# swap (restore drill), because the container freezes its env and volume
# references at creation time.
set -euo pipefail

ENV_FILE="${ENV_FILE:-/opt/hijuepapuscraft/.env}"
[[ -f "$ENV_FILE" ]] || { echo "missing $ENV_FILE" >&2; exit 1; }
set -a
# shellcheck disable=SC1090
source "$ENV_FILE"
set +a

podman rm -f mc-backup 2>/dev/null || true

podman create \
  --name mc-backup \
  --network mcnet \
  --restart=no \
  -v mc-data:/data:ro \
  --env-file "$ENV_FILE" \
  -e MODE=backup \
  -e SNAPSHOT_TAG=scheduled \
  "${BACKUP_IMAGE}"

echo "created. nightly timer and bot /backup now both start this container."
```

- [ ] **Step 6: Write `containers/systemd/mc.service`**

```ini
[Unit]
Description=Start Minecraft container at boot
Wants=network-online.target mcnet-network.service podman.socket
After=network-online.target mcnet-network.service podman.socket

[Service]
Type=oneshot
RemainAfterExit=yes
# Retry: boot can race image/network readiness.
ExecStart=/bin/sh -c 'for i in 1 2 3 4 5; do podman start mc && exit 0; sleep 10; done; exit 1'

[Install]
WantedBy=multi-user.target
```

- [ ] **Step 7: Write backup timer pair**

`containers/systemd/mc-backup.service`:
```ini
[Unit]
Description=Nightly world backup (runs attached so failures are visible)

[Service]
Type=oneshot
ExecStart=/usr/bin/podman start -a mc-backup
```

`containers/systemd/mc-backup.timer`:
```ini
[Unit]
Description=Nightly world backup at 04:30 Bogota

[Timer]
OnCalendar=*-*-* 04:30:00 America/Bogota
Persistent=false

[Install]
WantedBy=timers.target
```

- [ ] **Step 8: Write restart timer pair**

`containers/systemd/mc-restart.service`:
```ini
[Unit]
Description=Daily server restart with in-game warnings

[Service]
Type=oneshot
ExecStart=/opt/hijuepapuscraft/scripts/restart-warn.sh
```

`containers/systemd/mc-restart.timer`:
```ini
[Unit]
Description=Daily server restart at 06:00 Bogota

[Timer]
OnCalendar=*-*-* 06:00:00 America/Bogota
Persistent=false

[Install]
WantedBy=timers.target
```

- [ ] **Step 9: Write forget and check timer pairs**

`containers/systemd/restic-forget.service`:
```ini
[Unit]
Description=Weekly restic retention prune

[Service]
Type=oneshot
EnvironmentFile=/opt/hijuepapuscraft/.env
ExecStart=/usr/bin/podman run --rm --env-file /opt/hijuepapuscraft/.env -e MODE=forget ${BACKUP_IMAGE}
```

`containers/systemd/restic-forget.timer`:
```ini
[Unit]
Description=Weekly restic forget+prune, Sunday 05:30 Bogota

[Timer]
OnCalendar=Sun *-*-* 05:30:00 America/Bogota
Persistent=true

[Install]
WantedBy=timers.target
```

`containers/systemd/restic-check.service`:
```ini
[Unit]
Description=Monthly restic integrity check

[Service]
Type=oneshot
EnvironmentFile=/opt/hijuepapuscraft/.env
ExecStart=/usr/bin/podman run --rm --env-file /opt/hijuepapuscraft/.env -e MODE=check ${BACKUP_IMAGE}
```

`containers/systemd/restic-check.timer`:
```ini
[Unit]
Description=Monthly restic check, 1st at 06:30 Bogota

[Timer]
OnCalendar=*-*-01 06:30:00 America/Bogota
Persistent=true

[Install]
WantedBy=timers.target
```

- [ ] **Step 10: Verify Quadlet units parse (local dry run)**

Run:
```bash
QUADLET_UNIT_DIRS="$PWD/containers/quadlet" /usr/lib/podman/quadlet -dryrun 2>&1 | head -40
```
Expected: generated `bot.service`, `socket-proxy.service`, `mcnet-network.service` text; zero `quadlet-generator` errors. Confirm the output contains `--name bot`, `--name socket-proxy`, and `--network mcnet`.

- [ ] **Step 11: Verify scripts with bash -n and shellcheck**

Run:
```bash
bash -n containers/mc-recreate.sh containers/backup-recreate.sh
podman run --rm -v "$PWD:/mnt:ro" docker.io/koalaman/shellcheck:stable /mnt/containers/mc-recreate.sh /mnt/containers/backup-recreate.sh
```
Expected: no output (clean).

- [ ] **Step 12: Commit**

```bash
git add containers && git commit -m "Add container stack: quadlet units, systemd timers, recreate scripts"
```

---

### Task 3: packwiz mod pack

**Files:**
- Create: `pack/pack.toml`, `pack/index.toml`, `pack/mods/*.pw.toml` (generated by packwiz)

**Interfaces:**
- Consumes: nothing from other tasks (PACKWIZ_URL in Task 2 points at the file this task creates).
- Produces: `pack/pack.toml` at the raw URL `https://raw.githubusercontent.com/juanloaiza21/hijuepapuscraft/main/pack/pack.toml`.

- [ ] **Step 1: Install packwiz**

Run: `go install github.com/packwiz/packwiz@latest && export PATH="$HOME/go/bin:$PATH" && packwiz --help | head -3`
Expected: usage text. (No tagged releases exist; CI-built or go-installed binaries are the official methods.)

- [ ] **Step 2: Init the pack**

Run, inside `pack/` (create the dir first):
```bash
mkdir -p pack && cd pack
packwiz init --name "HijuepapusCraft" --author "juanloaiza21" --version 1.0.0 \
  --mc-version 1.21.1 --modloader fabric --fabric-version 0.19.3
```
Expected: `pack.toml` and `index.toml` created with `[versions] minecraft = "1.21.1"` and `fabric = "0.19.3"`. If the flag names differ in the installed build, run bare `packwiz init` and answer the prompts with the same values.

- [ ] **Step 3: Add the seven baseline mods (verified Modrinth slugs)**

Run, inside `pack/`:
```bash
for slug in lithium ferrite-core krypton chunky spark easyauth easywhitelist; do
  packwiz modrinth add "$slug" -y
done
```
Expected: seven `mods/*.pw.toml` files. packwiz resolves the newest 1.21.1 Fabric build of each because pack.toml pins the MC version. (If `-y` is not accepted by the installed build, confirm each prompt manually.)

- [ ] **Step 4: Verify and refresh**

Run, inside `pack/`: `packwiz refresh && ls mods/`
Expected: exit 0; seven `.pw.toml` files listed. Spot-check `mods/easyauth.pw.toml` contains `side = "server"` (Modrinth marks it server-required; if any perf mod shows `side = "both"` that is correct and harmless for a server-consumed pack).

- [ ] **Step 5: Commit**

```bash
git add pack && git commit -m "Add packwiz pack: 1.21.1 fabric baseline (lithium, ferrite-core, krypton, chunky, spark, easyauth, easywhitelist)"
```

---

### Task 4: Backup image and scripts

**Files:**
- Create: `backup/Dockerfile`
- Create: `backup/backup.sh`
- Create: `backup/restore.sh`

**Interfaces:**
- Consumes: env names from Task 1 (`RESTIC_*`, `AWS_*`, `RCON_PASSWORD`, `RCON_ADDR`, `MODE`, `SNAPSHOT_TAG`).
- Produces: image contract used by Task 2's units and Task 10's CI: entrypoint runs `backup.sh`, behavior selected by `MODE` in {`backup`, `forget`, `check`}; `/data` is the world mount.

- [ ] **Step 1: Write `backup/Dockerfile`**

```dockerfile
FROM docker.io/alpine:3.22
# restic 0.18.0 in alpine 3.22 (>= 0.14 required for R2). rcon-cli from itzg.
ARG TARGETARCH=arm64
ARG RCON_CLI_VERSION=1.7.6
RUN apk add --no-cache restic curl \
 && curl -fsSL "https://github.com/itzg/rcon-cli/releases/download/${RCON_CLI_VERSION}/rcon-cli_${RCON_CLI_VERSION}_linux_${TARGETARCH}.tar.gz" \
    | tar xz -C /usr/local/bin rcon-cli \
 && apk del curl
COPY backup.sh /usr/local/bin/backup.sh
ENTRYPOINT ["/bin/sh", "/usr/local/bin/backup.sh"]
```

- [ ] **Step 2: Write `backup/backup.sh`**

```sh
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
```

- [ ] **Step 3: Write `backup/restore.sh`**

```sh
#!/bin/sh
# Restore a snapshot into a fresh volume, then swap it in.
# Run ON THE HOST (uses podman), not inside a container:
#   backup/restore.sh <snapshot-id|latest>
# Follows the RUNBOOK drill; ends by telling you to re-run both
# recreate scripts so containers re-resolve the volume.
set -eu

SNAP="${1:?usage: restore.sh <snapshot-id|latest>}"
ENV_FILE="${ENV_FILE:-/opt/hijuepapuscraft/.env}"
. "$ENV_FILE"

echo ">> restoring snapshot $SNAP into volume mc-data-restore"
podman volume rm -f mc-data-restore 2>/dev/null || true
podman volume create mc-data-restore
podman run --rm --env-file "$ENV_FILE" \
  -v mc-data-restore:/restore \
  --entrypoint restic \
  "${BACKUP_IMAGE}" restore "$SNAP" --target /restore

echo ">> restored. To swap it in:"
echo "   systemctl stop mc.service && podman rm -f mc"
echo "   podman volume rm mc-data"
echo "   podman volume create mc-data"
echo "   podman run --rm -v mc-data-restore:/from:ro -v mc-data:/to docker.io/alpine:3.22 sh -c 'cp -a /from/data/. /to/'"
echo "   /opt/hijuepapuscraft/containers/mc-recreate.sh"
echo "   /opt/hijuepapuscraft/containers/backup-recreate.sh"
echo "   systemctl start mc.service"
```

- [ ] **Step 4: Lint scripts**

Run:
```bash
podman run --rm -v "$PWD:/mnt:ro" docker.io/koalaman/shellcheck:stable -s sh /mnt/backup/backup.sh /mnt/backup/restore.sh
```
Expected: clean.

- [ ] **Step 5: Build the image locally (native arch smoke build)**

Run:
```bash
podman build --build-arg TARGETARCH=amd64 -t backup-smoke backup/
```
Expected: build succeeds; `podman run --rm --entrypoint sh backup-smoke -c 'restic version && rcon-cli --help | head -1'` prints restic 0.18.x and rcon-cli usage.

- [ ] **Step 6: Local end-to-end restic drill (file backend, no R2 needed)**

Run:
```bash
mkdir -p /tmp/claude-1000/-home-juloaizar-Documents-personal/3313d0ac-33bb-4d43-8c21-9ee6b9e3d435/scratchpad/restic-drill/{repo,data}
echo "hello world $(date +%s)" > /tmp/claude-1000/-home-juloaizar-Documents-personal/3313d0ac-33bb-4d43-8c21-9ee6b9e3d435/scratchpad/restic-drill/data/level.dat
D=/tmp/claude-1000/-home-juloaizar-Documents-personal/3313d0ac-33bb-4d43-8c21-9ee6b9e3d435/scratchpad/restic-drill
podman run --rm -e RESTIC_REPOSITORY=/repo -e RESTIC_PASSWORD=drill -v "$D/repo:/repo" --entrypoint restic backup-smoke init
podman run --rm -e MODE=backup -e SNAPSHOT_TAG=drill -e RESTIC_REPOSITORY=/repo -e RESTIC_PASSWORD=drill -e RCON_PASSWORD=x -v "$D/repo:/repo" -v "$D/data:/data:ro" backup-smoke
podman run --rm -e MODE=forget -e RESTIC_REPOSITORY=/repo -e RESTIC_PASSWORD=drill -v "$D/repo:/repo" backup-smoke
podman run --rm -e MODE=check -e RESTIC_REPOSITORY=/repo -e RESTIC_PASSWORD=drill -v "$D/repo:/repo" backup-smoke
```
Expected: first run prints "RCON unreachable, cold backup" then "snapshot done (tag=drill)"; forget and check exit 0. This proves all three MODEs and the cold path before any cloud credentials exist.

- [ ] **Step 7: Commit**

```bash
git add backup && git commit -m "Add backup image: restic + rcon-cli, MODE dispatch, restore script"
```

### Task 5: Bot crate scaffold and config module

**Files:**
- Create: `bot/Cargo.toml`
- Create: `bot/src/main.rs` (stub, completed in Task 9)
- Create: `bot/src/config.rs`

**Interfaces:**
- Produces: `Config` struct consumed by every later bot task:
  `Config { discord_token: String, guild_id: u64, admin_role_id: u64, notify_channel_id: u64, docker_api_url: String, rcon_addr: String, rcon_password: String, server_address: String }`,
  `Config::from_env() -> anyhow::Result<Config>`,
  `Config::rcon_host_port(&self) -> anyhow::Result<(String, u16)>`.

- [ ] **Step 1: Write `bot/Cargo.toml`**

```toml
[package]
name = "hijuepapus-bot"
version = "0.1.0"
edition = "2021"
rust-version = "1.74"

[dependencies]
poise = "0.6.2"
mc-query = "2.0.0"
bollard = "0.21.0"
tokio = { version = "1", features = ["macros", "rt-multi-thread", "time"] }
anyhow = "1"
thiserror = "2"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
regex = "1"
```

- [ ] **Step 2: Stub `bot/src/main.rs` so the crate compiles**

```rust
mod config;

fn main() {
    println!("wired in task 9");
}
```

- [ ] **Step 3: Write the failing config tests (bottom of `bot/src/config.rs`)**

The struct reads env through an injected lookup so tests never mutate process env:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + '_ {
        let m: HashMap<&str, &str> = pairs.iter().copied().collect();
        move |k| m.get(k).map(|v| v.to_string())
    }

    #[test]
    fn parses_complete_env() {
        let cfg = Config::from_lookup(env(&[
            ("DISCORD_TOKEN", "t"),
            ("DISCORD_GUILD_ID", "123"),
            ("DISCORD_ADMIN_ROLE_ID", "456"),
            ("DISCORD_NOTIFY_CHANNEL_ID", "789"),
            ("DOCKER_API_URL", "http://socket-proxy:2375"),
            ("RCON_ADDR", "mc:25575"),
            ("SERVER_ADDRESS", "mc.hijuepapus.pro"),
            ("RCON_PASSWORD", "pw"),
        ]))
        .unwrap();
        assert_eq!(cfg.guild_id, 123);
        assert_eq!(cfg.rcon_host_port().unwrap(), ("mc".to_string(), 25575));
    }

    #[test]
    fn missing_var_is_a_named_error() {
        let err = Config::from_lookup(env(&[])).unwrap_err();
        assert!(err.to_string().contains("DISCORD_TOKEN"));
    }

    #[test]
    fn bad_id_is_a_named_error() {
        let err = Config::from_lookup(env(&[
            ("DISCORD_TOKEN", "t"),
            ("DISCORD_GUILD_ID", "notanumber"),
        ]))
        .unwrap_err();
        assert!(err.to_string().contains("DISCORD_GUILD_ID"));
    }
}
```

- [ ] **Step 4: Run tests, verify they fail to compile (Config missing)**

Run: `cd bot && cargo test`
Expected: compile error, `Config` not found.

- [ ] **Step 5: Implement `bot/src/config.rs` above the tests**

```rust
use anyhow::{bail, Context as _};

#[derive(Clone, Debug)]
pub struct Config {
    pub discord_token: String,
    pub guild_id: u64,
    pub admin_role_id: u64,
    pub notify_channel_id: u64,
    pub docker_api_url: String,
    pub rcon_addr: String,
    pub rcon_password: String,
    pub server_address: String,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let req = |k: &str| get(k).with_context(|| format!("missing env var {k}"));
        let id = |k: &str| -> anyhow::Result<u64> {
            req(k)?.parse().with_context(|| format!("{k} must be a numeric Discord id"))
        };
        Ok(Self {
            discord_token: req("DISCORD_TOKEN")?,
            guild_id: id("DISCORD_GUILD_ID")?,
            admin_role_id: id("DISCORD_ADMIN_ROLE_ID")?,
            notify_channel_id: id("DISCORD_NOTIFY_CHANNEL_ID")?,
            docker_api_url: get("DOCKER_API_URL").unwrap_or_else(|| "http://socket-proxy:2375".into()),
            rcon_addr: get("RCON_ADDR").unwrap_or_else(|| "mc:25575".into()),
            rcon_password: req("RCON_PASSWORD")?,
            server_address: get("SERVER_ADDRESS").unwrap_or_else(|| "unknown".into()),
        })
    }

    pub fn rcon_host_port(&self) -> anyhow::Result<(String, u16)> {
        match self.rcon_addr.rsplit_once(':') {
            Some((h, p)) => Ok((h.to_string(), p.parse().context("RCON_ADDR port")?)),
            None => bail!("RCON_ADDR must be host:port"),
        }
    }
}
```

Adjust the `parses_complete_env` test: it also needs `RCON_PASSWORD` (shown in the test above already).

- [ ] **Step 6: Run tests, verify pass**

Run: `cd bot && cargo test`
Expected: 3 passed.

- [ ] **Step 7: Commit**

```bash
git add bot && git commit -m "Bot: crate scaffold and env config with injected-lookup tests"
```

---

### Task 6: Bot parsers (spark TPS, player list)

**Files:**
- Create: `bot/src/parse.rs`
- Modify: `bot/src/main.rs` (add `mod parse;`)

**Interfaces:**
- Produces: `Tps { last_5s: f64, last_10s: f64, last_1m: f64, last_5m: f64, last_15m: f64, catching_up: bool }`, `parse_tps(raw: &str) -> Option<Tps>`; `PlayerList { online: u32, max: u32, names: Vec<String> }`, `parse_list(raw: &str) -> Option<PlayerList>`.

Background (verified from spark source): on Fabric, RCON output arrives as ONE concatenated string, no newlines, no legacy `§` color codes, each logical line prefixed `[⚡]`. TPS values are 2-decimal doubles capped at 20 with a `*` prefix when the server is catching up. The parser must still tolerate `§x` sequences so it also works on hybrid servers.

- [ ] **Step 1: Write failing tests (bottom of `bot/src/parse.rs`)**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // Reconstructed from spark's StatisticFormatter/HealthModule source.
    const SPARK_TPS_RCON: &str = "[⚡] TPS from last 5s, 10s, 1m, 5m, 15m: [⚡]  20.0, *20.0, 19.98, 19.87, 19.91[⚡] [⚡] Tick durations (min/med/95%ile/max ms) from last 10s, 1m: [⚡]  2.5/3.2/5.1/21.5;  2.4/3.4/6.8/45.2[⚡] [⚡] CPU usage from last 10s, 1m, 15m: [⚡]     12%, 15%, 14%  (system)[⚡]     8%, 9%, 9%  (process)";

    #[test]
    fn parses_spark_tps_line() {
        let tps = parse_tps(SPARK_TPS_RCON).unwrap();
        assert_eq!(tps.last_5s, 20.0);
        assert_eq!(tps.last_10s, 20.0);
        assert_eq!(tps.last_1m, 19.98);
        assert_eq!(tps.last_15m, 19.91);
        assert!(tps.catching_up); // the *20.0
    }

    #[test]
    fn tolerates_color_codes() {
        let painted = "[⚡] TPS from last 5s, 10s, 1m, 5m, 15m: §a20.0§r, §a19.99§r, 19.98, 19.87, 19.91";
        let tps = parse_tps(painted).unwrap();
        assert_eq!(tps.last_10s, 19.99);
        assert!(!tps.catching_up);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_tps("Unknown command").is_none());
    }

    #[test]
    fn parses_player_list() {
        let raw = "There are 3 of a max of 8 players online: alice, bob, carl";
        let l = parse_list(raw).unwrap();
        assert_eq!((l.online, l.max), (3, 8));
        assert_eq!(l.names, vec!["alice", "bob", "carl"]);
    }

    #[test]
    fn parses_empty_list() {
        let l = parse_list("There are 0 of a max of 8 players online:").unwrap();
        assert_eq!(l.online, 0);
        assert!(l.names.is_empty());
    }
}
```

- [ ] **Step 2: Run, verify compile failure**

Run: `cd bot && cargo test parse`
Expected: FAIL, functions not defined.

- [ ] **Step 3: Implement above the tests**

```rust
use regex::Regex;
use std::sync::OnceLock;

#[derive(Debug, PartialEq)]
pub struct Tps {
    pub last_5s: f64,
    pub last_10s: f64,
    pub last_1m: f64,
    pub last_5m: f64,
    pub last_15m: f64,
    pub catching_up: bool,
}

#[derive(Debug, PartialEq)]
pub struct PlayerList {
    pub online: u32,
    pub max: u32,
    pub names: Vec<String>,
}

fn strip_colors(raw: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new("§[0-9a-fk-or]").unwrap())
        .replace_all(raw, "")
        .into_owned()
}

pub fn parse_tps(raw: &str) -> Option<Tps> {
    let clean = strip_colors(raw);
    let after = clean.split("TPS from last 5s, 10s, 1m, 5m, 15m:").nth(1)?;
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(\*?)(\d+(?:\.\d+)?)").unwrap());
    let mut vals = Vec::with_capacity(5);
    let mut catching_up = false;
    for cap in re.captures_iter(after).take(5) {
        catching_up |= &cap[1] == "*";
        vals.push(cap[2].parse::<f64>().ok()?);
    }
    if vals.len() < 5 {
        return None;
    }
    Some(Tps {
        last_5s: vals[0],
        last_10s: vals[1],
        last_1m: vals[2],
        last_5m: vals[3],
        last_15m: vals[4],
        catching_up,
    })
}

pub fn parse_list(raw: &str) -> Option<PlayerList> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"There are (\d+) of a max of (\d+) players online:?\s*(.*)").unwrap()
    });
    let clean = strip_colors(raw);
    let cap = re.captures(&clean)?;
    let names = cap[3]
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    Some(PlayerList {
        online: cap[1].parse().ok()?,
        max: cap[2].parse().ok()?,
        names,
    })
}
```

- [ ] **Step 4: Run tests, verify 5 pass**

Run: `cd bot && cargo test parse`
Expected: 5 passed.

- [ ] **Step 5: Commit**

```bash
git add bot && git commit -m "Bot: spark TPS and player list parsers with source-derived fixtures"
```

---

### Task 7: Bot transports (RCON wrapper, Docker control)

**Files:**
- Create: `bot/src/rcon.rs`
- Create: `bot/src/docker.rs`
- Modify: `bot/src/main.rs` (add `mod rcon; mod docker;`)

**Interfaces:**
- Consumes: `Config` (Task 5).
- Produces:
  `McRcon::new(host: String, port: u16, password: String) -> McRcon`; `McRcon::cmd(&mut self, cmd: &str) -> anyhow::Result<String>` (persistent connection, one transparent reconnect on error; callers wrap in `Arc<tokio::sync::Mutex<McRcon>>`).
  `DockerCtl::connect(url: &str) -> anyhow::Result<DockerCtl>` (Clone); `StartOutcome { Started, AlreadyRunning }`; methods `start/stop/restart(&self, name: &str)`, `inspect(&self, name) -> anyhow::Result<Option<ContainerStatus>>` where `ContainerStatus { running: bool, health: Option<String>, started_at: Option<String>, exit_code: Option<i64> }`, `stats_mem(&self, name) -> anyhow::Result<Option<(u64, u64)>>`, `logs_tail(&self, name, lines: usize) -> anyhow::Result<String>`; free fn `guard(name: &str) -> anyhow::Result<()>` rejecting anything but `mc` / `mc-backup`.

- [ ] **Step 1: Write the failing guard test (bottom of `bot/src/docker.rs`)**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_allows_only_the_two_managed_containers() {
        assert!(guard("mc").is_ok());
        assert!(guard("mc-backup").is_ok());
        assert!(guard("bot").is_err());
        assert!(guard("socket-proxy").is_err());
        assert!(guard("mc; rm -rf /").is_err());
    }
}
```

- [ ] **Step 2: Run, verify failure; then implement `bot/src/docker.rs`**

```rust
use anyhow::{bail, Context as _};
use bollard::Docker;

pub const ALLOWED: [&str; 2] = ["mc", "mc-backup"];

pub fn guard(name: &str) -> anyhow::Result<()> {
    if ALLOWED.contains(&name) {
        Ok(())
    } else {
        bail!("container {name:?} is not managed by this bot")
    }
}

#[derive(Debug, PartialEq)]
pub enum StartOutcome {
    Started,
    AlreadyRunning,
}

#[derive(Debug, Clone)]
pub struct ContainerStatus {
    pub running: bool,
    pub health: Option<String>,
    pub started_at: Option<String>,
    pub exit_code: Option<i64>,
}

#[derive(Clone)]
pub struct DockerCtl {
    docker: Docker,
}

impl DockerCtl {
    pub async fn connect(url: &str) -> anyhow::Result<Self> {
        // Podman 4.9 compat API tops out around 1.41; bollard's default is
        // newer, so negotiate down or every call 400s.
        let docker = Docker::connect_with_http(url, 30, bollard::API_DEFAULT_VERSION)
            .context("bad DOCKER_API_URL")?
            .negotiate_version()
            .await
            .context("API version negotiation against socket-proxy failed")?;
        Ok(Self { docker })
    }

    pub async fn start(&self, name: &str) -> anyhow::Result<StartOutcome> {
        guard(name)?;
        match self.docker.start_container::<String>(name, None).await {
            Ok(()) => Ok(StartOutcome::Started),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 304, ..
            }) => Ok(StartOutcome::AlreadyRunning),
            Err(e) => Err(e).context("start failed"),
        }
    }

    pub async fn stop(&self, name: &str) -> anyhow::Result<()> {
        guard(name)?;
        self.docker.stop_container(name, None).await.context("stop failed")
    }

    pub async fn restart(&self, name: &str) -> anyhow::Result<()> {
        guard(name)?;
        self.docker.restart_container(name, None).await.context("restart failed")
    }

    pub async fn inspect(&self, name: &str) -> anyhow::Result<Option<ContainerStatus>> {
        guard(name)?;
        match self.docker.inspect_container(name, None).await {
            Ok(c) => {
                let state = c.state.unwrap_or_default();
                Ok(Some(ContainerStatus {
                    running: state.running.unwrap_or(false),
                    health: state.health.and_then(|h| h.status).map(|s| s.to_string()),
                    started_at: state.started_at,
                    exit_code: state.exit_code,
                }))
            }
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(None),
            Err(e) => Err(e).context("inspect failed"),
        }
    }

    pub async fn stats_mem(&self, name: &str) -> anyhow::Result<Option<(u64, u64)>> {
        guard(name)?;
        use bollard::container::StatsOptions;
        use futures_util::StreamExt as _;
        let mut s = self.docker.stats(
            name,
            Some(StatsOptions { stream: false, one_shot: true }),
        );
        match s.next().await {
            Some(Ok(st)) => Ok(st
                .memory_stats
                .usage
                .zip(st.memory_stats.limit)
                .map(|(u, l)| (u, l))),
            _ => Ok(None),
        }
    }

    pub async fn logs_tail(&self, name: &str, lines: usize) -> anyhow::Result<String> {
        guard(name)?;
        use bollard::container::LogsOptions;
        use futures_util::StreamExt as _;
        let mut out = String::new();
        let mut stream = self.docker.logs(
            name,
            Some(LogsOptions::<String> {
                stdout: true,
                stderr: true,
                tail: lines.to_string(),
                ..Default::default()
            }),
        );
        while let Some(Ok(chunk)) = stream.next().await {
            out.push_str(&chunk.to_string());
        }
        Ok(out)
    }
}
```

Add to `bot/Cargo.toml` dependencies: `futures-util = "0.3"`.

NOTE for the implementer: bollard 0.21 moved some option structs between modules across minor versions. If `bollard::container::StatsOptions` or `LogsOptions` fail to resolve, check `bollard::query_parameters` for the current names; the call shape stays the same. Fix imports until `cargo build` is clean; do not change the public interface of this module.

- [ ] **Step 3: Implement `bot/src/rcon.rs`**

```rust
use anyhow::Context as _;
use mc_query::rcon::RconClient;

pub struct McRcon {
    host: String,
    port: u16,
    password: String,
    conn: Option<RconClient>,
}

impl McRcon {
    pub fn new(host: String, port: u16, password: String) -> Self {
        Self { host, port, password, conn: None }
    }

    async fn ensure(&mut self) -> anyhow::Result<()> {
        if self.conn.is_none() {
            let mut c = RconClient::new(&self.host, self.port)
                .await
                .context("rcon connect")?;
            c.authenticate(&self.password).await.context("rcon auth")?;
            self.conn = Some(c);
        }
        Ok(())
    }

    /// One transparent reconnect: a dead persistent connection (server
    /// restarted) looks identical to a down server on the first error.
    pub async fn cmd(&mut self, cmd: &str) -> anyhow::Result<String> {
        self.ensure().await?;
        match self.conn.as_mut().unwrap().run_command(cmd).await {
            Ok(out) => Ok(out),
            Err(_) => {
                self.conn = None;
                self.ensure().await?;
                self.conn
                    .as_mut()
                    .unwrap()
                    .run_command(cmd)
                    .await
                    .context("rcon command failed after reconnect")
            }
        }
    }
}
```

- [ ] **Step 4: Run tests and build**

Run: `cd bot && cargo test && cargo build`
Expected: guard test passes, everything compiles (adjust bollard imports per the NOTE if needed).

- [ ] **Step 5: Commit**

```bash
git add bot && git commit -m "Bot: persistent RCON wrapper and guarded docker-compat control"
```

---

### Task 8: Monitor state machine

**Files:**
- Create: `bot/src/monitor.rs`
- Modify: `bot/src/main.rs` (add `mod monitor;`)

**Interfaces:**
- Consumes: `DockerCtl`, `McRcon` (Task 7), `Config` (Task 5).
- Produces: pure pieces `ServerState { Up, Starting, Unhealthy, Down }`, `classify(running: bool, health: Option<&str>, rcon_ok: bool) -> ServerState`, `Damper::new(initial: ServerState)`, `Damper::observe(&mut self, s: ServerState) -> Option<ServerState>` (Some = confirmed transition after 2 consecutive identical readings), `BackupWatch::default()`, `BackupWatch::observe(&mut self, running: bool, exit_code: Option<i64>) -> Option<BackupEvent>` with `BackupEvent { Succeeded, Failed(i64) }`; plus `run(...)` the 30 s loop used by Task 9.

- [ ] **Step 1: Write failing tests (bottom of `bot/src/monitor.rs`)**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_maps_observations() {
        assert_eq!(classify(false, None, false), ServerState::Down);
        assert_eq!(classify(true, Some("starting"), false), ServerState::Starting);
        assert_eq!(classify(true, Some("healthy"), true), ServerState::Up);
        assert_eq!(classify(true, Some("unhealthy"), true), ServerState::Unhealthy);
        // running, no healthcheck info, rcon answers: that's up
        assert_eq!(classify(true, None, true), ServerState::Up);
        // running but rcon dead and no health: still starting, not down
        assert_eq!(classify(true, None, false), ServerState::Starting);
    }

    #[test]
    fn damper_requires_two_consecutive_readings() {
        let mut d = Damper::new(ServerState::Up);
        assert_eq!(d.observe(ServerState::Down), None); // first sighting
        assert_eq!(d.observe(ServerState::Down), Some(ServerState::Down)); // confirmed
        assert_eq!(d.observe(ServerState::Down), None); // no repeat notifications
    }

    #[test]
    fn damper_resets_on_flap() {
        let mut d = Damper::new(ServerState::Up);
        assert_eq!(d.observe(ServerState::Down), None);
        assert_eq!(d.observe(ServerState::Up), None); // flap, no transition
        assert_eq!(d.observe(ServerState::Down), None); // counting restarts
        assert_eq!(d.observe(ServerState::Down), Some(ServerState::Down));
    }

    #[test]
    fn backup_watch_fires_once_per_run_end() {
        let mut w = BackupWatch::default();
        assert_eq!(w.observe(true, None), None); // started
        assert_eq!(w.observe(false, Some(1)), Some(BackupEvent::Failed(1)));
        assert_eq!(w.observe(false, Some(1)), None); // no repeat
        assert_eq!(w.observe(true, None), None);
        assert_eq!(w.observe(false, Some(0)), Some(BackupEvent::Succeeded));
    }
}
```

- [ ] **Step 2: Run, verify failure; implement above the tests**

```rust
use crate::config::Config;
use crate::docker::DockerCtl;
use crate::rcon::McRcon;
use poise::serenity_prelude::{ChannelId, Http};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerState {
    Up,
    Starting,
    Unhealthy,
    Down,
}

pub fn classify(running: bool, health: Option<&str>, rcon_ok: bool) -> ServerState {
    if !running {
        return ServerState::Down;
    }
    match health {
        Some("unhealthy") => ServerState::Unhealthy,
        Some("starting") => ServerState::Starting,
        Some("healthy") => ServerState::Up,
        _ if rcon_ok => ServerState::Up,
        _ => ServerState::Starting,
    }
}

pub struct Damper {
    current: ServerState,
    pending: Option<(ServerState, u8)>,
}

impl Damper {
    pub fn new(initial: ServerState) -> Self {
        Self { current: initial, pending: None }
    }

    pub fn observe(&mut self, s: ServerState) -> Option<ServerState> {
        if s == self.current {
            self.pending = None;
            return None;
        }
        match self.pending {
            Some((p, n)) if p == s && n + 1 >= 2 => {
                self.current = s;
                self.pending = None;
                Some(s)
            }
            Some((p, n)) if p == s => {
                self.pending = Some((p, n + 1));
                None
            }
            _ => {
                self.pending = Some((s, 1));
                None
            }
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum BackupEvent {
    Succeeded,
    Failed(i64),
}

#[derive(Default)]
pub struct BackupWatch {
    was_running: bool,
}

impl BackupWatch {
    pub fn observe(&mut self, running: bool, exit_code: Option<i64>) -> Option<BackupEvent> {
        let ev = if self.was_running && !running {
            match exit_code {
                Some(0) => Some(BackupEvent::Succeeded),
                Some(c) => Some(BackupEvent::Failed(c)),
                None => None,
            }
        } else {
            None
        };
        self.was_running = running;
        ev
    }
}

/// 30 s loop. Owns its own damper and watch; sends notifications to the
/// configured channel. Spawned from main after the Discord client is ready.
pub async fn run(
    http: Arc<Http>,
    cfg: Config,
    docker: DockerCtl,
    rcon: Arc<Mutex<McRcon>>,
) {
    let channel = ChannelId::new(cfg.notify_channel_id);
    let mut damper = Damper::new(ServerState::Down);
    let mut backups = BackupWatch::default();
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
    loop {
        tick.tick().await;

        let mc = docker.inspect("mc").await.ok().flatten();
        let rcon_ok = { rcon.lock().await.cmd("list").await.is_ok() };
        let state = classify(
            mc.as_ref().map(|s| s.running).unwrap_or(false),
            mc.as_ref().and_then(|s| s.health.as_deref()),
            rcon_ok,
        );
        if let Some(t) = damper.observe(state) {
            let msg = match t {
                ServerState::Up => ":green_circle: Server is up",
                ServerState::Starting => ":yellow_circle: Server is starting",
                ServerState::Unhealthy => ":orange_circle: Server is unhealthy (tick loop struggling), consider /restart",
                ServerState::Down => ":red_circle: Server is DOWN",
            };
            let _ = channel.say(&http, msg).await;
        }

        if let Ok(Some(b)) = docker.inspect("mc-backup").await {
            match backups.observe(b.running, b.exit_code) {
                Some(BackupEvent::Failed(c)) => {
                    let _ = channel
                        .say(&http, format!(":rotating_light: Backup FAILED with exit code {c}"))
                        .await;
                }
                Some(BackupEvent::Succeeded) => {
                    tracing::info!("backup finished ok");
                }
                None => {}
            }
        }
    }
}
```

- [ ] **Step 3: Run tests, verify pass**

Run: `cd bot && cargo test monitor`
Expected: 4 passed.

- [ ] **Step 4: Commit**

```bash
git add bot && git commit -m "Bot: monitored state machine with flap damping and backup watch"
```

---

### Task 9: Bot commands and main wiring

**Files:**
- Create: `bot/src/commands.rs`
- Modify: `bot/src/main.rs` (full rewrite below)

**Interfaces:**
- Consumes: everything above.
- Produces: `Data { cfg: Config, rcon: Arc<Mutex<McRcon>>, docker: DockerCtl }`, the poise command list `commands()`, and a runnable binary.

- [ ] **Step 1: Write `bot/src/commands.rs`**

```rust
use crate::config::Config;
use crate::docker::{DockerCtl, StartOutcome};
use crate::parse;
use crate::rcon::McRcon;
use poise::serenity_prelude::RoleId;
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct Data {
    pub cfg: Config,
    pub rcon: Arc<Mutex<McRcon>>,
    pub docker: DockerCtl,
}

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Ctx<'a> = poise::Context<'a, Data, Error>;

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![status(), start(), stop(), restart(), say(), whitelist(), backup()]
}

async fn is_admin(ctx: Ctx<'_>) -> Result<bool, Error> {
    let role = RoleId::new(ctx.data().cfg.admin_role_id);
    let ok = ctx
        .author_member()
        .await
        .map(|m| m.roles.contains(&role))
        .unwrap_or(false);
    if !ok {
        ctx.send(
            poise::CreateReply::default()
                .content("You need the admin role for that.")
                .ephemeral(true),
        )
        .await?;
    }
    Ok(ok)
}

/// Server status: players, TPS, uptime, memory.
#[poise::command(slash_command)]
pub async fn status(ctx: Ctx<'_>) -> Result<(), Error> {
    ctx.defer().await?;
    let d = ctx.data();
    let mc = d.docker.inspect("mc").await.ok().flatten();
    let running = mc.as_ref().map(|s| s.running).unwrap_or(false);
    if !running {
        ctx.say(format!(
            ":red_circle: **{}** is offline.",
            d.cfg.server_address
        ))
        .await?;
        return Ok(());
    }
    let (list, tps) = {
        let mut r = d.rcon.lock().await;
        (r.cmd("list").await.ok(), r.cmd("spark tps").await.ok())
    };
    let players = list.as_deref().and_then(parse::parse_list);
    let tps = tps.as_deref().and_then(parse::parse_tps);
    let mem = d.docker.stats_mem("mc").await.ok().flatten();

    let mut out = format!(":green_circle: **{}**\n", d.cfg.server_address);
    match players {
        Some(p) => {
            out += &format!("Players: {}/{}", p.online, p.max);
            if !p.names.is_empty() {
                out += &format!(" ({})", p.names.join(", "));
            }
            out += "\n";
        }
        None => out += "Players: RCON not answering yet\n",
    }
    if let Some(t) = tps {
        out += &format!(
            "TPS (1m/5m/15m): {:.1} / {:.1} / {:.1}{}\n",
            t.last_1m,
            t.last_5m,
            t.last_15m,
            if t.catching_up { " (catching up)" } else { "" }
        );
    }
    if let Some((used, limit)) = mem {
        out += &format!(
            "Memory: {:.1} / {:.1} GiB\n",
            used as f64 / 1e9 * 0.931,
            limit as f64 / 1e9 * 0.931
        );
    }
    if let Some(st) = mc.and_then(|s| s.started_at) {
        out += &format!("Container started: {st}\n");
    }
    ctx.say(out).await?;
    Ok(())
}

#[poise::command(slash_command)]
pub async fn start(ctx: Ctx<'_>) -> Result<(), Error> {
    if !is_admin(ctx).await? {
        return Ok(());
    }
    ctx.defer().await?;
    match ctx.data().docker.start("mc").await? {
        StartOutcome::Started => ctx.say("Starting the server.").await?,
        StartOutcome::AlreadyRunning => ctx.say("Already running.").await?,
    };
    Ok(())
}

#[poise::command(slash_command)]
pub async fn stop(ctx: Ctx<'_>) -> Result<(), Error> {
    if !is_admin(ctx).await? {
        return Ok(());
    }
    ctx.defer().await?;
    ctx.data().docker.stop("mc").await?;
    ctx.say("Server stopped. It stays stopped until /start.").await?;
    Ok(())
}

#[poise::command(slash_command)]
pub async fn restart(ctx: Ctx<'_>) -> Result<(), Error> {
    if !is_admin(ctx).await? {
        return Ok(());
    }
    ctx.defer().await?;
    ctx.data().docker.restart("mc").await?;
    ctx.say("Restarting.").await?;
    Ok(())
}

/// Relay a message to in-game chat.
#[poise::command(slash_command)]
pub async fn say(
    ctx: Ctx<'_>,
    #[description = "Message"] message: String,
) -> Result<(), Error> {
    ctx.defer().await?;
    match ctx
        .data()
        .rcon
        .lock()
        .await
        .cmd(&format!("say {message}"))
        .await
    {
        Ok(_) => ctx.say(format!("Sent: {message}")).await?,
        Err(_) => ctx.say("Server is offline, nothing sent.").await?,
    };
    Ok(())
}

#[poise::command(slash_command, subcommands("wl_add", "wl_remove", "wl_list"))]
pub async fn whitelist(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

#[poise::command(slash_command, rename = "add")]
pub async fn wl_add(
    ctx: Ctx<'_>,
    #[description = "Minecraft username"] name: String,
) -> Result<(), Error> {
    if !is_admin(ctx).await? {
        return Ok(());
    }
    ctx.defer().await?;
    whitelist_cmd(ctx, &format!("whitelist add {name}")).await
}

#[poise::command(slash_command, rename = "remove")]
pub async fn wl_remove(
    ctx: Ctx<'_>,
    #[description = "Minecraft username"] name: String,
) -> Result<(), Error> {
    if !is_admin(ctx).await? {
        return Ok(());
    }
    ctx.defer().await?;
    whitelist_cmd(ctx, &format!("whitelist remove {name}")).await
}

#[poise::command(slash_command, rename = "list")]
pub async fn wl_list(ctx: Ctx<'_>) -> Result<(), Error> {
    ctx.defer().await?;
    whitelist_cmd(ctx, "whitelist list").await
}

async fn whitelist_cmd(ctx: Ctx<'_>, cmd: &str) -> Result<(), Error> {
    match ctx.data().rcon.lock().await.cmd(cmd).await {
        Ok(out) => ctx.say(if out.is_empty() { "Done.".into() } else { out }).await?,
        Err(_) => {
            ctx.say("Server is offline; whitelist changes need it up.").await?
        }
    };
    Ok(())
}

#[poise::command(slash_command, subcommands("backup_now"))]
pub async fn backup(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

#[poise::command(slash_command, rename = "now")]
pub async fn backup_now(ctx: Ctx<'_>) -> Result<(), Error> {
    if !is_admin(ctx).await? {
        return Ok(());
    }
    ctx.defer().await?;
    let d = ctx.data();
    match d.docker.start("mc-backup").await? {
        StartOutcome::AlreadyRunning => {
            ctx.say("A backup is already running.").await?;
            return Ok(());
        }
        StartOutcome::Started => {}
    }
    // Poll to completion (max 10 min), then report honestly.
    for _ in 0..300 {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        if let Ok(Some(s)) = d.docker.inspect("mc-backup").await {
            if !s.running {
                let logs = d.docker.logs_tail("mc-backup", 5).await.unwrap_or_default();
                let code = s.exit_code.unwrap_or(-1);
                let verdict = if code == 0 {
                    ":white_check_mark: Backup finished"
                } else {
                    ":rotating_light: Backup FAILED"
                };
                ctx.say(format!("{verdict} (exit {code})\n```\n{logs}\n```"))
                    .await?;
                return Ok(());
            }
        }
    }
    ctx.say("Backup still running after 10 minutes, check the host.").await?;
    Ok(())
}
```

- [ ] **Step 2: Rewrite `bot/src/main.rs`**

```rust
mod commands;
mod config;
mod docker;
mod monitor;
mod parse;
mod rcon;

use commands::Data;
use config::Config;
use poise::serenity_prelude as serenity;
use std::sync::Arc;
use tokio::sync::Mutex;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cfg = Config::from_env()?;
    let (rcon_host, rcon_port) = cfg.rcon_host_port()?;

    // The proxy may come up after us; retry instead of crash-looping fast.
    let docker = {
        let mut attempt = 0u32;
        loop {
            match docker::DockerCtl::connect(&cfg.docker_api_url).await {
                Ok(d) => break d,
                Err(e) if attempt < 10 => {
                    attempt += 1;
                    tracing::warn!("docker API not ready ({e:#}), retry {attempt}/10");
                    tokio::time::sleep(std::time::Duration::from_secs(6)).await;
                }
                Err(e) => return Err(e),
            }
        }
    };

    let rcon = Arc::new(Mutex::new(rcon::McRcon::new(
        rcon_host,
        rcon_port,
        cfg.rcon_password.clone(),
    )));

    let intents = serenity::GatewayIntents::non_privileged();
    let guild = serenity::GuildId::new(cfg.guild_id);
    let monitor_cfg = cfg.clone();
    let monitor_docker = docker.clone();
    let monitor_rcon = rcon.clone();

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: commands::commands(),
            ..Default::default()
        })
        .setup(move |ctx, _ready, framework| {
            Box::pin(async move {
                poise::builtins::register_in_guild(
                    ctx,
                    &framework.options().commands,
                    guild,
                )
                .await?;
                tokio::spawn(monitor::run(
                    ctx.http.clone(),
                    monitor_cfg,
                    monitor_docker,
                    monitor_rcon,
                ));
                tracing::info!("commands registered, monitor running");
                Ok(Data { cfg, rcon, docker })
            })
        })
        .build();

    let mut client = serenity::ClientBuilder::new(&cfg_token()?, intents)
        .framework(framework)
        .await?;
    client.start().await?;
    Ok(())
}

fn cfg_token() -> anyhow::Result<String> {
    Ok(Config::from_env()?.discord_token)
}
```

NOTE for the implementer: `cfg` is moved into the setup closure; reading the token twice via `cfg_token()` avoids a borrow tangle. If you prefer, clone the token into a local before the closure instead. Either is fine; keep the rest identical.

- [ ] **Step 3: Full test suite, build, clippy**

Run: `cd bot && cargo test && cargo clippy -- -D warnings && cargo build --release`
Expected: all tests pass, zero clippy warnings, release binary builds. Expect to iterate on serenity/poise/bollard API details here; the compiler is the source of truth, the module interfaces above are the contract.

- [ ] **Step 4: Commit**

```bash
git add bot && git commit -m "Bot: slash commands, admin gating, wiring, monitor spawn"
```

---

### Task 10: Bot Dockerfile and CI workflow

**Files:**
- Create: `bot/Dockerfile`
- Create: `.github/workflows/images.yml`

**Interfaces:**
- Consumes: bot crate (Tasks 5-9), backup dir (Task 4).
- Produces: GHCR images `ghcr.io/juanloaiza21/hijuepapuscraft-bot:sha-<short>` and `-backup:sha-<short>`, referenced by `.env` and `containers/quadlet/bot.container`.

- [ ] **Step 1: Write `bot/Dockerfile`**

```dockerfile
# Build stage pins the rust minor that matches rust-version in Cargo.toml.
FROM docker.io/library/rust:1.96-slim AS chef
RUN cargo install cargo-chef --locked
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
# Dependency layer: only invalidated when Cargo.toml/lock change.
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN cargo build --release

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=builder /app/target/release/hijuepapus-bot /usr/local/bin/bot
USER nonroot
ENTRYPOINT ["/usr/local/bin/bot"]
```

- [ ] **Step 2: Verify the rust base tag exists, then local smoke build**

Run: `podman manifest inspect docker.io/library/rust:1.96-slim >/dev/null && echo TAG-OK`
Expected: `TAG-OK` (if not, use the newest 1.x-slim tag that is >= 1.74 and update the Dockerfile).
Run: `podman build -t bot-smoke bot/`
Expected: full build succeeds locally (native arch; arm64 happens in CI). This validates cargo-chef layering.

- [ ] **Step 3: Write `.github/workflows/images.yml`**

```yaml
name: images

on:
  push:
    branches: [main]
    paths: ['bot/**', 'backup/**', '.github/workflows/images.yml']

permissions:
  contents: read
  packages: write

jobs:
  bot:
    runs-on: ubuntu-24.04-arm
    steps:
      - uses: actions/checkout@v7
      - uses: docker/setup-buildx-action@v4
      - uses: docker/login-action@v4
        with:
          registry: ghcr.io
          username: ${{ github.actor }}
          password: ${{ secrets.GITHUB_TOKEN }}
      - uses: docker/metadata-action@v6
        id: meta
        with:
          images: ghcr.io/${{ github.repository_owner }}/hijuepapuscraft-bot
          tags: type=sha
      - uses: docker/build-push-action@v7
        with:
          context: bot
          platforms: linux/arm64
          push: true
          tags: ${{ steps.meta.outputs.tags }}
          labels: ${{ steps.meta.outputs.labels }}
          cache-from: type=gha,scope=bot
          cache-to: type=gha,scope=bot,mode=max

  backup:
    runs-on: ubuntu-24.04-arm
    steps:
      - uses: actions/checkout@v7
      - uses: docker/setup-buildx-action@v4
      - uses: docker/login-action@v4
        with:
          registry: ghcr.io
          username: ${{ github.actor }}
          password: ${{ secrets.GITHUB_TOKEN }}
      - uses: docker/metadata-action@v6
        id: meta
        with:
          images: ghcr.io/${{ github.repository_owner }}/hijuepapuscraft-backup
          tags: type=sha
      - uses: docker/build-push-action@v7
        with:
          context: backup
          platforms: linux/arm64
          push: true
          tags: ${{ steps.meta.outputs.tags }}
          labels: ${{ steps.meta.outputs.labels }}
          cache-from: type=gha,scope=backup
          cache-to: type=gha,scope=backup,mode=max
```

Notes baked into this file: `ubuntu-24.04-arm` is a native arm64 runner (GA for public repos since 2025-08-07), so no QEMU; action majors verified current 2026-08 (checkout v7, buildx v4, login v4, metadata v6, build-push v7); GHCR requires lowercase names.

- [ ] **Step 4: Validate YAML parses**

Run: `python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/images.yml')); print('YAML-OK')"`
Expected: `YAML-OK`.

- [ ] **Step 5: Commit**

```bash
git add bot/Dockerfile .github && git commit -m "Bot image (cargo-chef, distroless) and arm64 CI for both images"
```

### Task 11: Host bootstrap script

**Files:**
- Create: `scripts/bootstrap.sh`

**Interfaces:**
- Consumes: unit files from Task 2 (installs them), `.env.example` from Task 1.
- Produces: a configured host. Phase contract: `bootstrap.sh` (phase 1, safe anytime), `bootstrap.sh --harden` (phase 2, only after Tailscale works and key expiry is disabled).

- [ ] **Step 1: Write `scripts/bootstrap.sh`**

```bash
#!/usr/bin/env bash
# Idempotent bootstrap for the Oracle A1 Ubuntu 24.04 Minimal host.
# Phase 1 (default): user, hardening, podman, cockpit, tailscale, units, firewall for 25565.
# Phase 2 (--harden): restrict SSH and Cockpit to Tailscale. Run ONLY after:
#   1. tailscale up succeeded and you can SSH over the tailnet
#   2. key expiry is disabled for this node in the Tailscale admin console
# Manual Oracle console steps this script cannot do (see README):
#   VCN Security List ingress 25565/tcp from 0.0.0.0/0, reserved public IP.
set -euo pipefail

REPO_DIR=/opt/hijuepapuscraft
REPO_URL=https://github.com/juanloaiza21/hijuepapuscraft.git
ADMIN_USER="${ADMIN_USER:-papu}"
SSH_KEY="${SSH_KEY:-}"

need_root() { [[ $EUID -eq 0 ]] || { echo "run with sudo" >&2; exit 1; }; }

log() { echo ">>> $*"; }

ensure_pkg() {
  local missing=()
  for p in "$@"; do dpkg -s "$p" >/dev/null 2>&1 || missing+=("$p"); done
  if ((${#missing[@]})); then
    DEBIAN_FRONTEND=noninteractive apt-get install -y "${missing[@]}"
  fi
}

# Insert a rule before the first REJECT in a chain, only if absent.
# Published container ports traverse FORWARD (netavark DNAT), and OCI
# images pre-seed a FORWARD REJECT, so INPUT alone is not enough.
ensure_rule() {
  local chain=$1; shift
  if ! iptables -C "$chain" "$@" 2>/dev/null; then
    local pos
    pos=$(iptables -L "$chain" --line-numbers | awk '/REJECT/{print $1; exit}')
    if [[ -n "$pos" ]]; then
      iptables -I "$chain" "$pos" "$@"
    else
      iptables -A "$chain" "$@"
    fi
  fi
}

phase1() {
  log "timezone"
  timedatectl set-timezone America/Bogota

  log "apt update"
  apt-get update -y

  log "admin user ${ADMIN_USER}"
  if ! id "$ADMIN_USER" >/dev/null 2>&1; then
    adduser --disabled-password --gecos "" "$ADMIN_USER"
    usermod -aG sudo "$ADMIN_USER"
  fi
  if [[ -n "$SSH_KEY" ]]; then
    install -d -m 700 -o "$ADMIN_USER" -g "$ADMIN_USER" "/home/$ADMIN_USER/.ssh"
    grep -qxF "$SSH_KEY" "/home/$ADMIN_USER/.ssh/authorized_keys" 2>/dev/null \
      || echo "$SSH_KEY" >> "/home/$ADMIN_USER/.ssh/authorized_keys"
    chown "$ADMIN_USER:$ADMIN_USER" "/home/$ADMIN_USER/.ssh/authorized_keys"
    chmod 600 "/home/$ADMIN_USER/.ssh/authorized_keys"
  fi

  log "ssh hardening (key-only, no root)"
  install -m 644 /dev/stdin /etc/ssh/sshd_config.d/90-hijuepapus.conf <<'EOF'
PasswordAuthentication no
KbdInteractiveAuthentication no
PermitRootLogin no
EOF
  systemctl reload ssh || systemctl reload sshd

  log "packages: podman, firewall persistence, fail2ban, unattended-upgrades, git"
  ensure_pkg podman iptables-persistent netfilter-persistent fail2ban \
    unattended-upgrades git curl ca-certificates
  systemctl enable --now podman.socket

  log "cockpit from noble-backports (quadlet-aware cockpit-podman)"
  if ! grep -rq "noble-backports" /etc/apt/sources.list /etc/apt/sources.list.d/ 2>/dev/null; then
    echo "deb http://ports.ubuntu.com/ubuntu-ports noble-backports main universe" \
      > /etc/apt/sources.list.d/backports.list
    apt-get update -y
  fi
  DEBIAN_FRONTEND=noninteractive apt-get install -y -t noble-backports cockpit cockpit-podman
  systemctl enable --now cockpit.socket

  log "tailscale"
  if ! command -v tailscale >/dev/null 2>&1; then
    curl -fsSL https://tailscale.com/install.sh | sh
  fi

  log "unattended-upgrades on"
  install -m 644 /dev/stdin /etc/apt/apt.conf.d/20auto-upgrades <<'EOF'
APT::Periodic::Update-Package-Lists "1";
APT::Periodic::Unattended-Upgrade "1";
EOF

  log "repo at ${REPO_DIR}"
  if [[ -d "$REPO_DIR/.git" ]]; then
    git -C "$REPO_DIR" pull --ff-only
  else
    git clone "$REPO_URL" "$REPO_DIR"
  fi
  [[ -f "$REPO_DIR/.env" ]] || {
    cp "$REPO_DIR/.env.example" "$REPO_DIR/.env"
    chmod 600 "$REPO_DIR/.env"
    log "EDIT ${REPO_DIR}/.env BEFORE STARTING ANYTHING"
  }

  log "install quadlet and systemd units"
  install -d /etc/containers/systemd
  ln -sf "$REPO_DIR"/containers/quadlet/*.network /etc/containers/systemd/
  ln -sf "$REPO_DIR"/containers/quadlet/*.container /etc/containers/systemd/
  for u in "$REPO_DIR"/containers/systemd/*; do
    ln -sf "$u" "/etc/systemd/system/$(basename "$u")"
  done
  systemctl daemon-reload
  systemctl enable mc.service mc-backup.timer mc-restart.timer \
    restic-forget.timer restic-check.timer

  log "firewall phase 1: open 25565 in INPUT and FORWARD, persist"
  ensure_rule INPUT -p tcp --dport 25565 -j ACCEPT
  ensure_rule FORWARD -p tcp --dport 25565 -j ACCEPT
  ensure_rule FORWARD -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
  netfilter-persistent save

  log "phase 1 done. Next steps:"
  log "  1. tailscale up            (interactive auth)"
  log "  2. disable key expiry for this node in the Tailscale admin console"
  log "  3. edit ${REPO_DIR}/.env"
  log "  4. run the recreate scripts, start units (see README first run)"
  log "  5. re-run with --harden once SSH over Tailscale works"
}

phase2() {
  log "firewall phase 2: SSH and Cockpit via Tailscale only"
  # Remove the OCI-seeded world-open SSH rule if present.
  iptables -D INPUT -p tcp -m state --state NEW -m tcp --dport 22 -j ACCEPT 2>/dev/null || true
  ensure_rule INPUT -i tailscale0 -p tcp --dport 22 -j ACCEPT
  ensure_rule INPUT -s 100.64.0.0/10 -p tcp --dport 22 -j ACCEPT
  ensure_rule INPUT -i tailscale0 -p tcp --dport 9090 -j ACCEPT
  netfilter-persistent save
  log "hardened. Verify a NEW ssh session over Tailscale before closing this one."
  log "Break-glass if locked out: OCI console serial connection (see RUNBOOK)."
}

need_root
case "${1:-}" in
  --harden) phase2 ;;
  "") phase1 ;;
  *) echo "usage: $0 [--harden]" >&2; exit 2 ;;
esac
```

- [ ] **Step 2: Lint**

Run:
```bash
bash -n scripts/bootstrap.sh
podman run --rm -v "$PWD:/mnt:ro" docker.io/koalaman/shellcheck:stable /mnt/scripts/bootstrap.sh
```
Expected: clean. (shellcheck SC1091 on the sshd heredoc is not expected; if any SC warning appears, fix it rather than suppressing, except documented SC1090/SC1091 for runtime-sourced files.)

- [ ] **Step 3: Commit**

```bash
git add scripts/bootstrap.sh && git commit -m "Host bootstrap: two-phase idempotent setup with OCI FORWARD-chain handling"
```

---

### Task 12: Ops scripts

**Files:**
- Create: `scripts/restart-warn.sh`
- Create: `scripts/backup-now.sh`

**Interfaces:**
- Consumes: containers from Task 2, backup image contract from Task 4.
- Produces: `restart-warn.sh` (used by `mc-restart.service`), `backup-now.sh` (the manual pre-change snapshot path documented in MODS.md).

- [ ] **Step 1: Write `scripts/restart-warn.sh`**

```bash
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
```

- [ ] **Step 2: Write `scripts/backup-now.sh`**

```bash
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

exec podman run --rm \
  --network mcnet \
  -v mc-data:/data:ro \
  --env-file "$ENV_FILE" \
  -e MODE=backup \
  -e SNAPSHOT_TAG=pre-change \
  "${BACKUP_IMAGE}"
```

- [ ] **Step 3: Lint both**

Run:
```bash
bash -n scripts/restart-warn.sh scripts/backup-now.sh
podman run --rm -v "$PWD:/mnt:ro" docker.io/koalaman/shellcheck:stable /mnt/scripts/restart-warn.sh /mnt/scripts/backup-now.sh
```
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add scripts && git commit -m "Ops scripts: warned daily restart, manual pre-change snapshot"
```

---

### Task 13: Documentation (README, RUNBOOK, MODS)

**Files:**
- Create: `README.md`
- Create: `RUNBOOK.md`
- Create: `MODS.md`

**Interfaces:**
- Consumes: everything built; docs must reference only files, units, commands, and env vars that exist in this repo.

Style rule for all three files: concise direct prose, no em dashes anywhere.

- [ ] **Step 1: Write `README.md`** covering, in order:

1. One-paragraph overview and a mermaid architecture diagram (Discord -> bot -> {socket-proxy -> podman.sock, RCON -> mc}; timers -> mc-backup -> R2; players -> 25565).
2. Verified pins table (copy from the plan's Global Constraints, plus the GHCR SHA tags in use).
3. Manual Oracle console steps, exactly these: create VM.Standard.A1.Flex (2 OCPU, 12 GB) with the `Canonical-Ubuntu-24.04-Minimal-aarch64` image; reserve a public IP and attach it; VCN Security List ingress rule 25565/tcp from 0.0.0.0/0. State explicitly that bootstrap handles host iptables INPUT and FORWARD but cannot touch the VCN layer.
4. Cloudflare R2 setup: create account, add payment method (required even for free tier), create bucket `hijuepapus-backups`, create API token, fill `RESTIC_*`/`AWS_*` env vars, run `restic init` once via `podman run --rm --env-file /opt/hijuepapuscraft/.env --entrypoint restic $BACKUP_IMAGE init`.
5. Hostinger DNS: A record `mc` -> reserved IP, TTL 300; optional SRV `_minecraft._tcp.mc` port 25565; managed via the `hostinger` CLI (`hostinger dns records list hijuepapus.pro`) or the panel.
6. First run walkthrough: bootstrap phase 1 -> `tailscale up` -> disable key expiry -> edit `.env` -> `mc-recreate.sh` + `backup-recreate.sh` -> `systemctl start mcnet-network.service socket-proxy.service bot.service mc.service` -> EasyAuth config: after first boot edit `/data/config/EasyAuth/main.conf` via `podman exec -it mc sh` and set `premium-auto-login = true` and `prevent-offline-players-with-online-usernames = true`, then `podman restart mc` -> EasyAuth mixed-mode test with one premium and one cracked client -> Chunky pregen: `podman exec mc rcon-cli chunky radius 5000` then `podman exec mc rcon-cli chunky start`, expect hours of high CPU, progress via `chunky progress` -> bootstrap `--harden` -> reboot -> external connectivity test (`nc -vz mc.hijuepapus.pro 25565` from outside) -> UptimeRobot TCP monitor (liveness only, the bot monitors playability).
7. Host validation gate (from the spec, section 14): the four checks as a copy-paste block.

- [ ] **Step 2: Write `RUNBOOK.md`** with one section per scenario, each: symptoms, diagnosis commands, fix:

1. Server will not start: `systemctl status mc.service`, `podman logs --tail 100 mc`, common causes (bad .env, packwiz URL unreachable, EULA).
2. OOM: check `podman inspect mc | grep -i oom`, `free -h`, lower MEMORY or check backup timing.
3. TPS in the floor: `/status` first, then `podman exec mc rcon-cli spark profiler start` for 60 s and stop; read the spark URL it prints.
4. World corruption and restore drill: full command sequence from `backup/restore.sh`, ending with both recreate scripts, exactly as the script prints.
5. Oracle reclaimed or killed the instance: recreate VM, run bootstrap both phases, restore from R2, repoint the Hostinger A record via `hostinger dns` CLI, total data loss bounded by last nightly snapshot.
6. Locked out of SSH: OCI console serial connection path, then fix iptables or `tailscale up`.
7. Bot down: `systemctl status bot.service`, `podman logs bot`; the server keeps running without it.
8. Cracked player bought the game: their UUID changes; copy playerdata file under the world's `playerdata/` from old offline UUID to new premium UUID (get both from usercache.json), update whitelist entry.
9. Backup failure alert: `journalctl -u mc-backup.service`, `podman logs mc-backup`, common causes (R2 creds rotated, free tier exceeded, RCON password drift after recreate).

- [ ] **Step 3: Write `MODS.md`**: the backup-first rule (`sudo /opt/hijuepapuscraft/scripts/backup-now.sh` and wait for success), then add (`cd pack && packwiz modrinth add <slug>`), update (`packwiz update <mod>` or `--all`), remove (`packwiz remove <mod>`), always `packwiz refresh`, commit, push, then `podman restart mc` (container refetches the pack on start). Re-adding C2ME: `packwiz modrinth add c2me-fabric` only if chunk loading measurably lags, and remove it first if the server starts crash-looping. Rollback: `git revert` the pack commit, push, restart.

- [ ] **Step 4: Verify docs**

Run:
```bash
grep -n '—' README.md RUNBOOK.md MODS.md && echo "FAIL: em dashes" || echo CLEAN
grep -oE '(scripts|containers|backup)/[a-z-]+\.(sh|toml)' README.md RUNBOOK.md MODS.md | sort -u | while read -r f; do [ -e "${f#*:}" ] || echo "MISSING: $f"; done
```
Expected: `CLEAN` and no `MISSING:` lines.

- [ ] **Step 5: Commit**

```bash
git add README.md RUNBOOK.md MODS.md && git commit -m "Docs: README, RUNBOOK, MODS"
```

---

### Task 14: Push, CI green, pin the built images

**Files:**
- Modify: `containers/quadlet/bot.container` (replace `:changeme` tag)
- Modify: `.env.example` (replace `:changeme` tags)

- [ ] **Step 1: Push and watch CI**

Run: `git push && gh run watch --exit-status`
Expected: `images` workflow green on both jobs. If the bot job fails on crate API drift, fix in `bot/`, commit, push again.

- [ ] **Step 2: Record the built tags**

Run: `gh api /users/juanloaiza21/packages/container/hijuepapuscraft-bot/versions --jq '.[0].metadata.container.tags'`
Expected: a `sha-<short>` tag. Put that tag into `containers/quadlet/bot.container` (`Image=ghcr.io/juanloaiza21/hijuepapuscraft-bot:sha-<short>`) and both image vars in `.env.example`.

- [ ] **Step 3: Commit and push**

```bash
git add containers/quadlet/bot.container .env.example
git commit -m "Pin built image tags"
git push
```

- [ ] **Step 4: Final repo check**

Run: `git status --short` (clean), `git log --oneline | head -20` (one commit per task), and confirm `https://raw.githubusercontent.com/juanloaiza21/hijuepapuscraft/main/pack/pack.toml` returns the pack (curl it).
Expected: all good. Deployment to the actual Oracle host follows the README first-run walkthrough and is done with the user, not by this plan.

