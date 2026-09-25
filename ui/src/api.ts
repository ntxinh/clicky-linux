import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type {
  AppConfiguration,
  DeviceInfo,
  Diagnostics,
  KeyOverride,
  ProfileInfo,
} from "./config";

// IPC command surface (mirrors ipc.rs). Callers refresh from the `config`
// event broadcast, not setter return values.

export const api = {
  getConfig: () => invoke<AppConfiguration>("get_config"),
  setEnabled: (enabled: boolean) => invoke("set_enabled", { enabled }),
  setVolume: (volume: number) => invoke("set_volume", { volume }),
  setTone: (tone: number) => invoke("set_tone", { tone }),
  setPitch: (pitch: number) => invoke("set_pitch", { pitch }),
  setSpatial: (enabled: boolean, width?: number) =>
    invoke("set_spatial", { enabled, width }),
  setProfile: (id: string) => invoke("set_profile", { id }),
  listProfiles: () => invoke<ProfileInfo[]>("list_profiles"),
  preview: (keyid?: string, profile?: string) =>
    invoke("preview", { keyid: keyid ?? null, profile: profile ?? null }),
  setModifierMode: (mode: string) => invoke("set_modifier_mode", { mode }),
  setModifierGain: (gain: number) => invoke("set_modifier_gain", { gain }),
  setModifierPitch: (pitch: number) => invoke("set_modifier_pitch", { pitch }),
  setKeyOverride: (keyid: string, fields: KeyOverride) =>
    invoke("set_key_override", { keyid, fields }),
  clearKeyOverride: (keyid: string) => invoke("clear_key_override", { keyid }),
  listDevices: () => invoke<DeviceInfo[]>("list_devices"),
  setOutputDevice: (name: string) => invoke("set_output_device", { name }),
  setLaunchAtLogin: (enabled: boolean) =>
    invoke("set_launch_at_login", { enabled }),
  importPack: (path: string) => invoke<ProfileInfo>("import_pack", { path }),
  getDiagnostics: () => invoke<Diagnostics>("get_diagnostics"),
  visualizerSet: (kind: string, enabled: boolean) =>
    invoke("visualizer_set", { kind, enabled }),
  saveModifierPreset: (slot: number) => invoke("save_modifier_preset", { slot }),
  applyModifierPreset: (slot: number) =>
    invoke("apply_modifier_preset", { slot }),
  quitApp: () => invoke("quit_app"),
};

export const onConfig = (cb: (cfg: AppConfiguration) => void) =>
  listen<AppConfiguration>("config", (e) => cb(e.payload));
