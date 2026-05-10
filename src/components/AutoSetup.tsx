import { useEffect, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  detectGpu,
  detectSystem,
  fullStartProvider,
  startDaemon,
  getEstimatedEarnings,
  startSigninLoopback,
  cancelSigninLoopback,
  sendSigninEmail,
  type GpuInfo,
  type SigninReceivedPayload,
  type WizardProgress,
} from "../lib/api";

/**
 * Auto first-run experience.
 *
 * Replaces the previous 5-screen wizard (welcome → hardware → account →
 * config → installing). After the user clicks "Allow All" once, the only
 * thing the user ever has to type is their email address, and only then
 * if no api_key is on disk yet.
 *
 *   1. "Allow All" — user grants the one permission gate
 *   2. Email + magic-link sign-in — single email input, then we listen on
 *      a local loopback HTTP server for {api_key, role} delivered by
 *      dcp.sa/auth/verify after the user clicks the link in their inbox.
 *   3. Detect hardware             — detect_gpu / detect_system
 *   4. Install engine              — full_start_provider step
 *   5. Download model              — full_start_provider step
 *   6. Bring up secure tunnel      — full_start_provider step (WG)
 *   7. Start daemon + heartbeat    — full_start_provider step
 *
 * Returning users (api_key already in ~/.dcp/config.json) skip the email
 * screen entirely and drop straight into the auto-checklist.
 */

type StepStatus = "pending" | "active" | "done" | "error";

interface SetupStep {
  id: string;
  label: string;
  detail?: string;
  status: StepStatus;
}

interface AutoSetupProps {
  onComplete: () => void;
  /**
   * If a previous install left a saved API key, the parent passes it in
   * so we never bug the user for it again.
   */
  initialApiKey?: string;
}

const STEP_TEMPLATE: SetupStep[] = [
  { id: "hardware", label: "Detect hardware", status: "pending" },
  { id: "account", label: "Connect account", status: "pending" },
  { id: "engine", label: "Install inference engine", status: "pending" },
  { id: "model", label: "Download AI model", status: "pending" },
  { id: "tunnel", label: "Bring up secure tunnel", status: "pending" },
  { id: "daemon", label: "Start provider daemon", status: "pending" },
];

// Map Rust wizard:progress step_id → frontend step id
const STEP_MAP: Record<string, string> = {
  ollama_download: "engine",
  ollama_install: "engine",
  model_download: "model",
  model_verify: "model",
  tunnel: "tunnel",
  daemon: "daemon",
};

// Sign-in screen states
type SigninPhase = "form" | "sending" | "waiting" | "received" | "timeout" | "error";

export function AutoSetup({ onComplete, initialApiKey }: AutoSetupProps) {
  const [steps, setSteps] = useState<SetupStep[]>(STEP_TEMPLATE);
  const [permissionsGranted, setPermissionsGranted] = useState(false);
  const [needApiKey, setNeedApiKey] = useState(!initialApiKey);
  const [error, setError] = useState<string | null>(null);
  const [errorStep, setErrorStep] = useState<string | null>(null);
  const [retryCounter, setRetryCounter] = useState(0);
  const [earnings, setEarnings] = useState<number | null>(null);
  const [gpu, setGpu] = useState<GpuInfo | null>(null);
  const [complete, setComplete] = useState(false);

  // Sign-in form state
  const [email, setEmail] = useState("");
  const [signinPhase, setSigninPhase] = useState<SigninPhase>("form");
  const [signinError, setSigninError] = useState("");

  // Stable ref for API key once provided
  const apiKeyRef = useRef<string>(initialApiKey ?? "");

  // ── Helpers ─────────────────────────────────────────────────────────
  function setStep(id: string, patch: Partial<SetupStep>) {
    setSteps((prev) =>
      prev.map((s) => (s.id === id ? { ...s, ...patch } : s))
    );
  }

  function fail(id: string, msg: string) {
    setStep(id, { status: "error", detail: msg });
    setErrorStep(id);
    setError(msg);
  }

  // ── Permission grant ────────────────────────────────────────────────
  async function handleAllowAll() {
    // Future: request OS-level permissions here (notifications, autostart,
    // accessibility on macOS). For now this is the user's affirmative
    // consent to let us run everything that follows.
    setPermissionsGranted(true);
  }

  // ── Embedded magic-link sign-in ─────────────────────────────────────
  //
  // Subscribes to `signin:received`, `signin:timeout`, `signin:cancelled`
  // events from Rust. Mounts when the user is on the email screen and
  // tears down on unmount or after we get an api_key.
  useEffect(() => {
    if (!permissionsGranted) return;
    if (!needApiKey) return;

    let unlistenReceived: UnlistenFn | undefined;
    let unlistenTimeout: UnlistenFn | undefined;

    listen<SigninReceivedPayload>("signin:received", (event) => {
      const p = event.payload;
      if (!p || !p.api_key) return;
      apiKeyRef.current = p.api_key;
      setSigninPhase("received");
      // Tiny delay so the "Got it" tick is visible before transitioning.
      setTimeout(() => {
        setNeedApiKey(false);
      }, 600);
    }).then((fn) => {
      unlistenReceived = fn;
    });

    listen("signin:timeout", () => {
      setSigninPhase("timeout");
    }).then((fn) => {
      unlistenTimeout = fn;
    });

    return () => {
      unlistenReceived?.();
      unlistenTimeout?.();
      // Best-effort: if user navigates away, kill the listener.
      cancelSigninLoopback().catch(() => { /* ignore */ });
    };
  }, [permissionsGranted, needApiKey]);

  async function handleEmailSubmit() {
    setSigninError("");
    const e = email.trim();
    if (!e || !/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(e)) {
      setSigninError("Please enter a valid email address.");
      return;
    }
    setSigninPhase("sending");
    try {
      const cb = await startSigninLoopback();
      await sendSigninEmail(e, "provider", cb.callback_url);
      setSigninPhase("waiting");
    } catch (err) {
      setSigninError(String(err));
      setSigninPhase("error");
      // Tear down any half-started listener so a retry gets a clean port.
      cancelSigninLoopback().catch(() => { /* ignore */ });
    }
  }

  function handleResendEmail() {
    setSigninPhase("form");
    setSigninError("");
  }

  // ── Main install loop ───────────────────────────────────────────────
  useEffect(() => {
    if (!permissionsGranted) return;
    if (needApiKey) return;
    if (complete) return;

    let cancelled = false;
    let unlisten: UnlistenFn | undefined;

    // Subscribe to wizard:progress events from Rust
    listen<WizardProgress>("wizard:progress", (event) => {
      const p = event.payload;
      const sid = STEP_MAP[p.step_id];
      if (!sid) return;
      let detail = p.detail ?? "";
      if (p.pct != null) {
        const mb =
          p.mb_done != null && p.mb_total != null
            ? ` ${p.mb_done.toFixed(0)}/${p.mb_total.toFixed(0)} MB`
            : "";
        const mbps = p.mbps != null ? ` @ ${p.mbps.toFixed(1)} Mbps` : "";
        detail = `${p.pct.toFixed(0)}%${mb}${mbps}`;
      }
      const status: StepStatus =
        p.status === "error" ? "error" : p.status === "done" ? "done" : "active";
      setStep(sid, { status, detail });
      if (status === "error") {
        setErrorStep(sid);
        setError(p.error ?? detail);
      }
    }).then((fn) => {
      unlisten = fn;
    });

    async function run() {
      // Reset all to pending on retry
      setSteps(STEP_TEMPLATE.map((s) => ({ ...s })));
      setError(null);
      setErrorStep(null);

      // Step 1: hardware
      setStep("hardware", { status: "active" });
      try {
        const [g, _s] = await Promise.all([detectGpu(), detectSystem()]);
        if (cancelled) return;
        setGpu(g);
        const vramGb = Math.round((g.vram_mb ?? 0) / 1024);
        setStep("hardware", {
          status: "done",
          detail: `${g.name} • ${vramGb} GB`,
        });
      } catch (e) {
        fail("hardware", `Hardware detection failed: ${e}`);
        return;
      }

      // Step 2: account (already validated above before we got here)
      setStep("account", {
        status: "done",
        detail: "Signed in",
      });

      // Step 3-6: chained start. Rust emits wizard:progress events for the
      // engine / model / tunnel / daemon steps; the listener above flips
      // their statuses.
      setStep("engine", { status: "active", detail: "Starting…" });
      try {
        await fullStartProvider(apiKeyRef.current);
        if (cancelled) return;
      } catch (e) {
        const msg = String(e);
        // Best-effort attribution of the error to a step
        const target =
          /MLX|Ollama|engine|install/i.test(msg)
            ? "engine"
            : /model|pull|download/i.test(msg)
              ? "model"
              : /tunnel|wireguard|wg/i.test(msg)
                ? "tunnel"
                : /daemon|spawn|heartbeat/i.test(msg)
                  ? "daemon"
                  : "engine";
        fail(target, msg);
        return;
      }

      // Force any straggler steps to done — Rust succeeded overall
      setSteps((prev) =>
        prev.map((s) =>
          s.status === "pending" || s.status === "active"
            ? { ...s, status: "done" as StepStatus }
            : s
        )
      );

      // Persist daemon config with safe defaults (no UI for it anymore —
      // the user can tweak later from Settings).
      try {
        await startDaemon(apiKeyRef.current, {
          run_mode: "idle",
          gpu_usage_cap: 80,
          temp_limit: 85,
          start_on_boot: true,
        });
      } catch {
        // Non-fatal — config save failure doesn't break the running provider.
      }

      // Earnings estimate
      if (gpu) {
        try {
          const e = await getEstimatedEarnings(
            gpu.vram_mb,
            gpu.is_apple_silicon
          );
          setEarnings(e);
        } catch {
          setEarnings(gpu.is_apple_silicon ? 35 : 50);
        }
      }

      setComplete(true);
    }

    run().catch((err) => {
      if (!cancelled) setError(String(err));
    });

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [permissionsGranted, needApiKey, retryCounter]);

  // ── Render ──────────────────────────────────────────────────────────

  if (!permissionsGranted) {
    return (
      <div className="step-container">
        <div className="welcome-logo">DCP &infin;</div>
        <h1 className="welcome-title">Set up your node</h1>
        <p className="welcome-subtitle">
          DCP needs permission to install an inference engine, set up a secure
          mesh tunnel, and run a background daemon. After this one click,
          everything else is automatic.
        </p>
        <ul className="auto-setup-permissions">
          <li>Install Ollama (or MLX on Apple Silicon)</li>
          <li>Configure a WireGuard tunnel (admin password required once)</li>
          <li>Start the DCP daemon at login</li>
        </ul>
        <button
          className="btn btn-primary btn-large"
          onClick={handleAllowAll}
          aria-label="Allow setup to proceed"
        >
          Allow All &amp; Continue
        </button>
      </div>
    );
  }

  if (needApiKey) {
    if (signinPhase === "waiting") {
      return (
        <div className="step-container">
          <h2 className="step-title">📧 Check your email</h2>
          <p className="step-subtitle">
            We sent a sign-in link to <strong>{email}</strong>. Click the
            button in the email — this window will continue automatically
            when you do.
          </p>
          <div className="install-steps mt-16">
            <div className="install-step active">
              <div className="install-step-icon">
                <span className="spinner" />
              </div>
              <span className="install-step-label">
                Waiting for you to click the link…
              </span>
            </div>
          </div>
          <p className="text-sm text-muted mt-16 text-center">
            The link expires in 15 minutes. Check your spam folder if you
            don't see it within a minute.
          </p>
          <div className="step-actions mt-16">
            <button
              className="btn btn-secondary"
              onClick={() => {
                cancelSigninLoopback().catch(() => { /* ignore */ });
                setSigninPhase("form");
              }}
            >
              Use a different email
            </button>
          </div>
        </div>
      );
    }

    if (signinPhase === "received") {
      return (
        <div className="step-container">
          <div className="completion-card">
            <div className="completion-icon">&#10003;</div>
            <div className="completion-title">Signed in</div>
            <div className="completion-subtitle">
              Continuing setup…
            </div>
          </div>
        </div>
      );
    }

    if (signinPhase === "timeout") {
      return (
        <div className="step-container">
          <h2 className="step-title">Link expired</h2>
          <p className="step-subtitle">
            The sign-in link wasn't used within 5 minutes. Send a new one?
          </p>
          <div className="step-actions mt-16">
            <button className="btn btn-primary" onClick={handleResendEmail}>
              Send a new link
            </button>
          </div>
        </div>
      );
    }

    // signinPhase === "form" | "sending" | "error"
    const sending = signinPhase === "sending";
    return (
      <div className="step-container">
        <h2 className="step-title">Sign in to DCP</h2>
        <p className="step-subtitle">
          Enter your email to sign in or create your DCP account. We'll send
          you a one-click sign-in link — no password needed.
        </p>
        <input
          type="email"
          inputMode="email"
          autoComplete="email"
          className="input-field"
          placeholder="you@example.com"
          value={email}
          onChange={(e) => {
            setEmail(e.target.value);
            setSigninError("");
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !sending) handleEmailSubmit();
          }}
          autoFocus
          aria-label="Email address"
          disabled={sending}
        />
        {signinError && <div className="input-error mt-8">{signinError}</div>}
        <div className="step-actions">
          <button
            className="btn btn-primary"
            onClick={handleEmailSubmit}
            disabled={sending || email.length < 3}
          >
            {sending ? "Sending…" : "Send sign-in link"}
          </button>
        </div>
        <p className="text-sm text-muted mt-16 text-center">
          By continuing you agree to the DCP{" "}
          <a href="https://dcp.sa/terms" target="_blank" rel="noreferrer">
            Terms
          </a>
          {" "}and{" "}
          <a href="https://dcp.sa/privacy" target="_blank" rel="noreferrer">
            Privacy Policy
          </a>
          .
        </p>
      </div>
    );
  }

  if (complete) {
    return (
      <div className="step-container">
        <div className="completion-card">
          <div className="completion-icon">&#10003;</div>
          <div className="completion-title">You're earning!</div>
          {earnings != null && (
            <div className="completion-earnings">~${earnings}/month</div>
          )}
          <div className="completion-subtitle">
            Estimated based on your hardware and current network demand.
          </div>
        </div>
        <p className="text-sm text-muted mt-16 text-center">
          DCP is running in the background. Check the menu bar icon for status.
        </p>
        <div className="step-actions mt-16">
          <button className="btn btn-primary btn-large" onClick={onComplete}>
            Open Dashboard
          </button>
        </div>
      </div>
    );
  }

  const doneCount = steps.filter((s) => s.status === "done").length;
  const progress = Math.round((doneCount / steps.length) * 100);

  return (
    <div className="step-container">
      <h2 className="step-title">Setting up your node</h2>
      <p className="step-subtitle">
        This usually takes 2–5 minutes (most of that is the model download).
      </p>

      <div className="install-steps">
        {steps.map((s) => (
          <div
            key={s.id}
            className={`install-step ${s.status === "active" ? "active" : ""} ${s.status === "done" ? "done" : ""}`}
          >
            <div className="install-step-icon">
              {s.status === "done" && (
                <span className="checkmark" aria-label="complete">
                  &#10003;
                </span>
              )}
              {s.status === "active" && <span className="spinner" />}
              {s.status === "pending" && <span className="pending" />}
              {s.status === "error" && (
                <span className="error-icon" aria-label="error">
                  &#10007;
                </span>
              )}
            </div>
            <span className="install-step-label">
              {s.label}
              {s.detail && s.status !== "pending" && <> &mdash; {s.detail}</>}
            </span>
          </div>
        ))}
      </div>

      <div className="progress-bar-container">
        <div className="progress-bar-track">
          <div
            className="progress-bar-fill"
            style={{ width: `${progress}%` }}
            role="progressbar"
            aria-valuenow={progress}
            aria-valuemin={0}
            aria-valuemax={100}
          />
        </div>
      </div>

      {error && (
        <div className="input-error mt-12">
          <p>
            <strong>{steps.find((s) => s.id === errorStep)?.label ?? "Setup"}</strong>:{" "}
            {error.split("\n")[0].slice(0, 200)}
          </p>
          <div className="step-actions mt-12">
            <button
              className="btn btn-secondary"
              onClick={() => setRetryCounter((n) => n + 1)}
            >
              Retry
            </button>
            <a
              className="btn"
              href="https://dcp.sa/help"
              target="_blank"
              rel="noreferrer"
            >
              Ask the agent
            </a>
          </div>
        </div>
      )}
    </div>
  );
}
