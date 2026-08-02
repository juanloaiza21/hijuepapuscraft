# Managing the mod pack

The pack lives in `pack/` (packwiz format: `pack/pack.toml`, `pack/index.toml`, `pack/mods/*.pw.toml`). The `mc` container fetches it from `PACKWIZ_URL` on every start, so a repo change only takes effect after a restart.

## Backup-first rule

Before touching the pack, always take a manual pre-change snapshot and wait for it to finish successfully:

```bash
sudo /opt/hijuepapuscraft/scripts/backup-now.sh
```

This is mandatory, not a suggestion. A bad packwiz change plus a crash-looping server should never coincide with the only recent backup being from before the change.

## Add, update, remove

Add a mod (from Modrinth):

```bash
cd pack && packwiz modrinth add <slug>
```

Update a mod, or everything:

```bash
cd pack && packwiz update <mod>
# or
packwiz update --all
```

Remove a mod:

```bash
cd pack && packwiz remove <mod>
```

## Always, after any add, update, or remove

```bash
packwiz refresh
git add pack && git commit -m "..."
git push
podman restart mc
```

`packwiz refresh` keeps `pack/index.toml`'s hashes in sync with the mod files, so it has to run before committing. The `mc` container refetches the pack from `PACKWIZ_URL` on start, so `podman restart mc` is enough to apply the change; there is no need to run `containers/mc-recreate.sh` for a mod-only change.

## Re-adding C2ME

C2ME is deliberately left out of the baseline pack. Its parallel chunk generation pays off at 4 or more cores; on this instance's 2 OCPU it would be the pack's most crash-prone mod for marginal gain, and Chunky pregeneration already removes most ongoing chunk-generation load. Re-add it only if chunk loading measurably lags after a Chunky pregen has already run:

```bash
cd pack && packwiz modrinth add c2me-fabric
```

Then follow the add/update/remove sequence above (refresh, commit, push, restart).

If the server starts crash-looping after adding it, remove it first, then investigate separately:

```bash
cd pack && packwiz remove c2me-fabric
packwiz refresh
git add pack && git commit -m "Remove c2me-fabric: crash-looping"
git push
podman restart mc
```

## Rollback

If a pack change breaks the server and the specific mod is not obvious, roll back the whole pack commit instead of hunting for the cause under pressure:

```bash
git revert <pack-commit-sha>
git push
podman restart mc
```
