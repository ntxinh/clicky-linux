import { useEffect, useState } from "react";
import { api } from "../api";
import type { AppConfiguration, DeviceInfo, Diagnostics } from "../config";

interface Props {
  cfg: AppConfiguration;
  run: (p: Promise<unknown>) => void;
}

export default function AudioPage({ cfg, run }: Props) {
  const [devices, setDevices] = useState<DeviceInfo[]>([]);
  const [diag, setDiag] = useState<Diagnostics | null>(null);

  const refresh = () => {
    api.listDevices().then(setDevices).catch(() => {});
    api.getDiagnostics().then(setDiag).catch(() => {});
  };
  useEffect(refresh, []);

  const current =
    cfg.sound.outputDeviceUID ??
    devices.find((d) => d.is_default)?.name ??
    "";

  return (
    <>
      <div className="section">
        <h2>Output device</h2>
        <div className="row">
          <select
            style={{ flex: 1, minWidth: 0 }}
            value={current}
            onChange={(e) => run(api.setOutputDevice(e.target.value))}
          >
            {devices.map((d) => (
              <option key={d.name} value={d.name}>
                {d.name}
                {d.is_default ? " (default)" : ""}
              </option>
            ))}
          </select>
          <button className="small" onClick={refresh}>
            Refresh
          </button>
        </div>
        <div className="row">
          <span className="label">Preview</span>
          <button onClick={() => run(api.preview())}>Play</button>
        </div>
      </div>
      {diag && (
        <div className="section">
          <h2>Diagnostics</h2>
          <div className="row">
            <span className="label">Capture</span>
            <span className="value">{diag.capture}</span>
          </div>
          <div className="row">
            <span className="label">Keyboards</span>
            <span className="value">{diag.keyboards}</span>
          </div>
          <div className="row">
            <span className="label">Output</span>
            <span className="value">
              {diag.sample_rate} Hz · {diag.channels}ch
            </span>
          </div>
          <div className="row">
            <span className="label">Triggers</span>
            <span className="value">
              {diag.accepted} ok · {diag.dropped} dropped · {diag.stolen} stolen
            </span>
          </div>
          {diag.issues.map((i) => (
            <div className="muted-note" key={i}>
              {i}
            </div>
          ))}
        </div>
      )}
    </>
  );
}
