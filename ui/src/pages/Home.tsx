import { useEffect, useState } from "react";
import { api } from "../api";
import type { AppConfiguration, ProfileInfo } from "../config";

interface Props {
  cfg: AppConfiguration;
  run: (p: Promise<unknown>) => void;
}

export default function Home({ cfg, run }: Props) {
  const [profiles, setProfiles] = useState<ProfileInfo[]>([]);
  useEffect(() => {
    api.listProfiles().then(setProfiles).catch(() => {});
  }, []);
  const active = profiles.find((p) => p.id === cfg.sound.profileID);

  return (
    <>
      <div className="section">
        <h2>Sounds</h2>
        <div className="row">
          <span className="label">Enabled</span>
          <button
            className={`switch ${cfg.enabled ? "on" : ""}`}
            onClick={() => run(api.setEnabled(!cfg.enabled))}
          />
        </div>
        <div className="row">
          <span className="label">Profile</span>
          <select
            value={cfg.sound.profileID}
            onChange={(e) => run(api.setProfile(e.target.value))}
          >
            {profiles.map((p) => (
              <option key={p.id} value={p.id}>
                {p.name}
              </option>
            ))}
          </select>
        </div>
        <div className="row">
          <span className="label">Volume</span>
          <input
            type="range"
            min={0}
            max={1}
            step={0.01}
            value={cfg.sound.volume}
            onChange={(e) => run(api.setVolume(parseFloat(e.target.value)))}
          />
          <span className="value">{Math.round(cfg.sound.volume * 100)}%</span>
        </div>
        <div className="row">
          <span className="label">Preview</span>
          <button onClick={() => run(api.preview())}>
            Play {active?.name ?? cfg.sound.profileID}
          </button>
        </div>
      </div>
      <div className="section">
        <h2>App</h2>
        <div className="row">
          <span className="label">Launch at login</span>
          <button
            className={`switch ${cfg.general.launchAtLogin ? "on" : ""}`}
            onClick={() => run(api.setLaunchAtLogin(!cfg.general.launchAtLogin))}
          />
        </div>
        <div className="row">
          <span className="label">Quit clicky</span>
          <button onClick={() => run(api.quitApp())}>Quit</button>
        </div>
      </div>
    </>
  );
}
