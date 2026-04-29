# Incident Report — Fadi BSOD During DCP Wizard Install

**Date:** 2026-04-27
**Author:** Peter (with Claude)
**Severity:** None to DCP. Provider hardware issue.
**Status:** Closed — root cause identified, not DCP-related.

---

## TL;DR

Fadi (DESKTOP-HMS3278, RTX 3060 Ti / 8 GB VRAM) experienced a Blue Screen of Death during the DCP provider wizard install session at 17:13 UTC on 2026-04-27. The DCP daemon and installer did not cause the crash. Fadi's machine has **failing physical RAM**: Windows itself logged the bugcheck as `MEMORY_MANAGEMENT (0x0000001A)` and ran its built-in Memory Diagnostic, which removed bad memory regions earlier the same day. Fadi has crashed with the same bugcheck **at least three times today and twice in the prior two weeks — all before DCP was installed**.

---

## Timeline (2026-04-27, all times machine-local)

| Time | Event |
|---|---|
| 14:03 | BSOD #1 of the day. Bugcheck `0x0000001A`. (Pre-DCP install.) |
| 13:55 | BSOD #2 of the day. (Pre-DCP install.) |
| 15:23 | Windows Memory Diagnostic ran automatically and reported: **"Windows removed bad memory regions from this PC."** |
| ~16:00 | DCP wizard session begins. Wizard reaches "Downloading AI model" step. |
| ~16:30 | Ollama 1.85 GB installer downloads + runs successfully. |
| 17:13 | BSOD #3 of the day during model download. Same bugcheck `0x0000001A`. Auto-reboot. |
| 17:20 | Machine boots cleanly. DCP daemon manager re-launches. Wizard resumes the model download from where it left off. |

Prior BSODs on the same hardware (from `C:\Windows\Minidump\`):

- 4/14/2026 04:50 (2 weeks ago — pre-DCP)
- 4/25/2026 17:03 (2 days ago — pre-DCP)

---

## Root Cause

`MEMORY_MANAGEMENT` (`0x0000001A`) is a Windows kernel bugcheck raised when the memory manager detects a corrupted or unreadable physical memory page. Common causes, in order of likelihood:

1. **Failing physical RAM modules** — a bit error in DRAM that ECC cannot correct (consumer DDR4 has no ECC).
2. **Overclocked/unstable RAM timings** — XMP profiles that aren't stable on this specific module.
3. **A driver corrupting memory pages** — would normally show a different bugcheck (e.g. `BAD_POOL_HEADER`).

What rules out a driver/software cause on Fadi's box:

- **Windows itself diagnosed the issue.** The Memory Diagnostic event at 15:23 logged: *"Windows removed bad memory regions from this PC."* The OS does this when it detects repeatable read errors at specific physical addresses; it then maps those pages out of the available pool. This is hardware-level, not software-level.
- **The same bugcheck preceded DCP install.** Two BSODs today (13:55, 14:03) and at least two earlier this month happened with the identical bugcheck code — before any DCP binary was on the machine.
- **No DCP code touches kernel/driver state.** Audited:
  - The Tauri installer (`src-tauri/src/lib.rs`) has only two `unsafe` Rust blocks: a read-only DXGI GPU enumeration (`EnumAdapters1`, the standard Windows API every GPU app uses) and a string-lifetime cast. Neither can corrupt kernel memory.
  - The Python daemon (`dc1_daemon.py`) is pure HTTP. GPU info is read by spawning `nvidia-smi.exe` and parsing stdout; no `pynvml`, no `ctypes`, no driver loads.
  - The bundled Ollama installer (`OllamaSetup.exe`) does install a CUDA runtime, but had completed cleanly an hour before the BSOD; the crash happened during a model download, not during driver registration.

**Verdict: the BSOD was caused by Fadi's failing RAM, not by DCP.**

---

## Diagnostic Evidence

**Bugcheck event from Fadi's machine, 17:13:20 UTC (excerpted):**

```
Source:  Microsoft-Windows-WER-SystemErrorReporting
Event ID: 1001
Bugcheck code: 0x0000001A (0x0000000000061941, 0xFFFF570CB341980,
                            0x000000000000000B, 0xFFFFEE833EE27800)
Dump file: C:\Windows\MEMORY.DMP
```

**Memory Diagnostic event, 15:23:22 UTC (same day):**

```
Source:  Microsoft-Windows-Memory-Diagnostic-Task-Handler
Description: Windows removed bad memory regions from this PC.
```

**Minidump inventory on Fadi's machine post-incident:**

```
4/14/2026  4:50 AM    041426-7828-01.dmp     (964 KB)
4/25/2026  5:03 PM    042526-6468-01.dmp    (1.18 MB)
4/27/2026  1:55 PM    042726-6234-01.dmp    (1.29 MB)   ← pre-DCP
4/27/2026  2:03 PM    042726-6125-01.dmp     (940 KB)   ← pre-DCP
4/27/2026  5:13 PM    042726-6234-02.dmp     (908 KB)   ← during DCP session
```

5 unique BSODs in 2 weeks on an otherwise idle consumer desktop. That is a hardware diagnosis on its own.

---

## What DCP Did Well During the Crash

This is the part that matters for the demo narrative.

1. **Daemon survived an unannounced kill.** A BSOD is the harshest possible termination — no `SIGTERM`, no shutdown hook, no flush. Power yanked. On reboot, the daemon manager started cleanly with no manual intervention. Provider data on disk was not corrupted.
2. **No data loss.** The provider's API key, install token, and config persisted across the crash. Fadi did not have to re-onboard.
3. **Resumable install.** When the wizard relaunched, the Ollama install state was already on disk (it had completed before the crash); the wizard correctly skipped re-downloading and resumed at the model-download step. No double-download, no retry storm.
4. **No phantom heartbeats.** Once the daemon process died with the OS, Fadi's provider correctly disappeared from the marketplace — exactly what should happen when a provider is unreachable. (Compare to the orphan SSH-SQLite heartbeat loop incident from earlier today, which we killed because it was faking online status. The real daemon does the right thing.)
5. **Clean log story.** `startup.log` and `daemon.log` show the BSOD as a normal process exit followed by a normal restart. Nothing in our logs even looks broken — because nothing in our code broke.

For an investor question like *"what happens when a provider crashes?"* — this incident is the answer. We tested unintentionally on the worst-case path (kernel BSOD mid-job-flow on consumer hardware with failing RAM) and the system recovered without intervention.

---

## Recommendations for Fadi

1. Run `mdsched.exe` for a full overnight pass. If it reports errors, that's the smoking gun.
2. For a more thorough test, boot **MemTest86** from USB and run 8+ hours. Consumer RAM faults often only show up after sustained load.
3. If MemTest finds errors, replace the failing DIMM. RAM is cheap; BSODs cost hours.
4. Until the RAM is replaced, expect more crashes regardless of what software is installed.

---

## Recommendations for DCP

None blocking. Two opportunities surfaced:

1. **Wizard already supports resume after crash** — but it isn't documented anywhere user-facing. Worth adding a one-liner to the wizard FAQ: *"If your computer crashes during install, just relaunch DCP — it picks up where it left off."* That turns a scary moment into a confidence-building one.
2. **The bundled `OllamaSetup.exe` does run a CUDA driver registration step** that is technically a vector for driver/RAM interactions on bad hardware. Replacing it with `ollama-windows-amd64.zip` (a plain binary unzip, no driver install) is already on the deferred list as an out-of-scope follow-up. This incident slightly raises the priority of that change — not because Ollama caused the BSOD here (it didn't), but because removing any driver-touching code from our install path makes "DCP didn't BSOD me" easier to prove next time.

---

## Sign-off

Investigated by Peter + Claude on 2026-04-27. Closing without action items for DCP. Issue belongs entirely to Fadi's hardware.
