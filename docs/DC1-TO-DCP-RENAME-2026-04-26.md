# DC1 → DCP rename cutover

**Date:** 2026-04-26
**Authorized by:** Peter, "do this now and done with the new installs"
**Branch (desktop):** `peter/48h-stability-roadmap` → fold into 0.2.2
**Blast radius today:** 1 active provider (Fadi, currently offline). Lowest-cost window we will ever get.

## Why now

DC1 is the deprecated brand. Every public surface (`dcp.sa`, `api.dcp.sa`, store bundle id `sa.dcp.provider`, daemon name `DCP Provider`, `providers.db`) is already DCP. What remains is filesystem paths, repo dirs, the PM2 process name, and a handful of code constants. If we don't rename now, every future installer ships the legacy name forever.

## Inventory

### Renames

| Layer | Before | After |
|---|---|---|
| Local parent dir | `/Users/pp/DC1-Platform/` | `/Users/pp/DCP/` |
| Backend repo dir | `dc1-platform/` | `dcp-platform/` |
| VPS path | `/root/dc1-platform/` | `/root/dcp-platform/` |
| Provider home | `~/dc1-provider/` | `~/dcp-provider/` |
| PM2 process | `dc1-provider-onboarding` | `dcp-onboarding` |
| Daemon constants | `LOG_DIR`, `CONFIG_DIR`, `POWER_COST_CONFIG_FILE` (lines 151/178/180) | same names, `dcp-provider` paths |
| Tauri tray log path | `lib.rs:3045` `.join("dc1-provider")` | `.join("dcp-provider")` |
| Daemon docstrings | line 17, 186, 2558, 6398 | rephrase to dcp-provider |

### Stays unchanged (already DCP)

- `dcp.sa`, `api.dcp.sa`, `dcp-desktop` repo
- `sa.dcp.provider` (Tauri bundle id)
- `DCP Provider` (display name)
- `providers.db`, `dcp-provider-...` API key prefix
- daemon python module name `dcp_daemon.py`
- env vars `DCP_*`

## Phase order

### Phase A — code path strings (zero downtime, fully reversible)

**Scope:** desktop repo + daemon source. No VPS or filesystem touched yet.

1. Daemon: change `dc1-provider` → `dcp-provider` in 5 path constants + 4 docstrings.
2. Daemon: add migration block at startup — if `~/dc1-provider/` exists and `~/dcp-provider/` does not, `os.rename()` the directory. One-time, idempotent, logged.
3. Tauri `lib.rs:3045`: `dc1-provider` → `dcp-provider`. Tray "View Logs" works on old and new installs because daemon migration runs first.
4. Bump daemon to 4.2.0, desktop to 0.2.2.
5. `cargo check`, commit, push branch. **No deploy yet.**

### Phase B — daemon 4.2.0 release

**Scope:** SCP new daemon, hash-verify, hot-reload via existing endpoint.

1. SCP `dcp_daemon.py` to `/root/dc1-platform/backend/installers/` (path still old at this point — that's fine, file just lives there).
2. md5 verify.
3. Confirm `/api/providers/download/daemon` serves 4.2.0.
4. Test: when Fadi reinstalls, his old `~/dc1-provider/` gets renamed to `~/dcp-provider/` on first run. No data loss.

### Phase C — VPS rename

**Scope:** ~30s onboarding-API downtime. No active providers affected (Fadi is offline; daemon→backend traffic is to `/api/...` which routes via nginx independent of repo path).

```bash
# on VPS
pm2 stop dc1-provider-onboarding
mv /root/dc1-platform /root/dcp-platform
# edit /root/dcp-platform/ecosystem.config.js: name + cwd → dcp-onboarding, /root/dcp-platform
pm2 delete dc1-provider-onboarding
pm2 start /root/dcp-platform/ecosystem.config.js
pm2 save
```

Sanity:
- `curl https://api.dcp.sa/api/providers/download/daemon` returns 4.2.0
- `pm2 jlist | jq '.[] | .name'` shows `dcp-onboarding`
- nginx unchanged (proxies to `localhost:8083`, agnostic to filesystem path)

### Phase D — local working dir rename

**Scope:** my own working directory. Last because it disrupts this Claude Code session.

1. Commit + push everything on `peter/48h-stability-roadmap` first.
2. Close current session.
3. `mv /Users/pp/DC1-Platform /Users/pp/DCP` and `mv /Users/pp/DCP/dc1-platform /Users/pp/DCP/dcp-platform`.
4. Restart Claude Code from new path. Memory index pointers (`/Users/pp/.claude/projects/-Users-pp-DC1-Platform/`) keep working — they refer to the project hash, not the literal path; if breakage shows up, that's a separate fix.

## What about the 607 hits in backend repo?

Mostly: docs (`.md`/`.docx`), build artifacts (logs, old installers), and historical scripts. Strategy:

- **Code paths and live config** (ecosystem.config.js, nginx confs that reference `/root/dc1-platform`, daemon constants): rename in Phase A/C.
- **Documentation and historical reports**: leave alone. They're history — renaming them rewrites the past.
- **Build artifacts, generated logs**: deleted/regenerated naturally.

## Integration with 48h roadmap

This rename ships in the same 0.2.2 release accumulating G55, H9, L2, L4, M7, G2, M11, M6, H8, M3 partial. The G55 commit already changed the tray log path; Phase A updates it to `dcp-provider` as a one-line follow-up.

## Rollback

- Phase A: `git revert` the rename commit. No filesystem state changed.
- Phase B: redeploy daemon 4.1.2 (still in our git history); migration is one-way but `os.rename` of an empty `~/dc1-provider/` to `~/dcp-provider/` is harmless.
- Phase C: reverse the `mv` and ecosystem edit. ~30s.
- Phase D: reverse the local `mv`.

## Approval gate

Before executing, Peter approves:
- [ ] Phase A code changes
- [ ] Phase B daemon deploy (touches VPS file)
- [ ] Phase C VPS rename (30s downtime)
- [ ] Phase D local working dir rename (disrupts this session)

Recommendation: A and B together (zero downtime, immediate value), C in a quiet window today, D at end-of-session.
