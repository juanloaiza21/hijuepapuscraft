# HijuepapusCraft Infrastructure Design

Date: 2026-08-01
Status: draft for user review
Owners: juanloaiza21, second engineer to be added when they join the repo

## 1. Goal

A private, whitelisted, 24/7 Fabric Minecraft 1.21.1 server for 8 friends on an Oracle Cloud Always Free Ampere A1 instance, managed entirely from this repo. A Rust Discord bot handles day-to-day operations so nobody needs a terminal. Backups go off-instance to Cloudflare R2. Everything is pinned, documented, and restorable.

All decisions below were discussed and approved in the design conversation. Facts marked "verified" were checked against primary sources on 2026-08-01. The spec was additionally reviewed by an adversarial panel; its findings are folded in.

## 2. Target environment

- Host: OCI VM.Standard.A1.Flex, 2 OCPU, 12 GB RAM, 200 GB block volume
- OS: Ubuntu 24.04 Minimal aarch64 (official OCI platform image `Canonical-Ubuntu-24.04-Minimal-aarch64`, verified). No snapd. Idle footprint roughly 150-200 MB.
- Public IP: OCI reserved (static) public IP, created in the console so it survives instance recreation
- Domain: `hijuepapus.pro`, registered at Hostinger. Play address `mc.hijuepapus.pro` (A record to the reserved IP, TTL 300). Optional SRV record `_minecraft._tcp.mc.hijuepapus.pro` pointing at port 25565. Admin surfaces get no public DNS; Tailscale MagicDNS covers them.
- Timezone: all schedules are America/Bogota. Mechanism, not just intent: bootstrap runs `timedatectl set-timezone America/Bogota`, and every timer additionally carries an explicit timezone in its calendar expression (`OnCalendar=*-*-* 04:30:00 America/Bogota`) so a rebuilt host that missed the timedatectl step still fires at the right local time. OCI images boot in UTC; without this the nightly backup would fire at 23:30 Bogota, mid-play.

## 3. Container runtime: Podman, hybrid layout

Podman 4.9.3 rootful (Ubuntu noble repo, verified) replaces Docker. Rationale: container runtime overhead is near zero, but the Docker daemon idles at 200-300 MB; Podman is daemonless and its docker-compatible API socket (`podman.socket`) is systemd socket-activated. Rootless mode is rejected for now: user-mode networking taxes game traffic and complicates the port bind; revisit later if desired.

A verified Quadlet limitation dictates a hybrid layout: Quadlet units always generate `--rm` plus `podman rm` on stop, so a container stopped through the docker-compat API ceases to exist and a subsequent API start returns 404 (upstream wontfix). Therefore:

| Unit | Managed by | Why |
|---|---|---|
| `mc` (Minecraft) | Plain container, `--restart=on-failure`, created by versioned `containers/mc-recreate.sh`, boot-started by systemd unit `mc.service` | The bot must stop/start it via the compat API. Plain containers have Docker-identical semantics (verified): crashes auto-restart, deliberate stops stay stopped, API start works. |
| `bot` | Quadlet `.container` with `ContainerName=bot` | Never externally stopped. systemd `Restart=always` gives the 24/7 guarantee. |
| `socket-proxy` | Quadlet `.container` with `ContainerName=socket-proxy` | Same. Without ContainerName, Quadlet names it `systemd-socket-proxy` and the bot's `socket-proxy:2375` address would not resolve. |
| `mc-backup` | Plain container, `--restart=no`, pre-created by `containers/backup-recreate.sh` | Runs to completion per invocation. Bot triggers it with the compat-API start permission it already has; the timer runs the same image attached. |
| Cockpit | Host package, not a container | Socket-activated web UI, near zero idle RAM. |

Networking: one Podman network named `mcnet`, defined by the Quadlet file `mcnet.network` containing an explicit `NetworkName=mcnet` (without it Quadlet would create `systemd-mcnet` and every reference would break). Members: mc, bot, socket-proxy, mc-backup. Only published port on the host: 25565/tcp (Minecraft). RCON (25575) and the proxy API (2375) exist only inside `mcnet`. The bot has zero inbound ports (outbound-only Discord gateway).

Boot ordering: `mc.service` declares `Wants=`/`After=` on the Quadlet-generated `mcnet-network.service` and on `podman.socket`, with retry on failure, so a reboot cannot race the network into a silently down server. The recreate scripts require Quadlet units installed and `systemctl daemon-reload` run first; bootstrap enforces that order.

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

RCON credential scope, stated precisely: `RCON_PASSWORD` has exactly three consumers, all via container env from `.env`: the mc server, the bot, and the mc-backup container. The port is never published outside `mcnet`. Host scripts never handle the password; they go through `podman exec mc rcon-cli`, which reads it inside the container.

Healthcheck: `mc-health` (bundled in the image), attached in `mc-recreate.sh`. Podman takes no automatic action on unhealthy; consumption is defined in section 6 (bot reads health via inspect, notify-only by design). The host validation gate (section 14) confirms health status keeps being reported after a restart through the compat API, since Podman health timers are recreated on start and the bot's path must be the one tested.

World data: named volume `mc-data`.

## 5. Authentication: EasyAuth mixed mode

- EasyAuth 3.4.4 (verified: Fabric 1.21.1 build, published 2026-07-27, Modrinth slug `easyauth`), `premium-auto-login=true`, `prevent-offline-players-with-online-usernames=true`
- EasyWhitelist (same author) makes the whitelist name-based instead of UUID-based so it works for offline-UUID players. The bot's RCON `whitelist add/remove/list` keeps working unchanged.
- Risk flag (verified with a caveat): mixed mode with `online-mode=true` is documented by the author's wiki, but we test it empirically with one premium and one cracked client during first run. Fallback if it misbehaves: flip to `ONLINE_MODE=FALSE`, all players password-auth via EasyAuth, everything else unchanged.
- Known offline-mode side effect that still applies to cracked players: if one later buys the game, their UUID changes and their inventory needs a manual migration (documented in RUNBOOK).

## 6. Discord bot (Rust)

Slash-command bot, outbound-only, zero inbound ports.

Crates (verified current): `serenity` + `poise` (Discord, slash commands), `rcon` (Minecraft RCON), `bollard` 0.21.0 (docker-compat API; verified first-class Podman support), `tokio`, `anyhow` + `thiserror`, `tracing`.

Transports, strictly two:

1. RCON to `mc:25575` inside `mcnet` for in-game actions: whitelist, say, list, `spark tps` (Fabric has no vanilla TPS command; Spark is queried over RCON and its output parsed, color codes stripped). The bot holds one persistent RCON connection with reconnect-on-error; a fresh connection every poll would write an RCON client line to the server console every 30 s and bury real logs.
2. HTTP to `socket-proxy:2375` via bollard. At startup the client negotiates the API version down to Podman 4.9's compat maximum (about 1.41); bollard's default version is newer and unnegotiated requests would be rejected. Version negotiation is part of the validation gate.

Commands:

- `/status`: online players (RCON `list`), TPS (`spark tps`), uptime (container inspect), memory (container stats). Reports "offline" cleanly when the server is down.
- `/start`, `/stop`, `/restart`: container lifecycle through the proxy. Gated on `DISCORD_ADMIN_ROLE_ID`.
- `/whitelist add|remove|list`: RCON. If the server is down, replies that the whitelist is unavailable until the server is up (vanilla whitelist has no offline write path we trust).
- `/say <message>`: RCON `say`.
- `/backup now`: compat-API start of the pre-created `mc-backup` container, then polls inspect until the container stops and reports the exit code plus the last log lines (logs are a GET, already permitted). An HTTP 304 on start means a backup is already running and is reported as such, never as success. Gated on admin role.

Behavior:

- Monitor loop every 30 s: container state and `.State.Health` via proxy inspect, plus RCON reachability, feeding a small state machine (up, starting, unhealthy, down). Transitions post to `DISCORD_NOTIFY_CHANNEL_ID` after two consecutive identical readings (flap damping). Unhealthy is notify-only by design: the bot alerts and an admin decides on `/restart`; no automatic remediation loops.
- The monitor also watches `mc-backup` exit transitions and alerts on any nonzero exit, so a failing nightly backup is loud within a minute instead of silent for weeks.
- Bot-side guard: lifecycle calls are restricted to the container names `mc` and `mc-backup`, since the proxy cannot filter by container.
- All config via env. Slash commands registered guild-scoped (instant propagation, private server).

Packaging: multi-stage Dockerfile, `linux/arm64`, `cargo-chef` for a cached dependency layer, distroless (`cc`) final stage. Built in GitHub Actions on `ubuntu-24.04-arm` runners (native arm64, free for public repos), pushed to GHCR tagged with the immutable git SHA (never `latest`); the host pins the SHA tag in `.env`. Native host builds and cross-compilation from dev machines are explicitly rejected: slow or toolchain-fiddly, and CI serves both maintainers identically.

## 7. Socket proxy

Image: `linuxserver/socket-proxy` (verified: actively maintained, linux/arm64 published, Podman-aware; replaces tecnativa which has an unresolved Podman 503 issue). The exact tag and digest are resolved and pinned during component 1 and recorded in the README pins table; the Quadlet unit references the digest.

Mounts `/run/podman/podman.sock`, exposes filtered HTTP on `mcnet` only. Allowed: `CONTAINERS=1` (list, inspect, stats, logs), `ALLOW_START=1`, `ALLOW_STOP=1`, `ALLOW_RESTARTS=1` (verified: these explicitly bypass `POST=0`). Everything else denied, `POST=0` baseline, `EXEC=0`. The bot never sees the raw socket.

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

Tool choice, as originally asked: restic over rclone. rclone synchronizes files; it keeps exactly one remote copy, so corruption or a bad world state propagates to the backup on the next sync. restic takes deduplicated, encrypted, point-in-time snapshots with retention policies, so thirty snapshots of a slowly-mutating world cost little more than one and any of them can be restored. For backups, snapshots win; rclone is not in the design.

Target: Cloudflare R2. Verified: restic over R2's S3 API is known-good on any restic 0.14 plus, no special flags. Free tier: 10 GB storage, 1M Class A writes, 10M Class B reads monthly, zero egress. Heads-up documented in README: enabling R2 requires a card or PayPal on the Cloudflare account even within the free tier.

Implementation: a small `mc-backup` image (alpine, restic, itzg's `rcon-cli`, `backup.sh`), built in the same CI pipeline, arm64, GHCR, git-SHA tags. `backup.sh` dispatches on the `MODE` env var: `backup` (default), `forget`, `check`. One image, one script, every trigger goes through it.

Backup flow (`MODE=backup`): RCON `save-off`, `save-all flush`, restic snapshot of the `mc-data` volume, RCON `save-on`. `save-on` runs in a trap so a failed snapshot never leaves saving disabled. If the server is down, RCON steps are skipped and the snapshot proceeds (cold backup is the safest kind). An optional `SNAPSHOT_TAG` env var is applied to the snapshot.

Triggers, three, none fire-and-forget:

1. Nightly timer 04:30: `mc-backup.service` runs `podman start -a mc-backup` (attached, exit code propagates, so systemd sees failures) on the pre-created container.
2. Bot `/backup now`: compat-API start of the same pre-created container, with the polling and exit-code reporting described in section 6.
3. Manual pre-change snapshot `scripts/backup-now.sh`: `podman run --rm` from the same image with `MODE=backup` and `SNAPSHOT_TAG=pre-change`. Mandated by MODS.md before touching the pack. A fresh `podman run` is used here because the pre-created container has a fixed env and cannot take a per-invocation tag.

Failure visibility is double-covered: the attached timer run fails the systemd unit on nonzero exit, and the bot's monitor loop independently alerts on any nonzero mc-backup exit (section 6).

Maintenance timers, both running `podman run --rm` from the pinned image:

- Weekly `MODE=forget`: `restic forget --keep-daily 7 --keep-weekly 4 --keep-monthly 6 --prune`, `Persistent=true`
- Monthly `MODE=check`: `restic check`, `Persistent=true`
- Nightly backup and daily restart timers use `Persistent=false`; a missed 04:30 run should not fire at noon
- Prune and check are deliberately infrequent: R2 bills ListObjects as Class A writes

Restore: `backup/restore.sh` restores a chosen snapshot into a fresh volume; the RUNBOOK drill ends by re-running `mc-recreate.sh` and `backup-recreate.sh` so every container re-resolves the recreated volume (a pre-created container holds its volume reference from creation time). `backup-recreate.sh` documents in its header that it must be re-run after any env or secret change (R2 keys, RCON password). The restore path is executed once for real during initial build, against a scratch volume, before the server goes live. A backup nobody has restored is not a backup.

## 10. Host bootstrap (`scripts/bootstrap.sh`)

Idempotent bash, safe to re-run, two-phase to avoid SSH lockout.

Phase 1 (default): non-root admin user, SSH key-only, password and root login disabled, `timedatectl set-timezone America/Bogota`, Podman, Cockpit plus cockpit-podman from noble-backports (verified: stock noble version predates Quadlet integration), Tailscale (interactive `tailscale up`), fail2ban, unattended-upgrades, timers and units installed from `containers/systemd/`, Quadlet units linked into `/etc/containers/systemd/` followed by `systemctl daemon-reload`, `.env` scaffolded.

Firewall, the part the review panel flagged as a blocker in the naive design: traffic to a Podman-published port is DNAT'ed and traverses the FORWARD chain, not INPUT, and OCI Ubuntu images ship a pre-seeded `FORWARD -j REJECT` persisted in `/etc/iptables/rules.v4`. Opening INPUT alone leaves the server unreachable despite every other step being correct, and only after a clean reboot, when rule restore ordering bites. Bootstrap therefore:

- Inserts the 25565 ACCEPT in INPUT ahead of Oracle's REJECT (host-level hygiene)
- Explicitly handles FORWARD: inserts, ahead of the pre-seeded REJECT, an ACCEPT for tcp dport 25565 toward the `mcnet` subnet plus a conntrack ESTABLISHED,RELATED accept
- Persists both via netfilter-persistent so the ordering survives the boot-time restore race
- Never flushes the filter table while containers run; any rule rewrite is followed by a container restart or `podman network reload`
- The first-run checklist ends with a post-reboot external connectivity test, because this failure mode only reproduces on a clean boot

Phase 2 (`--harden`, run only after Tailscale connectivity is confirmed): 25565/tcp stays open to the world; SSH and Cockpit (9090) accept only from the `tailscale0` interface, with a belt-and-braces match on the Tailscale CGNAT range 100.64.0.0/10; everything else drops. Documented prerequisite: disable Tailscale key expiry for this node in the admin console first. The default 180-day expiry plus a Tailscale-only SSH rule is a scheduled lockout, not a tail risk. The RUNBOOK gets a break-glass entry: recovery via the OCI console serial connection when Tailscale or the firewall is wedged.

Oracle console steps the script cannot do, listed in README: VCN Security List ingress rule for 25565/tcp from 0.0.0.0/0, reserving the static public IP. Nothing else is opened at the VCN; Tailscale rides the existing stateful egress.

## 11. Operations

- Daily restart 06:00 America/Bogota via timer: `scripts/restart-warn.sh` broadcasts in-game warnings at 5 minutes, 1 minute, and 10 seconds (via `podman exec mc rcon-cli say`), then `podman restart mc`. Skips the waits when `list` shows zero players.
- Chunky pre-generation: documented one-time run (console: `chunky radius 5000`, `chunky start`) before friends are invited; expect hours of high CPU, run it before enabling the uptime monitor to avoid alert noise.
- Uptime monitoring: UptimeRobot free tier, TCP check against `mc.hijuepapus.pro:25565`, alerting to Discord via webhook. Documented in README as liveness-only: a TCP accept proves the process listens, not that the tick loop is alive; playability monitoring is the bot's RCON-based job.
- Image updates are manual and deliberate: bump the pinned tag in the repo, pull, recreate. podman-auto-update is intentionally not used.

## 12. Documentation set

All docs in concise, direct prose, no em dashes (repo style rule, applies to every doc written in component 7).

- `README.md`: architecture diagram, verified pins table (image tags and digests), Oracle console manual steps (VCN rule, reserved IP), Cloudflare R2 card requirement, Hostinger DNS records, UptimeRobot liveness caveat, first-run walkthrough end to end including the post-reboot connectivity test.
- `RUNBOOK.md`: server will not start, OOM, TPS in the floor (Spark triage), world corruption plus restore drill (ending with both recreate scripts), Oracle reclaimed the instance (rebuild from bootstrap plus restore, DNS repoint), bot down, locked out of SSH (OCI serial console break-glass), cracked player bought the game (UUID migration).
- `MODS.md`: packwiz add/update/remove, backup-first rule (`scripts/backup-now.sh`), rollout procedure, how to re-add C2ME if ever wanted.

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
│   │   ├── bot.container          # ContainerName=bot
│   │   ├── socket-proxy.container # ContainerName=socket-proxy
│   │   └── mcnet.network          # NetworkName=mcnet
│   ├── systemd/          # mc.service (boot start), timers: backup, restart, forget, check
│   ├── mc-recreate.sh    # single source of truth for the mc container definition
│   └── backup-recreate.sh
├── bot/
│   ├── Cargo.toml
│   ├── Dockerfile        # cargo-chef, distroless cc, linux/arm64
│   └── src/              # main, config, commands/, rcon, docker, monitor
├── backup/
│   ├── Dockerfile        # alpine + restic + rcon-cli
│   ├── backup.sh         # MODE dispatch: backup | forget | check
│   └── restore.sh
├── pack/                 # packwiz: pack.toml, index.toml, mods/*.pw.toml
├── scripts/
│   ├── bootstrap.sh
│   ├── restart-warn.sh
│   └── backup-now.sh     # podman run --rm, MODE=backup, SNAPSHOT_TAG=pre-change
├── docs/superpowers/specs/
└── .github/workflows/images.yml   # bot + backup images -> GHCR, ubuntu-24.04-arm
```

## 14. Build order

Component by component, with a check-in after each:

1. Repo scaffold: `.gitignore`, `.env.example`, `containers/` (Quadlet units, recreate scripts, systemd units); socket-proxy tag and digest resolved and pinned
2. packwiz pack with the seven baseline mods, exact versions pinned
3. `bootstrap.sh`
4. Backup image, scripts, timers; restore drill executed against a scratch volume
5. Discord bot plus Dockerfile plus CI (largest component; crate versions re-verified against crates.io at write time)
6. Ops extras: restart-warn, Chunky doc, UptimeRobot doc
7. README, RUNBOOK, MODS written against what was actually built

Host validation gate, run once bootstrap has a live host (after component 3, before the bot in component 5), scripted as a checklist:

- curl through the proxy: list, inspect, start, stop against a scratch container
- bollard smoke test with API version negotiation against the proxy
- `.State.Health` present in inspect after a restart issued through the compat API
- post-reboot external connectivity test on 25565 (the FORWARD-chain failure only reproduces on clean boot)

## 15. Risks and mitigations

| Risk | Mitigation |
|---|---|
| EasyAuth mixed mode misbehaves with online-mode=true | Empirical test at first run with premium and cracked clients; fallback flip to ONLINE_MODE=FALSE documented |
| linuxserver/socket-proxy quirk against Podman compat API | Host validation gate after component 3, before the bot exists; tecnativa or direct-socket-with-bot-side-filtering as fallbacks |
| OCI FORWARD-chain rules silently block the published port after reboot | Bootstrap handles FORWARD explicitly and persists ordering; post-reboot connectivity test in the first-run checklist |
| Tailscale key expiry locks both admins out of SSH | Key expiry disabled as a documented Phase 2 prerequisite; OCI serial console break-glass in RUNBOOK |
| Silent backup failures | Timer runs attached so the unit fails visibly; bot alerts on nonzero mc-backup exits; monthly restic check |
| Oracle reclaims the instance | AUTOPAUSE off keeps baseline load; RUNBOOK rebuild drill; reserved IP and DNS make recovery a repoint; nightly off-instance backups bound data loss to 24h |
| R2 free tier exceeded (10 GB) | restic dedup plus retention policy keeps a small world well under; `restic stats` check documented in RUNBOOK |
| Quadlet/Podman version drift on Ubuntu LTS | Podman stays 4.9.x for the LTS lifetime (verified); all behaviors used are in 4.9 |
| Bot compromise | Blast radius limited to RCON commands and start/stop of two named containers; no raw socket, no host access, no inbound ports |
