import { useEffect, useState } from "react";
import { api, onConfig } from "./api";
import type { AppConfiguration } from "./config";
import Home from "./pages/Home";
import Library from "./pages/Library";
import Tuning from "./pages/Tuning";
import Modifiers from "./pages/Modifiers";
import Visualizers from "./pages/Visualizers";
import AudioPage from "./pages/Audio";

const TABS = [
  ["home", "Home"],
  ["library", "Library"],
  ["tuning", "Tuning"],
  ["modifiers", "Modifiers"],
  ["visualizers", "Visualizers"],
  ["audio", "Audio"],
] as const;

type Tab = (typeof TABS)[number][0];

export default function App() {
  const [tab, setTab] = useState<Tab>("home");
  const [cfg, setCfg] = useState<AppConfiguration | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api.getConfig().then(setCfg).catch((e) => setError(String(e)));
    let unlisten: (() => void) | undefined;
    onConfig(setCfg).then((u) => (unlisten = u));
    return () => unlisten?.();
  }, []);

  // IPC setters resolve or throw; surface failures, config itself arrives
  // via the `config` broadcast.
  const run = (p: Promise<unknown>) => p.catch((e) => setError(String(e)));

  return (
    <div className="app">
      <header>
        <span className="logo">clicky</span>
        {cfg && (
          <span className={`dot ${cfg.enabled ? "on" : ""}`} title={cfg.enabled ? "enabled" : "muted"} />
        )}
      </header>
      <nav>
        {TABS.map(([id, label]) => (
          <button
            key={id}
            className={tab === id ? "tab active" : "tab"}
            onClick={() => setTab(id)}
          >
            {label}
          </button>
        ))}
      </nav>
      <main>
        {error && (
          <div className="error" onClick={() => setError(null)}>
            {error}
          </div>
        )}
        {!cfg && !error && <div className="loading">loading…</div>}
        {cfg && tab === "home" && <Home cfg={cfg} run={run} />}
        {cfg && tab === "library" && <Library cfg={cfg} run={run} />}
        {cfg && tab === "tuning" && <Tuning cfg={cfg} run={run} />}
        {cfg && tab === "modifiers" && <Modifiers cfg={cfg} run={run} />}
        {cfg && tab === "visualizers" && <Visualizers cfg={cfg} run={run} />}
        {cfg && tab === "audio" && <AudioPage cfg={cfg} run={run} />}
      </main>
    </div>
  );
}
