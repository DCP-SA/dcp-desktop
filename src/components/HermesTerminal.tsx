import { useState, useEffect } from "react";

export function HermesTerminal() {
  const [token, setToken] = useState("dcp-agent-ws-token-fixed");

  useEffect(() => {
    (async () => {
      try {
        const { invoke } = await import("@tauri-apps/api/core");
        const t = await invoke("get_hermes_session_token") as string;
        if (t) setToken(t.trim());
      } catch {}
    })();
  }, []);

  // Embed the Hermes dashboard directly — it handles WebSocket, PTY, everything
  return (
    <iframe
      src={`http://localhost:18789/chat?token=${token}`}
      style={{
        flex: 1,
        border: "none",
        borderRadius: "0 0 12px 12px",
        background: "#0A1018",
        width: "100%",
        minHeight: "300px",
      }}
      allow="clipboard-read; clipboard-write"
    />
  );
}
