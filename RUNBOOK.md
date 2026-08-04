# Runbook

Operational drills for HijuepapusCraft. Each section: symptoms, diagnosis, fix.

## Server will not start

**Symptoms:** `mc.service` fails, or `podman ps` shows `mc` restarting or exited; players cannot connect.

**Diagnosis:**

```bash
systemctl status mc.service
podman logs --tail 100 mc
```

**Fix, by cause:**

- Bad `.env` (missing or malformed `MEMORY`, `RCON_PASSWORD`, `DIFFICULTY`, or an image tag). Fix the value in `/opt/hijuepapuscraft/.env`, then `containers/mc-recreate.sh` and `systemctl restart mc.service`.
- `PACKWIZ_URL` unreachable (repo went private, GitHub outage, or the pack file moved). The logs show a failed pack fetch. Confirm `curl https://raw.githubusercontent.com/juanloaiza21/hijuepapuscraft/main/pack/pack.toml` returns content.
- EULA not accepted: `containers/mc-recreate.sh` sets `EULA=TRUE` unconditionally, so this only happens after a hand-edited container definition. Confirm with `podman inspect mc`.

## Container wedged in stopping / running with a dead conmon

**Symptoms:** `podman inspect mc` shows `State.Status=stopping` with a pid and exitcode 0 and no OOM; or `Status=running` while the recorded ConmonPid is dead. The bot says "trabada" or "Yace dormida". `/start` replies with podman's "must be in Created or Stopped state". `/stop` reports success but changes nothing. `journalctl -u mc-restart.service` shows `container restart` then `container stop` with no `died` line.

**Diagnosis:**

```bash
podman inspect mc --format '{{.State.Status}} conmon={{.State.ConmonPid}}'
kill -0 <conmonpid>
cat /proc/<conmonpid>/cgroup
```

The cgroup output must contain `libpod-conmon-`. If the conmon process is dead, this is the wedged-with-orphan case.

**Expected recovery:** `mc-watchdog.timer` detects this within 5-10 minutes, repairs the container automatically, and posts alerts to Discord at both ends.

**Manual repair, simple (destroys the container object):**

```bash
podman rm -f mc
containers/mc-recreate.sh
podman start mc
```

**Manual repair, advanced (preserves the container, skips re-init):** only if you need to keep the container object itself unchanged and cannot afford the 10-second re-init.

```bash
# Destroy the payload, forcing cleanup of the resource files.
crun kill --all <container-id> KILL
crun delete --force <container-id>

# Write exit code 0 to the exit file so libpod's checkExitFile unblocks the state machine.
# This is a bare integer with NO newline.
printf 0 > /run/libpod/exits/<container-id>

# Now podman can move the state to stopped and start again.
podman start mc
```

**Note:** podman 5.1.0+ includes an upstream fix for this failure mode. Ubuntu 24.04 ships podman 4.9.3 and will not receive the fix.

## Port 25565 dead after reboot

**Symptoms:** the server is up (`mc` is running, healthy, players can join over `podman exec mc rcon-cli list` locally), but external connections fail, and it worked before the last reboot.

**Diagnosis:**

```bash
iptables -t nat -S | grep -i netavark
podman inspect -f '{{.NetworkSettings.Networks.mcnet.IPAddress}}' mc
```

If the DNAT rule's destination IP does not match the IP `podman inspect` just printed, a stale netavark DNAT rule from before the reboot is shadowing the fresh one for the container's current address. This happens if `/etc/iptables/rules.v4` was ever persisted with a plain `netfilter-persistent save`, which captures netavark's live rules for whatever IP the container had at persist time.

**Fix:**

1. Sanity check the persisted rules do not carry a netavark/aardvark rule: `grep -viE 'netavark|aardvark' /etc/iptables/rules.v4` should show the same content as the full file; if it does not, a stale netavark rule is in there.
2. Re-run the curated persist via bootstrap so `/etc/iptables/rules.v4`/`rules.v6` hold only non-container rules again: `sudo scripts/bootstrap.sh` (phase 1 is idempotent and calls `persist_rules`).
3. `systemctl restart mc.service` so netavark re-issues the DNAT rule for the container's current IP.

## OOM

**Symptoms:** the `mc` container exits unexpectedly, the host feels sluggish, or players report lag right before a crash.

**Diagnosis:**

```bash
podman inspect mc | grep -i oom
free -h
```

**Fix:**

- If `OOMKilled` is true, the JVM heap (`MEMORY` in `.env`, currently `6G`) plus host overhead exceeded what the 12 GB instance has free. Lower `MEMORY` in `.env`, then `containers/mc-recreate.sh`.
- If the OOM lines up with the nightly backup window (04:30 America/Bogota, `mc-backup.timer`), the restic snapshot is competing with the JVM for memory. Check `journalctl -u mc-backup.service` for the timing and consider spacing the backup and restart timers further apart.

## TPS in the floor

**Symptoms:** players report rubber-banding or delayed actions.

**Diagnosis:** Start with `/status` in Discord (players, TPS at 1m/5m/15m, memory). If TPS confirms the problem, profile it:

```bash
podman exec mc rcon-cli spark profiler start
# let it run for 60 seconds
podman exec mc rcon-cli spark profiler stop
```

Read the URL spark prints on stop; it hosts an interactive flame graph of what is eating the tick.

**Fix:** depends on what the profile shows. Common culprits: too many entities or redstone in one chunk, a mod doing synchronous I/O, or chunk generation load (should be near zero after the Chunky pregen described in the README). There is no generic fix beyond what the profiler points at.

## World corruption and restore drill

**Symptoms:** the server crash-loops on a specific chunk, `mc` logs show corrupted region file errors, or a player reports lost builds beyond normal griefing.

**Fix:** run `backup/restore.sh` with a snapshot id (or `latest`):

```bash
backup/restore.sh <snapshot-id|latest>
```

This restores the chosen restic snapshot into a fresh volume, `mc-data-restore`, and prints the swap-in steps. Follow them exactly as printed:

```bash
podman rm -f mc mc-backup
podman volume rm mc-data
podman volume create mc-data
podman run --rm -v mc-data-restore:/from:ro -v mc-data:/to docker.io/alpine:3.22 sh -c 'cp -a /from/data/. /to/'
/opt/hijuepapuscraft/containers/mc-recreate.sh
/opt/hijuepapuscraft/containers/backup-recreate.sh
systemctl restart mc.service
```

Both recreate scripts have to run at the end. A pre-created container keeps its volume reference from creation time, so `mc` and `mc-backup` need to be rebuilt against the restored `mc-data` volume, not just started. Note: `mc.service` now has an `ExecStop` that gracefully stops the server with the full 120-second timeout, so the step `systemctl restart mc.service` waits for the server to exit before restarting.

## Oracle reclaimed or killed the instance

**Symptoms:** the instance is unreachable; SSH and the game port are both dead; the OCI console shows the instance terminated or stopped.

**Fix:**

Follows the same order as the README first run walkthrough; this drill only calls out where a rebuild differs from a from-scratch setup.

1. Launch a new `VM.Standard.A1.Flex` instance (README, Oracle Cloud host section).
2. Run `sudo SSH_KEY="$(cat ~/.ssh/id_ed25519.pub)" scripts/bootstrap.sh` (phase 1), then `tailscale up`, disable key expiry for the node.
3. Edit `/opt/hijuepapuscraft/.env` with the real values, then `sudo /opt/hijuepapuscraft/scripts/gen-scoped-env.sh` to regenerate `.env.bot`/`.env.backup`. The restic repository already exists in R2, so skip `restic init`.
4. `sudo systemctl start mcnet-network.service socket-proxy.service`.
5. Restore the world from R2: `backup/restore.sh latest` (or a specific snapshot id from `restic snapshots`), following the swap-in steps in the restore drill above. Its last step restarts `mc.service` (`systemctl restart`, not `start`: with `Type=oneshot`/`RemainAfterExit=yes`, `start` on a unit systemd still considers active is a no-op).
6. Run the Host validation gate checklist (README), then `sudo systemctl start bot.service`.
7. `sudo scripts/bootstrap.sh --harden` (phase 2) once Tailscale access is confirmed.
8. Repoint the Hostinger `mc` A record to the new reserved IP via the `hostinger` CLI:

   ```bash
   hostinger dns records list hijuepapus.pro
   ```

   then update the A record with the CLI's update command or the panel.
9. Data loss is bounded by the last nightly snapshot (04:30 America/Bogota). Anything played after that snapshot and before the reclaim is gone.

## Locked out of SSH

**Symptoms:** SSH connection refused or times out, both over the public IP and over Tailscale.

**Fix:**

1. Break-glass access via the OCI console serial connection: Compute > Instance > Console Connection in the OCI console. This does not depend on the network stack at all.
2. From the serial console, diagnose: `iptables -L` for a bad rule, or `tailscale status` if the tailnet itself is the problem.
3. Fix iptables directly, or run `tailscale up` again if the node fell off the tailnet (expired key, service restart).

## Bot down

**Symptoms:** slash commands stop responding, no status updates appear in the notify channel.

**Diagnosis:**

```bash
systemctl status bot.service
podman logs bot
```

**Fix:** the server itself is unaffected; `mc` runs independently of `bot`, so players stay online. Restart with `systemctl restart bot.service`. If it crash-loops, check `.env` for a stale `DISCORD_TOKEN` or an unreachable `DOCKER_API_URL`. `bot.service` reads the scoped `.env.bot`, not `.env` directly, so after fixing the value in `.env` run `scripts/gen-scoped-env.sh` (auto-generated, not hand-edited) before restarting the service. Note: `mc.service` now has an `ExecStop` that gracefully stops the server with the full 120-second timeout, so `systemctl stop mc.service` really stops the server and does not return until it has exited or timed out.

## Cracked player bought the game

**Symptoms:** a player who previously joined with a cracked client now has a legitimate Microsoft account; EasyAuth or the whitelist treats them as a brand new player.

**Fix:** their offline-mode UUID changes to their premium UUID. Migrate manually:

1. Get both UUIDs from `usercache.json` in the server's data directory (`/data` inside the `mc` container, next to `server.properties`): the old offline entry, and the new premium entry after they have attempted to join once.
2. Copy the playerdata file under the world's `playerdata/` directory from the old offline UUID's `.dat` filename to the new premium UUID's filename.
3. Update the whitelist entry: `/whitelist remove <name>` then `/whitelist add <name>` in Discord, or `podman exec mc rcon-cli whitelist add <name>` directly. EasyWhitelist keeps the whitelist name-based, so this survives the UUID change on its own; the manual step is only the playerdata copy.

## Backup failure alert

**Symptoms:** the bot posts a `Backup FAILED` alert in the notify channel, or `mc-backup.service` shows failed.

**Diagnosis:**

```bash
journalctl -u mc-backup.service
podman logs mc-backup
```

**Fix, common causes:**

- R2 credentials rotated: update `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, and `RESTIC_REPOSITORY` (if the account changed) in `.env`, then run `containers/backup-recreate.sh` to pick them up. The container freezes its env at creation time, so editing `.env` alone is not enough; `backup-recreate.sh` also re-runs `scripts/gen-scoped-env.sh` so `.env.backup` (which the nightly timer, `restic-forget.timer`, and `restic-check.timer` all read) picks up the rotated values too.
- R2 free tier exceeded: check usage in the Cloudflare dashboard. `restic-forget.timer` prunes old snapshots weekly, but a burst of large snapshots can still exceed the 10 GB free tier before the next prune.
- RCON password drift after a recreate: if `RCON_PASSWORD` changed in `.env` but `mc-backup` was not recreated afterward, the backup's RCON calls fail. The backup itself still completes as a cold backup, but `save-off`/`save-on` will not run around it. Run `containers/backup-recreate.sh`.
