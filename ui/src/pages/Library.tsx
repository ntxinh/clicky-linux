import { useEffect, useMemo, useState } from "react";
import { api } from "../api";
import type { AppConfiguration, ProfileInfo } from "../config";

interface Props {
  cfg: AppConfiguration;
  run: (p: Promise<unknown>) => void;
}

export default function Library({ cfg, run }: Props) {
  const [profiles, setProfiles] = useState<ProfileInfo[]>([]);
  const [query, setQuery] = useState("");
  const [importPath, setImportPath] = useState("");
  const [importing, setImporting] = useState(false);

  const refresh = () => {
    api.listProfiles().then(setProfiles).catch(() => {});
  };
  useEffect(refresh, []);

  const groups = useMemo(() => {
    const q = query.trim().toLowerCase();
    const shown = q
      ? profiles.filter(
          (p) =>
            p.name.toLowerCase().includes(q) ||
            (p.brand ?? "").toLowerCase().includes(q)
        )
      : profiles;
    const m = new Map<string, ProfileInfo[]>();
    for (const p of shown) {
      const brand = p.brand ?? "Other";
      m.set(brand, [...(m.get(brand) ?? []), p]);
    }
    return [...m.entries()].sort(([a], [b]) => a.localeCompare(b));
  }, [profiles, query]);

  const select = (id: string) => {
    run(api.setProfile(id));
    run(api.preview("7:40", id));
  };

  const doImport = () => {
    const path = importPath.trim();
    if (!path) return;
    setImporting(true);
    api
      .importPack(path)
      .then((info) => {
        refresh();
        setImportPath("");
        run(api.setProfile(info.id));
      })
      .catch((e) => alert(String(e)))
      .finally(() => setImporting(false));
  };

  return (
    <>
      <div className="section">
        <div className="row">
          <input
            type="text"
            placeholder="Search profiles…"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
        </div>
        <div className="row">
          <input
            type="text"
            placeholder="Import pack — path to pack.json dir"
            value={importPath}
            onChange={(e) => setImportPath(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && doImport()}
          />
          <button className="small" disabled={importing} onClick={doImport}>
            {importing ? "…" : "Import"}
          </button>
        </div>
      </div>
      {groups.map(([brand, ps]) => (
        <div key={brand}>
          <div className="brand">{brand}</div>
          <div className="cards">
            {ps.map((p) => (
              <button
                key={p.id}
                className={`card ${p.id === cfg.sound.profileID ? "active" : ""}`}
                onClick={() => select(p.id)}
                onDoubleClick={() => run(api.preview("7:40", p.id))}
              >
                <span
                  className="swatch"
                  style={{ background: p.color || "var(--accent)" }}
                />
                <span className="name">{p.name}</span>
                {p.subtitle && <div className="sub">{p.subtitle}</div>}
              </button>
            ))}
          </div>
        </div>
      ))}
    </>
  );
}
