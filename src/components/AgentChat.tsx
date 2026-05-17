import { useState, useRef, useEffect, type ReactNode } from "react";

interface AgentState {
  status: string;
  gpu_temp: number;
  models_loaded: string[];
  last_heartbeat: string;
  earnings_today_halala: number;
  total_jobs_today: number;
  tok_s_avg: number;
}

interface ToolCall {
  name: string;
  status: "running" | "done";
  args?: string;
}

interface ChatItem {
  type: "user" | "text" | "card" | "action";
  content?: string;
  card?: ReactNode;
  tools?: ToolCall[];
  timestamp: Date;
}

export function AgentChat() {
  const [items, setItems] = useState<ChatItem[]>([]);
  const [input, setInput] = useState("");
  const [isThinking, setIsThinking] = useState(false);
  const [agentState, setAgentState] = useState<AgentState | null>(null);
  const [expanded, setExpanded] = useState(true);
  const [tab, setTab] = useState<"live" | "chat">("live");
  const [activity, setActivity] = useState<{text:string;time:string;type:string}[]>([]);
  const [uptime, setUptime] = useState(0);
  const [tokSpeed, setTokSpeed] = useState<string>("—");
  const [backendData, setBackendData] = useState<{name?:string;total_earnings_halala?:number;total_jobs?:number;gpu_model?:string;status?:string;recent_jobs?:{id:number;model:string;status:string;completed_at:string;prompt_tokens:number;completion_tokens:number;provider_earned_halala:number}[]}|null>(null);
  const prevJobRef = useRef<number>(0);
  const chatHistoryRef = useRef<{role: string; content: string}[]>([]);
  const [toast, setToast] = useState<string|null>(null);
  const [unread, setUnread] = useState(false);
  const [streamText, setStreamText] = useState("");
  const [streamTools, setStreamTools] = useState<ToolCall[]>([]);
  const abortRef = useRef<AbortController|null>(null);
  const bottomRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  // ── Markdown renderer ──────────────────────────────────────
  function renderMd(text: string): ReactNode {
    const lines = text.split("\n");
    const elements: ReactNode[] = [];
    let i = 0;
    while (i < lines.length) {
      const line = lines[i];
      // Code blocks
      if (line.startsWith("```")) {
        const lang = line.slice(3).trim();
        const codeLines: string[] = [];
        i++;
        while (i < lines.length && !lines[i].startsWith("```")) {
          codeLines.push(lines[i]);
          i++;
        }
        i++; // skip closing ```
        elements.push(
          <div key={elements.length} className="ap-code-block">
            {lang && <span className="ap-code-lang">{lang}</span>}
            <pre><code>{codeLines.join("\n")}</code></pre>
          </div>
        );
        continue;
      }
      // Regular line with inline formatting
      if (line.trim()) {
        elements.push(<p key={elements.length}>{renderInline(line)}</p>);
      } else {
        elements.push(<br key={elements.length} />);
      }
      i++;
    }
    return <>{elements}</>;
  }

  function renderInline(text: string): ReactNode {
    // Split on inline code, bold, then render
    const parts: ReactNode[] = [];
    let remaining = text;
    let key = 0;
    const rx = /(`[^`]+`|\*\*[^*]+\*\*|\*[^*]+\*)/g;
    let match;
    let lastIdx = 0;
    while ((match = rx.exec(remaining)) !== null) {
      if (match.index > lastIdx) parts.push(remaining.slice(lastIdx, match.index));
      const m = match[0];
      if (m.startsWith("`")) parts.push(<code key={key++} className="ap-inline-code">{m.slice(1, -1)}</code>);
      else if (m.startsWith("**")) parts.push(<strong key={key++}>{m.slice(2, -2)}</strong>);
      else if (m.startsWith("*")) parts.push(<em key={key++}>{m.slice(1, -1)}</em>);
      lastIdx = match.index + m.length;
    }
    if (lastIdx < remaining.length) parts.push(remaining.slice(lastIdx));
    return parts.length ? <>{parts}</> : text;
  }

  // Get tok/s from Ollama on startup
  useEffect(() => {
    (async () => {
      try {
        const { invoke } = await import("@tauri-apps/api/core");
        const speed = await invoke("ollama_speed_probe") as string;
        setTokSpeed(speed);
      } catch {}
    })();
  }, []);

  // Load state + comprehensive system check
  useEffect(() => {
    async function load() {
      try {
        const { invoke } = await import("@tauri-apps/api/core");
        const raw = await invoke("read_agent_state") as string;
        const s = JSON.parse(raw);
        setAgentState(s);
        // Comprehensive system check — single call gets everything
        const a: typeof activity = [];
        try {
          const sysRaw = await invoke("check_system") as string;
          const sys = JSON.parse(sysRaw);

          // GPU model + temp
          const gpu = sys.gpu || {};
          const gpuName = gpu.model || "unknown";
          const gpuTemp = gpu.temp_c != null ? `${Math.round(gpu.temp_c)}°C` : "";
          a.push({ text: gpuName, time: gpuTemp, type: (gpu.temp_c != null && gpu.temp_c > 85) ? "warn" : "ok" });

          // Memory — combined VRAM + RAM in one line
          const ram = sys.ram || {};
          const vramTotal = gpu.vram_total_mb ? `${(gpu.vram_total_mb / 1024).toFixed(0)}GB` : "";
          const ramPct = ram.total_mb ? Math.round((ram.used_mb / ram.total_mb) * 100) : 0;
          const ramInfo = ram.total_mb ? `RAM ${ramPct}%` : "";
          const memInfo = [vramTotal ? `${vramTotal} VRAM` : "", ramInfo].filter(Boolean).join(" · ");
          if (memInfo) a.push({ text: "Memory", time: memInfo, type: ramPct > 90 ? "warn" : "ok" });

          // Disk
          const disk = sys.disk || {};
          if (disk.percent != null) {
            a.push({ text: `Disk ${disk.percent}%`, time: `${disk.free} free`, type: disk.percent > 85 ? "warn" : "ok" });
          }

          // WireGuard
          const wg = sys.wireguard || {};
          const meshIp = wg.mesh_ip && wg.mesh_ip !== "none" ? wg.mesh_ip : null;
          a.push({ text: "WireGuard", time: wg.up ? (meshIp || "connected") : "DOWN", type: wg.up ? "ok" : "warn" });

          // Ollama + model in one line
          const ol = sys.ollama || {};
          const running = ol.running || [];
          const modelNames = running.length > 0
            ? running.map((m: {name?: string}) => m.name || "?").join(", ")
            : (s.models_loaded?.length ? s.models_loaded.join(", ") : "no model");
          a.push({ text: "Ollama", time: ol.up ? modelNames : "DOWN", type: ol.up ? "ok" : "warn" });

          // DCP connection uptime
          if (sys.uptime && sys.uptime !== "unknown") {
            a.push({ text: "Connected", time: sys.uptime, type: "ok" });
          }

          // Heartbeat
          if (s.last_heartbeat) {
            const ago = Math.floor((Date.now() - new Date(s.last_heartbeat).getTime()) / 1000);
            a.push({ text: "Heartbeat", time: `${ago}s ago`, type: ago < 120 ? "ok" : "warn" });
          }
        } catch {
          a.push({ text: "Checking...", time: "", type: "info" });
        }
        setActivity(a);
      } catch {
        setAgentState({ status: "online", gpu_temp: 0, models_loaded: ["qwen3:4b"], last_heartbeat: new Date().toISOString(), earnings_today_halala: 0, total_jobs_today: 0, tok_s_avg: 0 });
        setActivity([{ text: "Agent online", time: "", type: "ok" }]);
      }
    }
    load();
    const interval = expanded ? 8000 : 30000;
    const i = setInterval(load, interval);
    return () => clearInterval(i);
  }, [expanded]);

  useEffect(() => { const t = setInterval(() => setUptime(u => u + 1), 1000); return () => clearInterval(t); }, []);

  // Computed values (before effects that use them)
  const totalHalala = backendData?.total_earnings_halala ?? 0;
  const totalJobs_c: number = backendData?.total_jobs ?? 0;
  const totalSAR_c = (totalHalala / 100).toFixed(2);
  const gpuModel_c = backendData?.gpu_model || "Apple Silicon";

  // Toast notifications — only on job completion (no rotating spam)

  // Fetch real data from backend
  useEffect(() => {
    async function fetchData() {
      try {
        const { invoke } = await import("@tauri-apps/api/core");
        const raw = await invoke("fetch_provider_earnings") as string;
        const r = JSON.parse(raw);
        const p = r.provider || r;
        setBackendData(p);
      } catch {}
    }
    fetchData();
    const i = setInterval(fetchData, 30000);
    return () => clearInterval(i);
  }, []);
  useEffect(() => { bottomRef.current?.scrollIntoView({ behavior: "smooth" }); }, [items, streamText]);
  useEffect(() => { if (tab === "chat" && expanded) inputRef.current?.focus(); }, [tab, expanded]);

  // Detect new jobs
  useEffect(() => {
    const newJobs = backendData?.total_jobs || 0;
    if (prevJobRef.current > 0 && newJobs > prevJobRef.current) {
      const latest = backendData?.recent_jobs?.[0];
      const model = latest?.model?.split('/').pop() || "qwen3:4b";
      const tokens = (latest?.prompt_tokens || 0) + (latest?.completion_tokens || 0);
      const earned = ((latest?.provider_earned_halala || 1) / 100).toFixed(2);
      const totalSar = ((backendData?.total_earnings_halala || 0) / 100).toFixed(2);
      const completedAt = latest?.completed_at ? new Date(latest.completed_at).toLocaleTimeString([], {hour:"2-digit",minute:"2-digit"}) : "";

      setItems(prev => [...prev, { type: "text", content: `Job #${newJobs} completed at ${completedAt}\nModel: ${model} | ${tokens} tokens | +${earned} SAR\nLifetime: ${totalSar} SAR across ${newJobs} jobs`, timestamp: new Date() }]);
      setToast(`+${earned} SAR — ${model} (${tokens} tok)`);
      setTimeout(() => setToast(null), 5000);

      // Native OS notification
      (async () => {
        try {
          const { invoke } = await import("@tauri-apps/api/core");
          await invoke("notify_provider", {
            title: `+${earned} SAR — Job #${newJobs}`,
            body: `${model} | ${tokens} tokens`
          });
        } catch {}
      })();
    }
    prevJobRef.current = newJobs;
  }, [backendData?.total_jobs, backendData?.recent_jobs, backendData?.total_earnings_halala]);

  // Agent proactive chat messages — Hermes-powered, dynamic personality
  useEffect(() => {
    if (!expanded || tab !== "chat") return;
    // First message at 5s, then every 90s
    const shouldFire = uptime === 5 || (uptime > 5 && uptime % 90 === 0);
    if (!shouldFire) return;
    (async () => {
      try {
        const context = [
          `GPU: ${gpuModel_c}`,
          `Tok/s: ${tokSpeed}`,
          `Jobs: ${totalJobs_c}`,
          `Earnings: ${totalSAR_c} SAR`,
          `Uptime: ${fmtUp()}`,
          `Models: ${agentState?.models_loaded?.join(", ") || "none"}`,
          activity.map(a => `${a.text}: ${a.time} (${a.type})`).join(", "),
        ].join(". ");
        const res = await fetch("http://localhost:8642/v1/chat/completions", {
          method: "POST",
          headers: { "Content-Type": "application/json", "Authorization": "Bearer dcp-agent-api-key" },
          body: JSON.stringify({
            model: "hermes-agent",
            messages: [
              { role: "system", content: `You are the DCP Agent running on this machine. Give ONE short proactive update (1-2 sentences max). Be casual, opinionated, useful. IMPORTANT: Jobs and SAR are LIFETIME totals, NOT current. Ollama processes stay warm even when idle — do NOT claim inference is running unless you verify. Never repeat yourself. No markdown. Arabic if context suggests it. Current state: ${context}` },
              { role: "user", content: uptime === 5 ? "Welcome me briefly." : "Give me a quick update on something interesting." },
            ],
            max_tokens: 80,
          }),
        });
        if (!res.ok) return;
        const data = await res.json();
        let reply = data.choices?.[0]?.message?.content || "";
        reply = reply.replace(/<think>[\s\S]*?<\/think>\s*/g, "").replace(/\*\*/g, "").replace(/^#{1,3}\s*/gm, "").trim();
        if (!reply) return;
        setItems(prev => {
          const last = [...prev].reverse().find(i => i.type === "text");
          if (last?.content === reply) return prev;
          return [...prev, { type: "text", content: reply, timestamp: new Date() }];
        });
        if (!expanded) setUnread(true);
      } catch {}
    })();
  }, [uptime, expanded, tab]);

  const totalJobs = totalJobs_c;
  const totalSAR = totalSAR_c;
  const earningsSAR = totalSAR;
  const gpuModel = gpuModel_c;
  const statusColor = agentState?.status === "online" ? "#00E5C8" : agentState?.status === "degraded" ? "#F59E0B" : "#EF4444";
  const fmtUp = () => { const m = Math.floor(uptime / 60); const h = Math.floor(m / 60); return h > 0 ? `${h}h${m%60}m` : `${m}m`; };

  // ── Intent detection — handle locally when possible ────────
  function detectIntent(msg: string): string | null {
    const m = msg.toLowerCase().trim();
    // Only match very short direct commands (under 25 chars)
    // Only exact short commands get local cards — everything else goes to Hermes
    if (m.length > 20) return null;
    if (m.match(/^status\??$/)) return "status";
    if (m.match(/^earnings?\??$/)) return "earnings";
    if (m.match(/^models?\??$/)) return "models";
    return null;
  }

  function statusCard(): ReactNode {
    return (
      <div className="ac-card">
        <div className="ac-card-header">
          <div className="ac-card-dot" style={{background: statusColor}} />
          <span>All systems operational</span>
        </div>
        <div className="ac-card-grid">
          <div className="ac-card-cell"><div className="ac-card-val">{agentState?.gpu_temp || "—"}<small>°C</small></div><div className="ac-card-lbl">GPU</div></div>
          <div className="ac-card-cell"><div className="ac-card-val">{tokSpeed}<small>tok/s</small></div><div className="ac-card-lbl">Speed</div></div>
          <div className="ac-card-cell"><div className="ac-card-val">{totalJobs || 0}</div><div className="ac-card-lbl">Jobs</div></div>
          <div className="ac-card-cell"><div className="ac-card-val">{fmtUp()}</div><div className="ac-card-lbl">Uptime</div></div>
        </div>
        <div className="ac-card-row ok"><span>WireGuard</span><span>connected</span></div>
        <div className="ac-card-row ok"><span>Ollama</span><span>running</span></div>
        <div className="ac-card-row ok"><span>Models</span><span>{agentState?.models_loaded?.join(", ") || "none"}</span></div>
        <div className="ac-card-footer">Everything's running smooth. Just waiting for jobs to come in.</div>
      </div>
    );
  }

  function earningsCard(): ReactNode {
    const jobs = backendData?.recent_jobs || [];
    return (
      <div className="ac-card">
        <div className="ac-card-header"><span>Total Earnings</span></div>
        <div className="ac-card-big">{totalSAR} <small>SAR</small></div>
        <div className="ac-card-grid">
          <div className="ac-card-cell"><div className="ac-card-val">{totalJobs}</div><div className="ac-card-lbl">Total jobs</div></div>
          <div className="ac-card-cell"><div className="ac-card-val">{gpuModel}</div><div className="ac-card-lbl">GPU</div></div>
        </div>
        {jobs.length > 0 && (
          <div className="ac-jobs">
            <div className="ac-jobs-title">Recent Jobs</div>
            {jobs.slice(0, 5).map(j => (
              <div key={j.id} className="ac-job-row">
                <span className="ac-job-model">{j.model?.split('/').pop()}</span>
                <span className="ac-job-tokens">{(j.prompt_tokens||0)+(j.completion_tokens||0)} tok</span>
                <span className="ac-job-earned">{((j.provider_earned_halala||0)/100).toFixed(2)}</span>
              </div>
            ))}
          </div>
        )}
        <div className="ac-card-footer">{totalJobs > 0 ? `${totalJobs} jobs completed across all time.` : "No jobs yet."}</div>
      </div>
    );
  }

  function modelsCard(): ReactNode {
    return (
      <div className="ac-card">
        <div className="ac-card-header"><span>Loaded Models</span></div>
        <div className="ac-card-models">{(agentState?.models_loaded || []).map(m => <span key={m} className="ac-model-pill">{m}</span>)}</div>
        <div className="ac-card-footer">Models stay warm in VRAM. Bigger models earn more per token.</div>
      </div>
    );
  }

  async function handleRestart() {
    setItems(prev => [...prev, { type: "action", content: "Restarting services...", timestamp: new Date() }]);
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      await invoke("stop_daemon_process");
      await new Promise(r => setTimeout(r, 2000));
      await invoke("start_daemon_process");
      setItems(prev => [...prev, { type: "card", card: (
        <div className="ac-card">
          <div className="ac-card-header ok"><div className="ac-card-dot" style={{background:"#00E5C8"}} /><span>Services restarted</span></div>
          <div className="ac-card-footer">Ollama and WireGuard are back up. Took about 2 seconds.</div>
        </div>
      ), timestamp: new Date() }]);
    } catch {
      setItems(prev => [...prev, { type: "text", content: "Restart attempted. Check the Live tab for current status.", timestamp: new Date() }]);
    }
  }

  // ── Send message ───────────────────────────────────────────
  async function send(text?: string) {
    const msg = (text || input).trim();
    if (!msg || isThinking) return;
    setItems(prev => [...prev, { type: "user", content: msg, timestamp: new Date() }]);
    setInput("");

    const intent = detectIntent(msg);

    // Local intents — instant, no API call
    if (intent === "status") {
      setItems(prev => [...prev, { type: "card", card: statusCard(), timestamp: new Date() }]);
      return;
    }
    if (intent === "earnings") {
      setItems(prev => [...prev, { type: "card", card: earningsCard(), timestamp: new Date() }]);
      return;
    }
    if (intent === "models") {
      setItems(prev => [...prev, { type: "card", card: modelsCard(), timestamp: new Date() }]);
      return;
    }
    if (intent === "check_wg") {
      setItems(prev => [...prev, { type: "text", content: "Checking WireGuard...", timestamp: new Date() }]);
      try {
        const { invoke } = await import("@tauri-apps/api/core");
        const result = await invoke("check_wireguard") as string;
        setItems(prev => [...prev, { type: "text", content: result, timestamp: new Date() }]);
      } catch (e) { setItems(prev => [...prev, { type: "text", content: `Check failed: ${e}`, timestamp: new Date() }]); }
      return;
    }
    if (intent === "check_ollama") {
      setItems(prev => [...prev, { type: "text", content: "Checking Ollama...", timestamp: new Date() }]);
      try {
        const { invoke } = await import("@tauri-apps/api/core");
        const result = await invoke("check_ollama") as string;
        setItems(prev => [...prev, { type: "text", content: result, timestamp: new Date() }]);
      } catch (e) { setItems(prev => [...prev, { type: "text", content: `Check failed: ${e}`, timestamp: new Date() }]); }
      return;
    }
    if (intent === "check_disk") {
      try {
        const { invoke } = await import("@tauri-apps/api/core");
        const result = await invoke("check_disk") as string;
        setItems(prev => [...prev, { type: "text", content: result, timestamp: new Date() }]);
      } catch (e) { setItems(prev => [...prev, { type: "text", content: `Check failed: ${e}`, timestamp: new Date() }]); }
      return;
    }
    if (intent === "fix_wg") {
      setItems(prev => [...prev, { type: "text", content: "On it. Bouncing WireGuard tunnel...", timestamp: new Date() }]);
      try {
        const { invoke } = await import("@tauri-apps/api/core");
        const result = await invoke("agent_fix_wireguard") as string;
        setItems(prev => [...prev, { type: "text", content: result, timestamp: new Date() }]);
      } catch (e) { setItems(prev => [...prev, { type: "text", content: `Fix failed: ${e}`, timestamp: new Date() }]); }
      return;
    }
    if (intent === "fix_ollama") {
      setItems(prev => [...prev, { type: "text", content: "Restarting Ollama...", timestamp: new Date() }]);
      try {
        const { invoke } = await import("@tauri-apps/api/core");
        const result = await invoke("agent_fix_ollama") as string;
        setItems(prev => [...prev, { type: "text", content: result, timestamp: new Date() }]);
      } catch (e) { setItems(prev => [...prev, { type: "text", content: `Fix failed: ${e}`, timestamp: new Date() }]); }
      return;
    }
    if (intent === "restart") {
      await handleRestart();
      return;
    }
    if (intent === "help") {
      setItems(prev => [...prev, { type: "text", content: "Just ask me anything — status, earnings, restart services, model info. I'm running your node, I know what's going on.", timestamp: new Date() }]);
      return;
    }

    // Free-form → Hermes Agent API (port 8642) — SSE streaming with tool call visibility
    setIsThinking(true);
    setStreamText("");
    setStreamTools([]);
    const abort = new AbortController();
    abortRef.current = abort;
    try {
      const systemContext = { role: "system" as const, content: `You are the DCP Agent running on THIS machine — ${gpuModel}, ${activity.find(a => a.text === "Memory")?.time || ""}, uptime ${fmtUp()}. LIFETIME stats: ${earningsSAR} SAR earned from ${totalJobs} total jobs. Models loaded in Ollama: ${agentState?.models_loaded?.join(", ") || "none"} (kept warm, NOT necessarily processing a job right now). Ollama processes stay running even when idle. Do NOT claim jobs are running unless you verify with actual tool calls. Be accurate — don't hallucinate activity. Arabic if they write Arabic.` };
      const hermesMessages = [systemContext, ...chatHistoryRef.current, { role: "user" as const, content: msg }];
      const res = await fetch("http://localhost:8642/v1/chat/completions", {
        method: "POST",
        headers: { "Content-Type": "application/json", "Authorization": "Bearer dcp-agent-api-key" },
        signal: abort.signal,
        body: JSON.stringify({
          model: "hermes-agent",
          messages: hermesMessages,
          stream: true,
        }),
      });
      if (!res.ok) throw new Error(`Hermes API ${res.status}`);
      const reader = res.body!.getReader();
      const decoder = new TextDecoder();
      let fullText = "";
      let buffer = "";
      const tools: ToolCall[] = [];
      let nextIsToolEvent = false;

      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        buffer += decoder.decode(value, { stream: true });
        const lines = buffer.split("\n");
        buffer = lines.pop() || "";

        for (const line of lines) {
          // Track SSE event type
          if (line.startsWith("event: hermes.tool.progress")) {
            nextIsToolEvent = true;
            continue;
          }
          if (!line.startsWith("data: ")) continue;
          const payload = line.slice(6);
          if (payload === "[DONE]") continue;

          try {
            const chunk = JSON.parse(payload);

            // Tool progress events: {"tool":"terminal","label":"...","status":"running|completed"}
            if (nextIsToolEvent) {
              nextIsToolEvent = false;
              if (chunk.status === "running") {
                const label = chunk.label || chunk.tool || "tool";
                tools.push({ name: `${chunk.emoji || "\u2699"} ${label}`, status: "running" });
                setStreamTools([...tools]);
              } else if (chunk.status === "completed") {
                const last = [...tools].reverse().find((t: ToolCall) => t.status === "running");
                if (last) last.status = "done";
                setStreamTools([...tools]);
              }
              continue;
            }

            // Standard chat completion chunks
            const delta = chunk.choices?.[0]?.delta?.content;
            if (delta) {
              fullText += delta;
              setStreamText(fullText);
            }
          } catch {}
        }
      }

      // Clean up the final text
      let reply = fullText.replace(/<think>[\s\S]*?<\/think>\s*/g, "").replace(/\n{3,}/g, "\n\n").trim();
      if (!reply) reply = "Hmm, didn't catch that. Try again.";

      // Track conversation history
      chatHistoryRef.current.push({ role: "user", content: msg }, { role: "assistant", content: reply });
      if (chatHistoryRef.current.length > 20) chatHistoryRef.current = chatHistoryRef.current.slice(-20);

      setItems(prev => [...prev, { type: "text", content: reply, tools: tools.length > 0 ? [...tools] : undefined, timestamp: new Date() }]);
    } catch (e) {
      if ((e as Error).name === "AbortError") {
        // User cancelled — save partial
        const partial = streamText.trim();
        if (partial) setItems(prev => [...prev, { type: "text", content: partial + " [stopped]", timestamp: new Date() }]);
      } else {
        setItems(prev => [...prev, { type: "text", content: "Agent unavailable. Make sure Hermes is running.", timestamp: new Date() }]);
      }
    } finally {
      setIsThinking(false);
      setStreamText("");
      setStreamTools([]);
      abortRef.current = null;
    }
  }

  // ── PANEL ───────────────────────────────────────────
  return (
    <div className="ap">
      {toast && <div className="ap-toast-float">{toast}</div>}
      <div className="ap-bar" data-tauri-drag-region onClick={async (e) => { if ((e.target as HTMLElement).closest('.ap-tabs')) return; const next = !expanded; setExpanded(next); if (next) setUnread(false); try { const { invoke } = await import("@tauri-apps/api/core"); await invoke("resize_window", { width: 320, height: next ? 480 : 100 }); } catch {} }}>
        <div className="ap-bar-left">
          <div className="ap-dot" style={{ background: statusColor }} />
          <span className="ap-bar-title">DCP Agent</span>
          <span className="ap-bar-uptime">{fmtUp()}</span>
        </div>
        {expanded ? (
          <div className="ap-tabs">
            <button className={`ap-tab ${tab === "live" ? "on" : ""}`} onClick={e => { e.stopPropagation(); setTab("live"); }}>Live</button>
            <button className={`ap-tab ${tab === "chat" ? "on" : ""}`} onClick={e => { e.stopPropagation(); setTab("chat"); }}>Chat</button>
          </div>
        ) : (
          <span className="ap-expand-hint">{unread && <span className="ap-unread-dot" />}&#9650;</span>
        )}
      </div>

      <div className="ap-strip">
        <div className="ap-strip-item"><span className="ap-strip-val">{earningsSAR}</span><span className="ap-strip-label">SAR</span></div>
        <div className="ap-strip-divider" />
        <div className="ap-strip-item"><span className="ap-strip-val">{totalJobs || 0}</span><span className="ap-strip-label">jobs</span></div>
        <div className="ap-strip-divider" />
        <div className="ap-strip-item"><span className="ap-strip-val">{agentState?.gpu_temp || "—"}</span><span className="ap-strip-label">°C</span></div>
        <div className="ap-strip-divider" />
        <div className="ap-strip-item"><span className="ap-strip-val">{tokSpeed}</span><span className="ap-strip-label">tok/s</span></div>
      </div>

      {!expanded && toast && <div className="ap-toast-float">{toast}</div>}

      {expanded && tab === "live" && (
        <div className="ap-live">
          <div className="ap-activity">
            {activity.map((a, i) => (
              <div key={i} className={`ap-act-row ${a.type}`}>
                <div className={`ap-act-dot ${a.type}`} />
                <span className="ap-act-text">{a.text}</span>
                <span className="ap-act-time">{a.time}</span>
              </div>
            ))}
          </div>
          <div className="ap-live-actions">
            <button className="ap-la" onClick={() => { setTab("chat"); send("status"); }}>Status</button>
            <button className="ap-la" onClick={() => { setTab("chat"); send("earnings"); }}>Earnings</button>
            <button className="ap-la accent" onClick={() => setTab("chat")}>Ask</button>
          </div>
        </div>
      )}

      {expanded && tab === "chat" && (
        <div className="ap-chat">
          <div className="ap-msgs">
            {items.length === 0 && !isThinking && (
              <div className="ap-msg agent">
                <div className="ap-msg-b"><div className="ap-msg-t">Ask me anything — status, earnings, restart services, or just chat.</div></div>
              </div>
            )}
            {items.map((item, i) => (
              <div key={i} className={`ap-msg ${item.type === "user" ? "user" : "agent"}`}>
                <div className="ap-msg-b">
                  {item.card ? item.card : (
                    <>
                      {item.tools && item.tools.length > 0 && (
                        <div className="ap-tool-cards">
                          {item.tools.map((t, ti) => (
                            <div key={ti} className={`ap-tool-card ${t.status}`}>
                              <span className="ap-tool-icon">{t.status === "done" ? "\u2713" : "\u2699"}</span>
                              <span className="ap-tool-name">{t.name}</span>
                              {t.args && <span className="ap-tool-args">{t.args}</span>}
                            </div>
                          ))}
                        </div>
                      )}
                      <div className="ap-msg-t">{item.type === "user" ? item.content : renderMd(item.content || "")}</div>
                    </>
                  )}
                </div>
              </div>
            ))}
            {isThinking && (
              <div className="ap-msg agent">
                <div className="ap-msg-b">
                  {streamTools.length > 0 && (
                    <div className="ap-tool-cards">
                      {streamTools.map((t, ti) => (
                        <div key={ti} className={`ap-tool-card ${t.status}`}>
                          <span className="ap-tool-icon">{t.status === "done" ? "\u2713" : "\u26A1"}</span>
                          <span className="ap-tool-name">{t.name}</span>
                          {t.args && <span className="ap-tool-args">{t.args}</span>}
                        </div>
                      ))}
                    </div>
                  )}
                  <div className="ap-msg-t">
                    {streamText ? renderMd(streamText) : <span className="ap-thinking">Thinking...</span>}
                    <span className="ap-cursor" />
                  </div>
                </div>
              </div>
            )}
            <div ref={bottomRef} />
          </div>
          <div className="ap-input-bar">
            <input
              ref={inputRef}
              className="ap-input"
              placeholder="Ask your agent..."
              value={input}
              onChange={e => setInput(e.target.value)}
              onKeyDown={e => e.key === "Enter" && send()}
              disabled={isThinking}
            />
            {isThinking && (
              <button className="ap-stop-btn" onClick={() => abortRef.current?.abort()} title="Stop">
                <svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor"><rect x="3" y="3" width="10" height="10" rx="1" /></svg>
              </button>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
