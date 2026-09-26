import { api } from "../api";
import type { AppConfiguration, ModifierSoundMode } from "../config";

interface Props {
  cfg: AppConfiguration;
  run: (p: Promise<unknown>) => void;
}

const MODES: [ModifierSoundMode, string][] = [
  ["soft", "Soft — quieter (25%)"],
  ["silent", "Silent"],
  ["full", "Full — same as other keys"],
  ["custom", "Custom gain/pitch"],
];

export default function Modifiers({ cfg, run }: Props) {
  const s = cfg.sound;
  const presets = Array.from({ length: 6 }, (_, i) =>
    cfg.favorites.find((f) => f.id === `preset-${i}`)
  );

  return (
    <>
      <div className="section">
        <h2>Modifier keys</h2>
        <div className="row">
          <span className="label">Mode</span>
          <select
            value={s.modifierSoundMode}
            onChange={(e) => run(api.setModifierMode(e.target.value))}
          >
            {MODES.map(([v, label]) => (
              <option key={v} value={v}>
                {label}
              </option>
            ))}
          </select>
        </div>
        {s.modifierSoundMode === "custom" && (
          <>
            <div className="row">
              <span className="label">Gain</span>
              <input
                type="range" min={0} max={1} step={0.01}
                value={s.modifierCustomVolume}
                onChange={(e) =>
                  run(api.setModifierGain(parseFloat(e.target.value)))
                }
                onMouseUp={() => run(api.preview("7:225"))}
              />
              <span className="value">
                {Math.round(s.modifierCustomVolume * 100)}%
              </span>
            </div>
            <div className="row">
              <span className="label">Pitch</span>
              <input
                type="range" min={-1} max={1} step={0.01}
                value={s.modifierCustomPitch}
                onChange={(e) =>
                  run(api.setModifierPitch(parseFloat(e.target.value)))
                }
                onMouseUp={() => run(api.preview("7:225"))}
              />
              <span className="value">{s.modifierCustomPitch.toFixed(2)}</span>
            </div>
          </>
        )}
        <div className="row">
          <span className="muted-note">
            Applies to Ctrl/Shift/Alt/Super, left and right independently.
          </span>
          <button className="small" onClick={() => run(api.preview("7:225"))}>
            Preview Shift
          </button>
        </div>
      </div>

      <div className="section">
        <h2>Presets</h2>
        <div className="muted-note">
          A preset snapshots sound settings + key overrides.
        </div>
        <div className="spacer" />
        <div className="preset-grid">
          {presets.map((fav, i) => (
            <div key={i} style={{ display: "flex", gap: 4 }}>
              <button
                className="small"
                style={{ flex: 1 }}
                disabled={!fav}
                title={fav ? `Apply ${fav.name}` : "Empty slot"}
                onClick={() => run(api.applyModifierPreset(i))}
              >
                {i + 1}
              </button>
              <button
                className="small"
                title="Save current settings into this slot"
                onClick={() => run(api.saveModifierPreset(i))}
              >
                ↓
              </button>
            </div>
          ))}
        </div>
      </div>
    </>
  );
}
