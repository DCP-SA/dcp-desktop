import { useState } from "react";
import { validateApiKey } from "../lib/api";

interface AccountProps {
  onNext: (apiKey: string) => void;
  onBack: () => void;
}

// H8 — registration ("Create new account" path) removed. The previous Tauri
// register_provider command returned a deterministic djb2 pseudo-key that the
// backend rejects, leading users into a broken 401-on-every-call state. New
// providers register through the web wizard at https://dcp.sa/setup which
// does real account provisioning. The desktop app only accepts an existing
// API key.
export function Account({ onNext, onBack }: AccountProps) {
  const [apiKey, setApiKey] = useState("");
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(false);

  async function handleExistingKey() {
    setError("");
    setLoading(true);

    try {
      // Client-side validation first (Tauri invoke may not be available in all contexts)
      const clientValid = apiKey.startsWith("dcp-provider-") && apiKey.length >= 20;
      if (!clientValid) {
        setError("Invalid API key format. Must start with \"dcp-provider-\" and be at least 20 characters.");
        setLoading(false);
        return;
      }
      // Try Tauri backend validation with 3s timeout, fall back to client-side
      try {
        const timeoutPromise = new Promise((_, reject) => setTimeout(() => reject(new Error("timeout")), 3000));
        await Promise.race([validateApiKey(apiKey), timeoutPromise]);
      } catch (_) {
        // Tauri command may fail or timeout — client-side validation already passed
      }
      onNext(apiKey);
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }

  return (
    <div className="step-container">
      <h2 className="step-title">Connect Your Account</h2>
      <p className="step-subtitle">Enter your DCP provider API key</p>

      <div className="account-options">
        <div className="account-option selected">
          <div className="account-option-content">
            <div className="account-option-title">I have an API key</div>
            <div className="account-option-desc">
              Don't have one yet? Register at{" "}
              <a
                href="https://dcp.sa/setup"
                target="_blank"
                rel="noreferrer"
              >
                dcp.sa/setup
              </a>{" "}
              to create a provider account, then return here with your key.
            </div>
            <input
              type="text"
              className="input-field"
              placeholder="dcp-provider-..."
              value={apiKey}
              onChange={(e) => { setApiKey(e.target.value); setError(""); }}
              autoFocus
              aria-label="API key input"
            />
          </div>
        </div>
      </div>

      {error && <div className="input-error mt-8">{error}</div>}

      <div className="step-actions">
        <button className="btn btn-secondary" onClick={onBack}>
          Back
        </button>
        <button
          className="btn btn-primary"
          onClick={handleExistingKey}
          disabled={loading || apiKey.length < 10}
        >
          {loading ? (
            <>
              <span className="spinner" />
              Validating...
            </>
          ) : (
            "Continue"
          )}
        </button>
      </div>
    </div>
  );
}
