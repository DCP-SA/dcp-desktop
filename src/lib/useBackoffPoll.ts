import { useEffect, useRef } from "react";

/**
 * useBackoffPoll — drop-in replacement for `setInterval(poll, baseMs)` that
 * doubles the delay on consecutive failures (capped) and resets to base on
 * success.
 *
 * Audit 2026-05-14 F13 — the Dashboard runs 8 concurrent intervals (5s, 10s,
 * 30s, 60s …) with no backoff. When `api.dcp.sa` is unreachable each loop
 * keeps firing at base rate → ~50 req/min from every provider during an
 * incident. This hook gives every call site exponential backoff for free.
 *
 * Usage:
 *   useBackoffPoll(async () => {
 *     const m = await getLiveMetrics();
 *     setMetrics(m);
 *   }, 5000);
 *
 * The poll fn must throw on failure for backoff to kick in.
 */
export function useBackoffPoll(
  poll: () => Promise<unknown>,
  baseMs: number,
  opts?: { maxMs?: number; deps?: unknown[] }
) {
  const maxMs = opts?.maxMs ?? 5 * 60 * 1000; // cap at 5 minutes
  const cancelled = useRef(false);

  useEffect(() => {
    cancelled.current = false;
    let delay = baseMs;
    let timer: ReturnType<typeof setTimeout> | null = null;

    const tick = async () => {
      if (cancelled.current) return;
      try {
        await poll();
        delay = baseMs; // success → reset
      } catch {
        delay = Math.min(delay * 2, maxMs); // failure → back off
      }
      if (cancelled.current) return;
      timer = setTimeout(tick, delay);
    };

    // Fire once immediately, then schedule.
    tick();

    return () => {
      cancelled.current = true;
      if (timer) clearTimeout(timer);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, opts?.deps ?? []);
}
