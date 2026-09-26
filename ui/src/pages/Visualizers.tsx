import { api } from "../api";
import type { AppConfiguration } from "../config";

interface Props {
  cfg: AppConfiguration;
  run: (p: Promise<unknown>) => void;
}

const KINDS: [string, string][] = [
  ["keyboard", "On-screen keyboard — highlights presses"],
  ["keystrokes", "Keystroke pills — key names + modifiers"],
  ["combo", "Combo counter — presses while modifiers held"],
  ["bezel", "Bezel — pulsing screen border"],
  ["keyboard3d", "3D keyboard (experimental)"],
];

export default function Visualizers({ cfg, run }: Props) {
  return (
    <div className="section">
      <h2>Overlays</h2>
      {KINDS.map(([kind, desc]) => (
        <div className="row" key={kind}>
          <span>
            <div>{kind}</div>
            <div className="muted-note">{desc}</div>
          </span>
          <button
            className={`switch ${cfg.visualizer.kinds[kind] ? "on" : ""}`}
            onClick={() =>
              run(api.visualizerSet(kind, !cfg.visualizer.kinds[kind]))
            }
          />
        </div>
      ))}
      <div className="muted-note" style={{ marginTop: 8 }}>
        Overlay windows land with the layer-shell task; toggles persist now.
      </div>
    </div>
  );
}
