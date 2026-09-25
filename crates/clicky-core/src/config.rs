//! Clicky `AppConfiguration` v1 — persisted settings store.
//!
//! Field names and defaults are ported verbatim from Clicky so a config.json
//! written by either app round-trips in the other. Every field deserializes
//! with `#[serde(default)]` so partial JSON merges with defaults
//! (forward-compatible); unknown fields are ignored.
//!
//! Store: `load()` gates on `schemaVersion == 1`, backs up unreadable or
//! foreign configs to `<file>.bak`, and never overwrites them. Writes are
//! atomic (`config.json.tmp` + rename) and debounced to ≥250 ms apart —
//! `save_debounced` marks a pending write, `flush()` (called by the daemon on
//! a timer/shutdown) persists.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

const SCHEMA_VERSION: u32 = 1;
const DEBOUNCE: Duration = Duration::from_millis(250);
const MAX_FAVORITES: usize = 6;

/// Store errors.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// `schemaVersion` was present but != 1. The file was moved to `.bak`.
    #[error("unsupported config schemaVersion (expected 1)")]
    UnknownSchema,
    /// Filesystem error.
    #[error(transparent)]
    Io(#[from] io::Error),
}

fn def_profile_id() -> String {
    "thocky".into()
}
fn def_true() -> bool {
    true
}
fn def_spatial_width() -> f32 {
    0.8
}
fn def_home_row_softness() -> f32 {
    0.15
}
fn def_mouse_volume() -> f32 {
    0.25
}
fn def_offset() -> f64 {
    28.0
}
fn def_one_f64() -> f64 {
    1.0
}
fn def_combo_timeout() -> f64 {
    2.0
}
fn def_shortcut_usage() -> u32 {
    14 // HID usage K
}
fn def_tap_count() -> u32 {
    3
}

/// Clamp; NaN-safe (`PartialOrd` fails open to `lo`).
fn clamp<T: PartialOrd>(v: T, lo: T, hi: T) -> T {
    if v >= lo {
        if v <= hi {
            v
        } else {
            hi
        }
    } else {
        lo
    }
}

/// `modifierSoundMode`: soft | silent | full | custom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModifierSoundMode {
    Soft,
    Silent,
    Full,
    Custom,
}

impl Default for ModifierSoundMode {
    fn default() -> Self {
        Self::Soft
    }
}

/// Bundled/imported extra sounds (mouse clicks, Enter).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExtraSound {
    None,
    Soft,
    Crisp,
    Hard,
    Ding,
    Typewriter,
    Custom,
    #[serde(rename = "razer-orochi-v2")]
    RazerOrochiV2,
}

impl Default for ExtraSound {
    fn default() -> Self {
        Self::None
    }
}

/// User-imported single sound ({name, pressPath, releasePath?}).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ImportedSound {
    pub name: String,
    pub press_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_path: Option<String>,
}

impl Default for ImportedSound {
    fn default() -> Self {
        Self {
            name: String::new(),
            press_path: String::new(),
            release_path: None,
        }
    }
}

/// `sound{…}` — global sound/tuning settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SoundSettings {
    #[serde(rename = "profileID", default = "def_profile_id")]
    pub profile_id: String,
    pub volume: f32,
    pub tone: f32,
    pub pitch: f32,
    #[serde(default = "def_true")]
    pub spatial: bool,
    #[serde(default = "def_spatial_width")]
    pub spatial_width: f32,
    #[serde(default = "def_true")]
    pub normalization: bool,
    #[serde(default = "def_true")]
    pub variation: bool,
    #[serde(default = "def_home_row_softness")]
    pub home_row_softness: f32,
    pub modifier_sound_mode: ModifierSoundMode,
    #[serde(rename = "outputDeviceUID", skip_serializing_if = "Option::is_none")]
    pub output_device_uid: Option<String>,
    #[serde(default = "def_mouse_sound")]
    pub mouse_sound: ExtraSound,
    #[serde(default = "def_mouse_volume")]
    pub mouse_volume: f32,
    pub enter_sound: ExtraSound,
    pub enter_volume: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_mouse: Option<ImportedSound>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_enter: Option<ImportedSound>,
}

fn def_mouse_sound() -> ExtraSound {
    ExtraSound::Soft
}

impl Default for SoundSettings {
    fn default() -> Self {
        Self {
            profile_id: def_profile_id(),
            volume: 0.4,
            tone: 0.0,
            pitch: 0.0,
            spatial: true,
            spatial_width: 0.8,
            normalization: true,
            variation: true,
            home_row_softness: 0.15,
            modifier_sound_mode: ModifierSoundMode::Soft,
            output_device_uid: None,
            mouse_sound: ExtraSound::Soft,
            mouse_volume: 0.25,
            enter_sound: ExtraSound::None,
            enter_volume: 0.4,
            custom_mouse: None,
            custom_enter: None,
        }
    }
}

impl SoundSettings {
    fn validate(&mut self) {
        self.volume = clamp(self.volume, 0.0, 1.0);
        self.tone = clamp(self.tone, -1.0, 1.0);
        self.pitch = clamp(self.pitch, -1.0, 1.0);
        self.spatial_width = clamp(self.spatial_width, 0.0, 1.0);
        self.home_row_softness = clamp(self.home_row_softness, 0.0, 1.0);
        self.mouse_volume = clamp(self.mouse_volume, 0.0, 1.0);
        self.enter_volume = clamp(self.enter_volume, 0.0, 1.0);
    }
}

/// Per-key override; all fields optional. A fully-null entry in
/// `keyOverrides` is a delete and is dropped by `validated()`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct KeyOverride {
    #[serde(rename = "profileID", skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tone: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pitch: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volume: Option<f32>,
}

impl KeyOverride {
    /// True when every field is null/absent (i.e. a delete marker).
    pub fn is_empty(&self) -> bool {
        self.profile_id.is_none()
            && self.tone.is_none()
            && self.pitch.is_none()
            && self.volume.is_none()
    }

    fn validate(&mut self) {
        self.tone = self.tone.map(|v| clamp(v, -1.0, 1.0));
        self.pitch = self.pitch.map(|v| clamp(v, -1.0, 1.0));
        self.volume = self.volume.map(|v| clamp(v, 0.0, 1.0));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VisualizerStyle {
    Keyboard,
    Keystrokes,
    Combo,
    Bezel,
}

impl Default for VisualizerStyle {
    fn default() -> Self {
        Self::Keyboard
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VisualizerPlacement {
    Cursor,
    TopLeft,
    TopCenter,
    TopRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
    Random,
}

impl Default for VisualizerPlacement {
    fn default() -> Self {
        Self::Cursor
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VisualizerTheme {
    Graphite,
    Porcelain,
    Amber,
    GlassDark,
    GlassClear,
}

impl Default for VisualizerTheme {
    fn default() -> Self {
        Self::Graphite
    }
}

/// `visualizer{…}` — on-screen keystroke overlay settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct VisualizerSettings {
    pub enabled: bool,
    pub style: VisualizerStyle,
    pub placement: VisualizerPlacement,
    pub theme: VisualizerTheme,
    #[serde(default = "def_one_f64")]
    pub scale: f64,
    #[serde(default = "def_offset")]
    pub offset: f64,
    #[serde(default = "def_one_f64")]
    pub dismiss_delay: f64,
    #[serde(default = "def_combo_timeout")]
    pub combo_timeout: f64,
    pub keep_combo: bool,
    pub keep_visible: bool,
}

impl Default for VisualizerSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            style: VisualizerStyle::Keyboard,
            placement: VisualizerPlacement::Cursor,
            theme: VisualizerTheme::Graphite,
            scale: 1.0,
            offset: 28.0,
            dismiss_delay: 1.0,
            combo_timeout: 2.0,
            keep_combo: false,
            keep_visible: false,
        }
    }
}

impl VisualizerSettings {
    fn validate(&mut self) {
        self.scale = clamp(self.scale, 0.5, 2.0);
        self.offset = clamp(self.offset, 0.0, 150.0);
        self.dismiss_delay = clamp(self.dismiss_delay, 0.3, 5.0);
        self.combo_timeout = clamp(self.combo_timeout, 0.3, 30.0);
    }
}

/// Shortcut modifier mask, serialized as Clicky's `KeyModifiers` OptionSet:
/// `{"rawValue": N}` with bits command=1, shift=2, option=4, control=8,
/// function=16 (macOS labels; command≙GUI/Super, option≙Alt).
///
/// The engine's held-modifier mask is per-usage (bit = usage − 224, L/R
/// distinct); this type is the L/R-collapsed form. Mapping:
/// CONTROL ↔ usages 224/228, SHIFT ↔ 225/229, OPTION ↔ 226/230,
/// COMMAND ↔ 227/231 (any held modifier of that kind sets the bit).
/// FUNCTION (Clicky bit 16) has no page-7 usage in 224–231; it survives
/// round-trips but never matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShortcutModifiers {
    /// Clicky OptionSet raw value.
    pub raw_value: u32,
}

impl ShortcutModifiers {
    /// GUI/Super (Clicky `command`).
    pub const COMMAND: Self = Self { raw_value: 1 };
    /// Shift (Clicky `shift`).
    pub const SHIFT: Self = Self { raw_value: 2 };
    /// Alt (Clicky `option`).
    pub const OPTION: Self = Self { raw_value: 4 };
    /// Ctrl (Clicky `control`).
    pub const CONTROL: Self = Self { raw_value: 8 };
    /// Fn (Clicky `function`; no HID usage in 224–231).
    pub const FUNCTION: Self = Self { raw_value: 16 };

    /// Collapse an engine held-modifier mask (bit = usage − 224) into
    /// Clicky kind bits — either side sets the bit.
    pub fn from_held_mask(held: u8) -> Self {
        let mut v = 0;
        if held & 0b_0001_0001 != 0 {
            v |= Self::CONTROL.raw_value; // 224 | 228
        }
        if held & 0b_0010_0010 != 0 {
            v |= Self::SHIFT.raw_value; // 225 | 229
        }
        if held & 0b_0100_0100 != 0 {
            v |= Self::OPTION.raw_value; // 226 | 230
        }
        if held & 0b_1000_1000 != 0 {
            v |= Self::COMMAND.raw_value; // 227 | 231
        }
        Self { raw_value: v }
    }

    /// True if `held` (engine mask, bit = usage − 224) contains every kind
    /// this mask requires; either side counts. FUNCTION never matches (no
    /// page-7 usage). Exact-equality checks are the recognizer's job.
    pub fn matches(&self, held: u8) -> bool {
        let kinds = Self::from_held_mask(held).raw_value;
        let required = self.raw_value & !Self::FUNCTION.raw_value;
        kinds & required == required
    }
}

impl Default for ShortcutModifiers {
    /// Default shortcut: Super/GUI held (Clicky `command`).
    fn default() -> Self {
        Self::COMMAND
    }
}

#[derive(Serialize, Deserialize)]
struct RawValue {
    #[serde(rename = "rawValue", default)]
    raw_value: u32,
}

impl Serialize for ShortcutModifiers {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        RawValue {
            raw_value: self.raw_value,
        }
        .serialize(s)
    }
}

impl<'de> Deserialize<'de> for ShortcutModifiers {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let r = RawValue::deserialize(d)?;
        Ok(Self {
            raw_value: r.raw_value,
        })
    }
}

/// Toggle-shortcut: N taps of `usage` while `modifiers` are held, inside
/// `interval` seconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ShortcutConfiguration {
    #[serde(default = "def_shortcut_usage")]
    pub usage: u32,
    pub modifiers: ShortcutModifiers,
    #[serde(default = "def_tap_count")]
    pub tap_count: u32,
    #[serde(default = "def_one_f64")]
    pub interval: f64,
}

/// Default: Super + K ×3 taps in 1 s.
impl Default for ShortcutConfiguration {
    fn default() -> Self {
        Self {
            usage: 14,
            modifiers: ShortcutModifiers::COMMAND,
            tap_count: 3,
            interval: 1.0,
        }
    }
}

impl ShortcutConfiguration {
    fn validate(&mut self) {
        self.tap_count = clamp(self.tap_count, 1, 5);
        self.interval = clamp(self.interval, 0.3, 3.0);
    }
}

/// `general{…}` — app-level settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GeneralSettings {
    pub launch_at_login: bool,
    #[serde(default = "def_true")]
    pub show_menu_bar: bool,
    pub shortcut: ShortcutConfiguration,
}

impl Default for GeneralSettings {
    fn default() -> Self {
        Self {
            launch_at_login: false,
            show_menu_bar: true,
            shortcut: ShortcutConfiguration::default(),
        }
    }
}

/// Preset snapshot ({id, name, sound, keyOverrides}), max 6 stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Favorite {
    pub id: String,
    pub name: String,
    /// Sound snapshot for preset recall. Non-optional on the wire — Clicky's
    /// decoder rejects a Favorite without `sound`.
    pub sound: SoundSettings,
    pub key_overrides: HashMap<String, KeyOverride>,
}

impl Default for Favorite {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            sound: SoundSettings::default(),
            key_overrides: HashMap::new(),
        }
    }
}


/// Clicky `AppConfiguration` v1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppConfiguration {
    #[serde(default = "def_schema_version")]
    pub schema_version: u32,
    #[serde(default = "def_true")]
    pub enabled: bool,
    pub sound: SoundSettings,
    pub visualizer: VisualizerSettings,
    pub general: GeneralSettings,
    pub key_overrides: HashMap<String, KeyOverride>,
    pub favorites: Vec<Favorite>,
}

fn def_schema_version() -> u32 {
    SCHEMA_VERSION
}

impl Default for AppConfiguration {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            enabled: true,
            sound: SoundSettings::default(),
            visualizer: VisualizerSettings::default(),
            general: GeneralSettings::default(),
            key_overrides: HashMap::new(),
            favorites: Vec::new(),
        }
    }
}

impl AppConfiguration {
    /// Clamp all numerics to spec §5 ranges, truncate favorites to 6, and
    /// drop fully-null key overrides (delete markers).
    pub fn validated(mut self) -> Self {
        self.sound.validate();
        self.visualizer.validate();
        self.general.shortcut.validate();
        self.key_overrides.retain(|_, o| !o.is_empty());
        for o in self.key_overrides.values_mut() {
            o.validate();
        }
        self.favorites.truncate(MAX_FAVORITES);
        for f in &mut self.favorites {
            f.sound.validate();
            f.key_overrides.retain(|_, o| !o.is_empty());
            for o in f.key_overrides.values_mut() {
                o.validate();
            }
        }
        self
    }

    /// Serialize as pretty JSON, Clicky field names.
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }
}

/// Persisted store for an `AppConfiguration` file.
///
/// `cfg` is the authoritative in-memory copy; `save_debounced` updates it and
/// schedules an atomic write ≥250 ms after the first pending change;
#[derive(Debug)]
pub struct Store {
    /// Current configuration (validated).
    pub cfg: AppConfiguration,
    path: PathBuf,
    pending_since: Option<Instant>,
}

impl Store {
    /// Default config path: `~/.config/clicky/config.json`.
    pub fn default_path() -> PathBuf {
        directories::BaseDirs::new()
            .map(|b| b.config_dir().join("clicky/config.json"))
            .unwrap_or_else(|| PathBuf::from("config.json"))
    }

    /// Load from `path`. Missing file → defaults. Parse error → file moved to
    /// `.bak`, defaults returned. `schemaVersion != 1` → `Err(UnknownSchema)`
    /// with the file moved to `.bak` (never overwritten).
    pub fn load(path: impl Into<PathBuf>) -> Result<Self, Error> {
        let path = path.into();
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(Self::at(path, AppConfiguration::default()));
            }
            Err(e) => return Err(e.into()),
        };
        match serde_json::from_str::<AppConfiguration>(&text) {
            Ok(cfg) if cfg.schema_version == SCHEMA_VERSION => {
                Ok(Self::at(path, cfg.validated()))
            }
            Ok(_) => {
                let _ = backup(&path);
                Err(Error::UnknownSchema)
            }
            Err(_) => {
                let _ = backup(&path);
                Ok(Self::at(path, AppConfiguration::default()))
            }
        }
    }

    fn at(path: PathBuf, cfg: AppConfiguration) -> Self {
        Self {
            cfg,
            path,
            pending_since: None,
        }
    }

    /// File this store reads/writes.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Update the in-memory config (validated — Clicky's `save()` writes
    /// `config.validated()`, and `self.cfg` mirrors what lands on disk) and
    /// coalesce a write. The write lands on `flush()`, or immediately once
    /// ≥250 ms have passed since the first pending change — writes are always
    /// ≥250 ms apart.
    pub fn save_debounced(&mut self, cfg: &AppConfiguration) -> Result<(), Error> {
        self.cfg = cfg.clone().validated();
        let since = self.pending_since.get_or_insert_with(Instant::now);
        if since.elapsed() >= DEBOUNCE {
            self.flush()?;
        }
        Ok(())
    }

    /// Persist now if a debounced write is pending (no-op otherwise).
    pub fn flush(&mut self) -> Result<(), Error> {
        if self.pending_since.take().is_some() {
            write_atomic(&self.path, &self.cfg)?;
        }
        Ok(())
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

fn backup(path: &Path) -> io::Result<()> {
    fs::rename(path, suffixed(path, ".bak"))
}

fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

fn write_atomic(path: &Path, cfg: &AppConfiguration) -> Result<(), Error> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = suffixed(path, ".tmp");
    // Re-validate at the boundary — on-disk must always be in-range.
    let json = cfg
        .clone()
        .validated()
        .to_json()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    fs::write(&tmp, json)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    fn tmpdir() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "clicky-cfg-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn cfg_path() -> PathBuf {
        tmpdir().join("config.json")
    }

    #[test]
    fn round_trip() {
        let cfg = AppConfiguration::default();
        let json = cfg.to_json().unwrap();
        // Clicky field names on the wire.
        for key in [
            "schemaVersion",
            "keyOverrides",
            "profileID",
            "spatialWidth",
            "homeRowSoftness",
            "modifierSoundMode",
            "outputDeviceUID",
            "mouseSound",
            "enterSound",
            "dismissDelay",
            "comboTimeout",
            "launchAtLogin",
            "showMenuBar",
            "tapCount",
        ] {
            // outputDeviceUID is skipped when None; check the rest.
            if key == "outputDeviceUID" {
                continue;
            }
            assert!(json.contains(&format!("\"{key}\"")), "missing {key}");
        }
        let back: AppConfiguration = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn enums_serde_strings() {
        let s = serde_json::to_string(&ModifierSoundMode::Soft).unwrap();
        assert_eq!(s, "\"soft\"");
        let s = serde_json::to_string(&ExtraSound::RazerOrochiV2).unwrap();
        assert_eq!(s, "\"razer-orochi-v2\"");
        let m: ModifierSoundMode = serde_json::from_str("\"custom\"").unwrap();
        assert_eq!(m, ModifierSoundMode::Custom);
    }

    #[test]
    fn missing_file_is_defaults() {
        let store = Store::load(cfg_path()).unwrap();
        assert_eq!(store.cfg, AppConfiguration::default());
        assert!(store.cfg.enabled);
        assert_eq!(store.cfg.sound.profile_id, "thocky");
    }

    #[test]
    fn partial_json_merges_defaults() {
        let p = cfg_path();
        fs::write(&p, r#"{"enabled": false, "sound": {"volume": 0.9}}"#).unwrap();
        let store = Store::load(&p).unwrap();
        assert!(!store.cfg.enabled);
        assert_eq!(store.cfg.sound.volume, 0.9);
        // untouched fields get defaults
        assert_eq!(store.cfg.sound.profile_id, "thocky");
        assert_eq!(store.cfg.visualizer.dismiss_delay, 1.0);
        assert_eq!(store.cfg.general.shortcut.tap_count, 3);
    }

    #[test]
    fn unknown_schema_version_backs_up_and_errors() {
        let p = cfg_path();
        fs::write(&p, r#"{"schemaVersion": 99, "enabled": false}"#).unwrap();
        let err = Store::load(&p).unwrap_err();
        assert!(matches!(err, Error::UnknownSchema));
        let bak = suffixed(&p, ".bak");
        assert!(bak.exists());
        assert!(fs::read_to_string(bak).unwrap().contains("99"));
        assert!(!p.exists(), "original moved aside, never overwritten");
    }

    #[test]
    fn corrupt_json_backs_up_and_defaults() {
        let p = cfg_path();
        fs::write(&p, "{not json!!").unwrap();
        let store = Store::load(&p).unwrap();
        assert_eq!(store.cfg, AppConfiguration::default());
        assert!(suffixed(&p, ".bak").exists());
    }

    #[test]
    fn clamps_on_load() {
        let p = cfg_path();
        fs::write(
            &p,
            r#"{
                "sound": {"volume": 2.0, "tone": -9.0, "pitch": 7.0,
                          "spatialWidth": 4.0, "homeRowSoftness": -1.0,
                          "mouseVolume": 2.0, "enterVolume": -5.0},
                "visualizer": {"scale": 0.0, "offset": 999.0,
                               "dismissDelay": 99.0, "comboTimeout": 0.0},
                "general": {"shortcut": {"tapCount": 99, "interval": 0.0}},
                "keyOverrides": {"7:4": {"volume": 3.0, "tone": -8.0}}
            }"#,
        )
        .unwrap();
        let store = Store::load(&p).unwrap();
        let c = &store.cfg;
        assert_eq!(c.sound.volume, 1.0);
        assert_eq!(c.sound.tone, -1.0);
        assert_eq!(c.sound.pitch, 1.0);
        assert_eq!(c.sound.spatial_width, 1.0);
        assert_eq!(c.sound.home_row_softness, 0.0);
        assert_eq!(c.sound.mouse_volume, 1.0);
        assert_eq!(c.sound.enter_volume, 0.0);
        assert_eq!(c.visualizer.scale, 0.5);
        assert_eq!(c.visualizer.offset, 150.0);
        assert_eq!(c.visualizer.dismiss_delay, 5.0);
        assert_eq!(c.visualizer.combo_timeout, 0.3);
        assert_eq!(c.general.shortcut.tap_count, 5);
        assert_eq!(c.general.shortcut.interval, 0.3);
        let o = &c.key_overrides["7:4"];
        assert_eq!(o.volume, Some(1.0));
        assert_eq!(o.tone, Some(-1.0));
    }

    #[test]
    fn seventh_favorite_dropped() {
        let mut cfg = AppConfiguration::default();
        for i in 0..7 {
            cfg.favorites.push(Favorite {
                id: format!("f{i}"),
                name: format!("fav{i}"),
                ..Default::default()
            });
        }
        let cfg = cfg.validated();
        assert_eq!(cfg.favorites.len(), 6);
        assert_eq!(cfg.favorites[5].id, "f5");
    }

    #[test]
    fn empty_override_dropped_partial_kept() {
        let mut cfg = AppConfiguration::default();
        cfg.key_overrides
            .insert("7:4".into(), KeyOverride::default()); // delete marker
        cfg.key_overrides.insert(
            "7:5".into(),
            KeyOverride {
                volume: Some(0.5),
                ..Default::default()
            },
        );
        let cfg = cfg.validated();
        assert!(!cfg.key_overrides.contains_key("7:4"));
        assert_eq!(cfg.key_overrides["7:5"].volume, Some(0.5));

        // Null JSON object behaves the same.
        let p = cfg_path();
        fs::write(&p, r#"{"keyOverrides": {"7:4": {}, "7:5": {"pitch": 0.2}}}"#).unwrap();
        let store = Store::load(&p).unwrap();
        assert!(!store.cfg.key_overrides.contains_key("7:4"));
        assert_eq!(store.cfg.key_overrides["7:5"].pitch, Some(0.2));
    }

    #[test]
    fn debounced_save_coalesces_and_flushes_atomically() {
        let p = cfg_path();
        let mut store = Store::load(&p).unwrap();

        let mut cfg = AppConfiguration::default();
        cfg.enabled = false;
        store.save_debounced(&cfg).unwrap();
        assert!(!p.exists(), "debounced write pending, not yet on disk");

        // Second change inside the window coalesces — still nothing written.
        cfg.sound.volume = 0.7;
        store.save_debounced(&cfg).unwrap();
        assert!(!p.exists());

        store.flush().unwrap();
        assert!(p.exists());
        assert!(!suffixed(&p, ".tmp").exists(), "tmp renamed away");
        let on_disk: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(on_disk["enabled"], false);
        assert_eq!(on_disk["sound"]["volume"], 0.7);
    }

    #[test]
    fn debounced_save_writes_after_window() {
        let p = cfg_path();
        let mut store = Store::load(&p).unwrap();
        let cfg = AppConfiguration::default();
        store.save_debounced(&cfg).unwrap();
        assert!(!p.exists());
        std::thread::sleep(Duration::from_millis(260));
        store.save_debounced(&cfg).unwrap();
        assert!(p.exists(), "write lands once window elapsed");
    }
    #[test]
    fn clicky_modifier_object_round_trips() {
        // Clicky serializes KeyModifiers as {"rawValue": N}.
        let p = cfg_path();
        fs::write(
            &p,
            r#"{"general": {"shortcut": {"usage": 14, "modifiers": {"rawValue": 8}, "tapCount": 3, "interval": 1.0}}}"#,
        )
        .unwrap();
        let store = Store::load(&p).unwrap();
        assert_eq!(store.cfg.general.shortcut.modifiers.raw_value, 8);

        // Round-trips back to the same wire shape.
        let json = store.cfg.to_json().unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            v["general"]["shortcut"]["modifiers"],
            serde_json::json!({ "rawValue": 8 })
        );
    }

    #[test]
    fn modifier_kind_collapses_left_right() {
        // LCtrl (224) or RCtrl (228) → control bit 8; either matches.
        assert_eq!(
            ShortcutModifiers::from_held_mask(1 << 0).raw_value,
            8
        );
        assert_eq!(
            ShortcutModifiers::from_held_mask(1 << 4).raw_value,
            8
        );
        // Default shortcut = Super/GUI; LGUI 227 → bit 3 held → rawValue 1.
        let m = ShortcutModifiers::from_held_mask(1 << 3);
        assert_eq!(m.raw_value, 1);
        assert!(ShortcutModifiers::COMMAND.matches(1 << 3));
        assert!(!ShortcutModifiers::COMMAND.matches(1 << 0));
        // LShift (225) + RShift (229) same kind.
        assert_eq!(ShortcutModifiers::from_held_mask(1 << 1 | 1 << 5).raw_value, 2);
    }

    #[test]
    fn saved_output_is_always_validated() {
        let p = cfg_path();
        let mut store = Store::load(&p).unwrap();
        let mut cfg = AppConfiguration::default();
        cfg.sound.volume = 2.0;
        cfg.visualizer.dismiss_delay = 99.0;
        store.save_debounced(&cfg).unwrap();
        store.flush().unwrap();

        // In-memory copy mirrors on-disk.
        assert_eq!(store.cfg.sound.volume, 1.0);
        let v: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["sound"]["volume"], 1.0);
        assert_eq!(v["visualizer"]["dismissDelay"], 5.0);
    }
}
