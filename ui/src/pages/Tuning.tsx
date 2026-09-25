import { useState } from "react";
import { api } from "../api";
import type { AppConfiguration } from "../config";

interface Props {
  cfg: AppConfiguration;
  run: (p: Promise<unknown>) => void;
}

// ANSI 60% rows as (label, keyid "page:usage", width in units). Usages per
// HID usage tables page 7; matches engine's 15-column LAYOUT geometry.
type KeyDef = [label: string, keyid: string, w?: number];
const ROWS: KeyDef[][] = [
  [
    ["`", "7:53"], ["1", "7:30"], ["2", "7:31"], ["3", "7:32"], ["4", "7:33"],
    ["5", "7:34"], ["6", "7:35"], ["7", "7:36"], ["8", "7:37"], ["9", "7:38"],
    ["0", "7:39"], ["-", "7:45"], ["=", "7:46"], ["⌫", "7:42", 2],
  ],
  [
    ["Tab", "7:43", 1.5], ["Q", "7:20"], ["W", "7:26"], ["E", "7:8"],
    ["R", "7:21"], ["T", "7:23"], ["Y", "7:28"], ["U", "7:24"], ["I", "7:12"],
    ["O", "7:18"], ["P", "7:19"], ["[", "7:47"], ["]", "7:48"], ["\\", "7:49", 1.5],
  ],
  [
    ["Caps", "7:57", 1.75], ["A", "7:4"], ["S", "7:22"], ["D", "7:7"],
    ["F", "7:9"], ["G", "7:10"], ["H", "7:11"], ["J", "7:13"], ["K", "7:14"],
    ["L", "7:15"], [";", "7:51"], ["'", "7:52"], ["⏎", "7:40", 2.25],
  ],
  [
    ["Shift", "7:225", 2.25], ["Z", "7:29"], ["X", "7:27"], ["C", "7:6"],
    ["V", "7:25"], ["B", "7:5"], ["N", "7:17"], ["M", "7:16"], [",", "7:54"],
    [".", "7:55"], ["/", "7:56"], ["Shift", "7:229", 2.75],
  ],
  [
    ["Ctrl", "7:224", 1.25], ["Super", "7:227", 1.25], ["Alt", "7:226", 1.25],
    ["Space", "7:44", 6.25], ["Alt", "7:230", 1.25], ["Fn", "7:-1", 1.25],
    ["Menu", "7:101", 1.25], ["Ctrl", "7:228", 1.25],
  ],
];

const UNIT = 100 / 15;

export default function Tuning({ cfg, run }: Props) {
  const [selected, setSelected] = useState<string | null>(null);
  const s = cfg.sound;
  const over = selected ? cfg.keyOverrides[selected] : undefined;

  return (
    <>
      <div className="section">
        <h2>Global tuning</h2>
        <div className="row">
          <span className="label">Tone</span>
          <input
            type="range" min={-1} max={1} step={0.01} value={s.tone}
            onChange={(e) => run(api.setTone(parseFloat(e.target.value)))}
            onMouseUp={() => run(api.preview())}
          />
          <span className="value">{s.tone.toFixed(2)}</span>
        </div>
        <div className="row">
          <span className="label">Pitch</span>
          <input
            type="range" min={-1} max={1} step={0.01} value={s.pitch}
            onChange={(e) => run(api.setPitch(parseFloat(e.target.value)))}
            onMouseUp={() => run(api.preview())}
          />
          <span className="value">{s.pitch.toFixed(2)}</span>
        </div>
        <div className="row">
          <span className="label">Spatial</span>
          <button
            className={`switch ${s.spatial ? "on" : ""}`}
            onClick={() => run(api.setSpatial(!s.spatial))}
          />
        </div>
        {s.spatial && (
          <div className="row">
            <span className="label">Width</span>
            <input
              type="range" min={0} max={1} step={0.01} value={s.spatialWidth}
              onChange={(e) =>
                run(api.setSpatial(true, parseFloat(e.target.value)))
              }
            />
            <span className="value">{s.spatialWidth.toFixed(2)}</span>
          </div>
        )}
      </div>

      <div className="section">
        <h2>Per-key override</h2>
        <div className="kbd">
          {ROWS.map((row, i) => (
            <div className="krow" key={i}>
              {row.map(([label, keyid, w]) => {
                if (keyid === "7:-1")
                  return (
                    <div
                      key={keyid}
                      className="key"
                      style={{ width: `${(w ?? 1) * UNIT}%` }}
                    />
                  );
                const cls = [
                  "key",
                  cfg.keyOverrides[keyid] ? "overridden" : "",
                  selected === keyid ? "selected" : "",
                ].join(" ");
                return (
                  <button
                    key={keyid}
                    className={cls}
                    style={{ width: `${(w ?? 1) * UNIT}%` }}
                    title={keyid}
                    onClick={() => {
                      setSelected(keyid);
                      run(api.preview(keyid));
                    }}
                  >
                    {label}
                  </button>
                );
              })}
            </div>
          ))}
        </div>
        {selected && (
          <>
            <div className="spacer" />
            <div className="row">
              <span className="label">Key {selected}</span>
              <button
                className="small"
                disabled={!over}
                onClick={() => run(api.clearKeyOverride(selected))}
              >
                Clear override
              </button>
            </div>
            {(["volume", "pitch", "tone"] as const).map((f) => {
              const v = over?.[f];
              const min = f === "volume" ? 0 : -1;
              const fallback = f === "volume" ? s.volume : s[f];
              return (
                <div className="row" key={f}>
                  <span className="label">
                    {f[0].toUpperCase() + f.slice(1)}
                    {v == null && <span className="muted-note"> (inherited)</span>}
                  </span>
                  <input
                    type="range" min={min} max={1} step={0.01}
                    value={v ?? fallback}
                    onChange={(e) =>
                      run(
                        api.setKeyOverride(selected, {
                          profileID: over?.profileID ?? null,
                          tone: over?.tone ?? null,
                          pitch: over?.pitch ?? null,
                          volume: over?.volume ?? null,
                          [f]: parseFloat(e.target.value),
                        })
                      )
                    }
                    onMouseUp={() => run(api.preview(selected))}
                  />
                  <span className="value">{(v ?? fallback).toFixed(2)}</span>
                </div>
              );
            })}
          </>
        )}
      </div>
    </>
  );
}
