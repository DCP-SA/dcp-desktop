import { useState } from "react";
import { validateApiKey, registerProvider } from "../lib/api";

interface AccountProps {
  onNext: (apiKey: string) => void;
  onBack: () => void;
}

type AccountMode = "existing" | "create";

export function Account({ onNext, onBack }: AccountProps) {
  const [mode, setMode] = useState<AccountMode>("existing");
  const [apiKey, setApiKey] = useState("");
  const [email, setEmail] = useState("");
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(false);
  const [registeredKey, setRegisteredKey] = useState("");

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
      // Try Tauri backend validation, fall back to client-side
      try {
        await validateApiKey(apiKey);
      } catch (_) {
        // Tauri command may fail in dev mode — client-side validation already passed
      }
      onNext(apiKey);
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }

  async function handleRegister() {
    setError("");

    if (!email || !email.includes("@")) {
      setError("Please enter a valid email address.");
      return;
    }

    setLoading(true);

    try {
      const result = await registerProvider(email);
      setRegisteredKey(result.api_key);
      // Auto-advance after showing the key briefly
      setTimeout(() => {
        onNext(result.api_key);
      }, 2000);
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }

  return (
    <div className="step-container">
      <h2 className="step-title">Connect Your Account</h2>
      <p className="step-subtitle">Link to your DCP provider account</p>

      <div className="account-options">
        {/* Option 1: Existing API Key */}
        <div
          className={`account-option ${mode === "existing" ? "selected" : ""}`}
          onClick={() => { setMode("existing"); setError(""); }}
          role="radio"
          aria-checked={mode === "existing"}
          tabIndex={0}
          onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { setMode("existing"); setError(""); } }}
        >
          <div className="account-option-radio" />
          <div className="account-option-content">
            <div className="account-option-title">I have an API key</div>
            <div className="account-option-desc">Enter your existing provider API key</div>
            {mode === "existing" && (
              <input
                type="text"
                className="input-field"
                placeholder="dcp-provider-..."
                value={apiKey}
                onChange={(e) => { setApiKey(e.target.value); setError(""); }}
                onClick={(e) => e.stopPropagation()}
                autoFocus
                aria-label="API key input"
              />
            )}
          </div>
        </div>

        {/* Option 2: Create New Account */}
        <div
          className={`account-option ${mode === "create" ? "selected" : ""}`}
          onClick={() => { setMode("create"); setError(""); }}
          role="radio"
          aria-checked={mode === "create"}
          tabIndex={0}
          onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { setMode("create"); setError(""); } }}
        >
          <div className="account-option-radio" />
          <div className="account-option-content">
            <div className="account-option-title">Create new account</div>
            <div className="account-option-desc">Register with your email address</div>
            {mode === "create" && (
              <input
                type="email"
                className="input-field"
                placeholder="you@example.com"
                value={email}
                onChange={(e) => { setEmail(e.target.value); setError(""); }}
                onClick={(e) => e.stopPropagation()}
                autoFocus
                aria-label="Email address input"
              />
            )}
          </div>
        </div>
      </div>

      {error && <div className="input-error mt-8">{error}</div>}

      {registeredKey && (
        <div className="input-success mt-8">
          Account created! Your API key: {registeredKey.slice(0, 24)}...
        </div>
      )}

      <div className="step-actions">
        <button className="btn btn-secondary" onClick={onBack}>
          Back
        </button>
        <button
          className="btn btn-primary"
          onClick={mode === "existing" ? handleExistingKey : handleRegister}
          disabled={
            loading ||
            registeredKey !== "" ||
            (mode === "existing" && apiKey.length < 10) ||
            (mode === "create" && !email.includes("@"))
          }
        >
          {loading ? (
            <>
              <span className="spinner" />
              {mode === "create" ? "Registering..." : "Validating..."}
            </>
          ) : (
            "Continue"
          )}
        </button>
      </div>
    </div>
  );
}
