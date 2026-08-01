# HijuepapusCraft Infrastructure Design

Date: 2026-08-01
Status: approved pending user review
Owners: juanloaiza21 plus one co-maintainer

## 1. Goal

A private, whitelisted, 24/7 Fabric Minecraft 1.21.1 server for 8 friends on an Oracle Cloud Always Free Ampere A1 instance, managed entirely from this repo. A Rust Discord bot handles day-to-day operations so nobody needs a terminal. Backups go off-instance to Cloudflare R2. Everything is pinned, documented, and restorable.

All decisions below were discussed and approved in the design conversation. Facts marked "verified" were checked against primary sources on 2026-08-01.

## 2. Target environment

- Host: OCI VM.Standard.A1.Flex, 2 OCPU, 12 GB RAM, 200 GB block volume
- OS: Ubuntu 24.04 Minimal aarch64 (official OCI platform image `Canonical-Ubuntu-24.04-Minimal-aarch64`, verified). No snapd. Idle footprint roughly 150-200 MB.
- Public IP: OCI reserved (static) public IP, created in the console so it survives instance recreation
- Domain: `hijuepapus.pro`, registered at Hostinger. Play address `mc.hijuepapus.pro` (A record to the reserved IP, TTL 300). Optional SRV record `_minecraft._tcp.mc.hijuepapus.pro` pointing at port 25565. Admin surfaces get no public DNS; Tailscale MagicDNS covers them.
- Timezone for all schedules: America/Bogota

## 3. Container runtime: Podman, hybrid layout

Podman 4.9.3 rootful (Ubuntu noble repo, verified) replaces Docker. Rationale: container runtime overhead is near zero, but the Docker daemon idles at 200-300 MB; Podman is daemonless and its docker-compatible API socket (`podman.socket`) is systemd socket-activated. Rootless mode is rejected for now: user-mode networking taxes game traffic and complicates the port bind; revisit later if desired.

A verified Quadlet limitation dictates a hybrid layout: Quadlet units always generate `--rm` plus `podman rm` on stop, so a container stopped through the docker-compat API ceases to exist and a subsequent API start returns 404 (upstream wontfix). Therefore:

| Unit | Managed by | Why |
|---|---|---|
| `mc` (Minecraft) | Plain container, `--restart=on-failure`, created by versioned `containers/mc-recreate.sh`, boot-started by a small systemd unit | The bot must stop/start it via the compat API. Plain containers have Docker-identical semantics (verified): crashes auto-restart, deliberate stops stay stopped, API start works. |
| `bot` | Quadlet `.container` | Never externally stopped. systemd `Restart=always` gives the 24/7 guarantee. |
| `socket-proxy` | Quadlet `.container` | Same. |
| `mc-backup` | Plain container, `--restart=no`, pre-created by `containers/backup-recreate.sh` | Runs to completion per invocation. Bot triggers it with the compat-API start permission it already has; timers trigger the same path. |
| Cockpit | Host package, not a container | Socket-activated web UI, near zero idle RAM. |

Networking: one Podman network `mcnet` (Quadlet `.network`). Members: mc, bot, socket-proxy, mc-backup. Only published port on the host: 25565/tcp (Minecraft). RCON (25575) and the proxy API (2375) exist only inside `mcnet`. The bot has zero inbound ports (outbound-only Discord gateway).

There is no compose file. The declarative sources of truth are the Quadlet units plus the two recreate scripts, all versioned in `containers/`.

## 4. Minecraft service

Image: `itzg/minecraft-server:2026.7.2-java21` (verified: latest stable versioned tag, linux/arm64 manifest present).

Key environment (full list in `.env.example`):

- `TYPE=FABRIC`, `VERSION=1.21.1`, `EULA=TRUE`
- `ONLINE_MODE=TRUE` with EasyAuth mixed mode (section 5). Premium friends auto-login with real UUIDs and skins; cracked-client friends use password auth.
- `ENABLE_WHITELIST=TRUE`, `ENFORCE_WHITELIST=TRUE`
- `USE_AIKAR_FLAGS=true`, `MEMORY=6G`, `INIT_MEMORY=6G`
- `ENABLE_RCON=true`, password from env, port never published
- `ENABLE_AUTOPAUSE=FALSE` (explicit: Oracle reclaims Always Free instances when CPU, memory, and network all sit under 20 percent for 7 days; an idling JVM works against us)
- `PACKWIZ_URL=https://raw.githubusercontent.com/juanloaiza21/hijuepapuscraft/main/pack/pack.toml` (repo is public)
- `DIFFICULTY=hard` (carried over from the previous server)

Heap justification: 6 GB heap means roughly 7 to 7.5 GB JVM RSS (metaspace, GC, netty). System overhead on this stack is about 350 MB (minimal OS, conmon per container, bot, proxy, idle Cockpit). That leaves 4 GB or more for page cache and the backup container. Page cache does more for chunk latency than extra heap. Raising to 7-8 GB later is a one-line `.env` change with ample margin.

Healthcheck: `mc-health` (bundled in the image), attached in `mc-recreate.sh`. Podman takes no automatic action on unhealthy; the bot's monitor loop is the actor (section 6). Restart policy `on-failure` covers crashes.

World data: named volume `mc-data`. `rcon-cli` inside the image is used by host scripts via `podman exec` so RCON credentials never leave the container env.

## 5. Authentication: EasyAuth mixed mode

- EasyAuth 3.4.4 (verified: Fabric 1.21.1 build, published 2026-07-27, Modrinth slug `easyauth`), `premium-auto-login=true`, `prevent-offline-players-with-online-usernames=true`
- EasyWhitelist (same author) makes the whitelist name-based instead of UUID-based so it works for offline-UUID players. The bot's RCON `whitelist add/remove/list` keeps working unchanged.
- Risk flag (verified with a caveat): mixed mode with `online-mode=true` is documented by the author's wiki, but we test it empirically with one premium and one cracked client during first run. Fallback if it misbehaves: flip to `ONLINE_MODE=FALSE`, all players password-auth via EasyAuth, everything else unchanged.
- Known offline-mode side effect that still applies to cracked players: if one later buys the game, their UUID changes and their inventory needs a manual migration (documented in RUNBOOK).

## 6. Discord bot (Rust)

Slash-command bot, outbound-only, zero inbound ports.

Crates (verified current): `serenity` + `poise` (Discord, slash commands), `rcon` (Minecraft RCON), `bollard` 0.21.0 (docker-compat API; verified first-class Podman support), `tokio`, `anyhow` + `thiserror`, `tracing`.

Transports, strictly two:

1. RCON to `mc:25575` inside `mcnet` for in-game actions: whitelist, say, list, `spark tps` (Fabric has no vanilla TPS command; Spark is queried over RCON and its output parsed, color codes stripped).
2. HTTP to `socket-proxy:2375` via bollard for container lifecycle and stats only.

Commands:

- `/status`: online players (RCON `list`), TPS (`spark tps`), uptime (container inspect), memory (container stats). Reports "offline" cleanly when the server is down.
- `/start`, `/stop`, `/restart`: container lifecycle through the proxy. Gated on `DISCORD_ADMIN_ROLE_ID`.
- `/whitelist add|remove|list`: RCON. If the server is down, replies that the whitelist is unavailable until the server is up (vanilla whitelist has no offline write path we trust).
- `/say <message>`: RCON `say`.
- `/backup now`: compat-API start of the pre-created `mc-backup` container. Gated on admin role.

Behavior:

- Monitor loop every 30 s: container state via proxy plus RCON reachability, with a small state machine (up, starting, down). On transitions it posts to `DISCORD_NOTIFY_CHANNEL_ID`. Health flapping is dampened (two consecutive readings before a transition).
- Bot-side guard: lifecycle calls are restricted to the container names `mc` and `mc-backup`, since the proxy cannot filter by container.
- All config via env. Slash commands registered guild-scoped (instant propagation, private server).

Packaging: multi-stage Dockerfile, `linux/arm64`, `cargo-chef` for a cached dependency layer, distroless (`cc`) final stage. Built in GitHub Actions on `ubuntu-24.04-arm` runners (native arm64, free for public repos), pushed to GHCR with pinned tags. The host only ever pulls. Native host builds and cross-compilation from dev machines are explicitly rejected: slow or toolchain-fiddly, and CI serves both maintainers identically.

## 7. Socket proxy

Image: `linuxserver/socket-proxy` (verified: actively maintained, linux/arm64 published, Podman-aware; replaces tecnativa which has an unresolved Podman 503 issue). Exact tag pinned at build time.

Mounts `/run/podman/podman.sock` read path, exposes filtered HTTP on `mcnet` only. Allowed: `CONTAINERS=1` (list, inspect, stats), `ALLOW_START=1`, `ALLOW_STOP=1`, `ALLOW_RESTARTS=1`. Everything else denied, `POST=0` baseline. The bot never sees the raw socket.

Accepted residual risk: the proxy grants start/stop on any container, not per-container. Mitigations: proxy reachable only from `mcnet`, bot-side name allowlist, no other tenants on the host.

## 8. Mod pack (packwiz)

Baseline, all verified to have Fabric 1.21.1 builds:

| Mod | Purpose |
|---|---|
| Lithium | Game logic optimization |
| FerriteCore | RAM reduction |
| Krypton | Network stack optimization |
| Chunky | One-time world pre-generation |
| Spark | Profiling, TPS source for the bot |
| EasyAuth | Mixed-mode authentication |
| EasyWhitelist | Name-based whitelist |

C2ME is deliberately excluded: its parallel chunk generation pays off at 4 plus cores, and on 2 OCPU it adds the pack's most crash-prone mod for marginal gain. Chunky pre-generation removes most chunk-gen load permanently. Adding C2ME later is one packwiz command if chunk loading measurably lags.

Workflow: `packwiz modrinth add <slug>` in `pack/`, commit, push, restart server (MODS.md documents this, including the backup-first rule). The container refreshes the pack on start via `PACKWIZ_URL`.

## 9. Backups: restic to Cloudflare R2

Verified: restic over R2's S3 API is known-good on any restic 0.14 plus, no special flags. Free tier: 10 GB storage, 1M Class A writes, 10M Class B reads monthly, zero egress. Heads-up documented in README: enabling R2 requires a card or PayPal on the Cloudflare account even within the free tier.

Implementation: a small `mc-backup` image (alpine, restic, itzg's `rcon-cli`, `backup.sh`), built in the same CI pipeline, arm64. One container, one code path, two triggers (bot and timer).

Backup flow: RCON `save-off`, `save-all flush`, restic snapshot of the `mc-data` volume, RCON `save-on`. `save-on` runs in a trap so a failed snapshot never leaves saving disabled. If the server is down, RCON steps are skipped and the snapshot proceeds (cold backup is the safest kind).

Schedule (systemd timers, versioned in repo, installed by bootstrap):

- Nightly snapshot 04:30
- Weekly `restic forget --keep-daily 7 --keep-weekly 4 --keep-monthly 6 --prune`
- Monthly `restic check`
- Prune and check are deliberately infrequent: R2 bills ListObjects as Class A writes

Manual pre-change snapshot: `scripts/backup-now.sh` (tags the snapshot `pre-change`), mandated by MODS.md before touching the pack.

Restore: `backup/restore.sh` restores a chosen snapshot into a fresh volume and the RUNBOOK documents the full drill. The restore path is executed once for real during initial build, against a scratch volume, before the server goes live. A backup nobody has restored is not a backup.

## 10. Host bootstrap (`scripts/bootstrap.sh`)

Idempotent bash, safe to re-run, two-phase to avoid SSH lockout:

Phase 1 (default): non-root admin user, SSH key-only, password and root login disabled, Podman, Cockpit plus cockpit-podman from noble-backports (verified: stock noble version predates Quadlet integration), Tailscale (interactive `tailscale up`), fail2ban, unattended-upgrades, timers and units installed from `containers/systemd/`, Quadlet units linked into `/etc/containers/systemd/`, `.env` scaffolded.

Phase 2 (`--harden`, run only after Tailscale connectivity is confirmed): rewrites the firewall so 25565/tcp is open to the world, SSH and Cockpit (9090) accept only from the `tailscale0` interface, everything else drops.

Oracle-specific handling, prominently documented in README:

- OCI Ubuntu images ship pre-seeded restrictive iptables rules persisted via netfilter-persistent in `/etc/iptables/rules.v4`. The script inserts the 25565 ACCEPT ahead of the REJECT rule and persists. It does not blindly flush Oracle's chains.
- The VCN Security List is a separate layer that the script cannot touch. README lists the exact console clicks: ingress rule for 25565/tcp from 0.0.0.0/0, plus reserving the static public IP. Nothing else is opened at the VCN; Tailscale traffic rides the existing stateful egress.

## 11. Operations

- Daily restart 06:00 via timer: `scripts/restart-warn.sh` broadcasts in-game warnings at 5 minutes, 1 minute, and 10 seconds (via `podman exec mc rcon-cli say`), then `podman restart mc`. Skips the waits when `list` shows zero players.
- Chunky pre-generation: documented one-time run (console: `chunky radius 5000`, `chunky start`) before friends are invited; expect hours of high CPU, run it before enabling the uptime monitor to avoid alert noise.
- Uptime monitoring: UptimeRobot free tier, TCP check against `mc.hijuepapus.pro:25565`, alerting to Discord via webhook. Setup documented in README (external service, no repo artifact).
- Image updates are manual and deliberate: bump the pinned tag in the repo, pull, recreate. podman-auto-update is intentionally not used.

## 12. Documentation set

- `README.md`: architecture diagram, verified pins table, Oracle console manual steps (VCN rule, reserved IP), Cloudflare R2 card requirement, Hostinger DNS records, first-run walkthrough end to end.
- `RUNBOOK.md`: server will not start, OOM, TPS in the floor (Spark triage), world corruption plus restore drill, Oracle reclaimed the instance (full rebuild from bootstrap plus restore, DNS repointing), bot down, cracked player bought the game (UUID migration).
- `MODS.md`: packwiz add/update/remove, backup-first rule, rollout procedure, how to re-add C2ME if ever wanted.

## 13. Repository layout

```
hijuepapuscraft/
├── README.md
├── RUNBOOK.md
├── MODS.md
├── .env.example          # every secret and knob, dummy values; real .env gitignored
├── .gitignore
├── containers/
│   ├── quadlet/
│   │   ├── bot.container
│   │   ├── socket-proxy.container
│   │   └── mc.network
│   ├── systemd/          # mc.service (boot start), timers: backup, restart, prune, check
│   ├── mc-recreate.sh    # single source of truth for the mc container definition
│   └── backup-recreate.sh
├── bot/
│   ├── Cargo.toml
│   ├── Dockerfile        # cargo-chef, distroless cc, linux/arm64
│   └── src/              # main, config, commands/, rcon, docker, monitor
├── backup/
│   ├── Dockerfile        # alpine + restic + rcon-cli
│   ├── backup.sh
│   └── restore.sh
├── pack/                 # packwiz: pack.toml, index.toml, mods/*.pw.toml
├── scripts/
│   ├── bootstrap.sh
│   ├── restart-warn.sh
│   └── backup-now.sh
├── docs/superpowers/specs/
└── .github/workflows/images.yml   # bot + backup images -> GHCR, ubuntu-24.04-arm
```

## 14. Build order

Component by component, with a check-in after each:

1. Repo scaffold: `.gitignore`, `.env.example`, `containers/` (Quadlet units, recreate scripts, systemd units)
2. packwiz pack with the seven baseline mods, exact versions pinned
3. `bootstrap.sh`
4. Backup image, scripts, timers; restore drill executed against a scratch volume
5. Discord bot plus Dockerfile plus CI (largest component; crate versions re-verified against crates.io at write time)
6. Ops extras: restart-warn, Chunky doc, UptimeRobot doc
7. README, RUNBOOK, MODS written against what was actually built

## 15. Risks and mitigations

| Risk | Mitigation |
|---|---|
| EasyAuth mixed mode misbehaves with online-mode=true | Empirical test at first run with premium and cracked clients; fallback flip to ONLINE_MODE=FALSE documented |
| linuxserver/socket-proxy quirk against Podman compat API | Validated during component 1 with curl before the bot exists; tecnativa or direct-socket-with-bot-side-filtering as fallbacks |
| Oracle reclaims the instance | AUTOPAUSE off keeps baseline load; RUNBOOK rebuild drill; reserved IP and DNS make recovery a repoint; nightly off-instance backups bound data loss to 24h |
| R2 free tier exceeded (10 GB) | restic dedup plus retention policy keeps a small world well under; `restic stats` check documented in RUNBOOK |
| Quadlet/Podman version drift on Ubuntu LTS | Podman stays 4.9.x for the LTS lifetime (verified); all behaviors used are in 4.9 |
| Bot compromise | Blast radius limited to RCON commands and start/stop of two named containers; no raw socket, no host access, no inbound ports |
