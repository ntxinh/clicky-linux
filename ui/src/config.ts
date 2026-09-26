// Wire types — mirror clicky-core AppConfiguration serde names (camelCase,
// profileID uppercase, per `#[serde(rename_all = "camelCase")]` + renames).

export type ModifierSoundMode = "soft" | "silent" | "full" | "custom";

export interface KeyOverride {
  profileID?: string | null;
  tone?: number | null;
  pitch?: number | null;
  volume?: number | null;
}

export interface SoundSettings {
  profileID: string;
  volume: number;
  tone: number;
  pitch: number;
  spatial: boolean;
  spatialWidth: number;
  normalization: boolean;
  variation: boolean;
  homeRowSoftness: number;
  modifierSoundMode: ModifierSoundMode;
  modifierCustomVolume: number;
  modifierCustomPitch: number;
  outputDeviceUID?: string | null;
  mouseSound: string;
  mouseVolume: number;
  enterSound: string;
  enterVolume: number;
}

export interface VisualizerSettings {
  enabled: boolean;
  kinds: Record<string, boolean>;
  style: string;
  placement: string;
  theme: string;
  scale: number;
  offset: number;
  dismissDelay: number;
  comboTimeout: number;
  keepCombo: boolean;
  keepVisible: boolean;
}

export interface GeneralSettings {
  launchAtLogin: boolean;
  showMenuBar: boolean;
}

export interface Favorite {
  id: string;
  name: string;
  sound: SoundSettings;
  keyOverrides: Record<string, KeyOverride>;
}

export interface AppConfiguration {
  schemaVersion: number;
  enabled: boolean;
  sound: SoundSettings;
  visualizer: VisualizerSettings;
  general: GeneralSettings;
  keyOverrides: Record<string, KeyOverride>;
  favorites: Favorite[];
}

export interface ProfileInfo {
  id: string;
  name: string;
  brand: string | null;
  subtitle: string;
  color: string;
  key_count: number;
}

export interface DeviceInfo {
  name: string;
  is_default: boolean;
}

export interface Diagnostics {
  enabled: boolean;
  profile: string;
  audio_device: string;
  sample_rate: number;
  channels: number;
  capture: "live" | "dead" | "no-keyboards" | "denied";
  keyboards: number;
  issues: string[];
  accepted: number;
  dropped: number;
  stolen: number;
}
