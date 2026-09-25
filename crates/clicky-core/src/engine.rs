//! Trigger resolution — port of Clicky's `AudioController` typing path:
//! `KeyEvent` → mixer `Trigger`.
//!
//! Resolution order per key: key override → modifier policy → profile
//! default. The release variant is chosen AT PRESS time and remembered
//! (frozen trigger) so a mid-hold profile/config change can't cross wires;
//! a muted press records `None` → silent release.
//!
//! Gain stack (DSP.swift):
//! `gain = volume × profile.gain × normalization × jitter × 2` and the
//! typing path multiplies override volume, modifier gain and home-row
//! softness into `volume`. Recorded releases ×0.7. Pitch speed
//! `2^(pitch×0.5)` × jitter, clamped 0.25–4. Variation jitter: ±2.5% pitch,
//! 0.94–1.0 gain.

use std::collections::HashMap;

use crate::config::{AppConfiguration, ModifierSoundMode};
use crate::mixer::{Trigger, TriggerProducer};
use crate::normalizer::{KeyEvent, Phase};
use crate::profiles::Profile;

/// Recorded release strokes sit ~3.1 dB under the press.
const RELEASE_GAIN: f32 = 0.7;
/// Prepared assets carry 0.32 extraction gain; restore 6 dB at playback.
const PLAYBACK_CAL: f32 = 2.0;
/// Modifier usages (page 7): L/R Ctrl, Shift, Alt, GUI.
const MODIFIER_USAGES: core::ops::RangeInclusive<u16> = 224..=231;
/// Home-row usages softened by `home_row_softness` (Clicky `isHomeRow`).
const HOME_ROW: &[u16] = &[4, 22, 7, 9, 13, 14, 15, 51];

/// Key geometry over a 15-column ANSI board — port of Clicky's
/// `KeyboardLayout.swift` (`usage, x, width`; y irrelevant to pan).
/// pan position = `(x + w/2) / 15 × 2 − 1`.
const LAYOUT: &[(u16, f32, f32)] = &[
    (41, 0.0, 1.5), // esc
    (58, 1.5, 1.0), (59, 2.5, 1.0), (60, 3.5, 1.0), (61, 4.5, 1.0),
    (62, 5.5, 1.0), (63, 6.5, 1.0), (64, 7.5, 1.0), (65, 8.5, 1.0),
    (66, 9.5, 1.0), (67, 10.5, 1.0), (68, 11.5, 1.0), (69, 12.5, 1.0), // F1–F12
    (76, 13.5, 1.5), // fwd delete
    (53, 0.0, 1.0), (30, 1.0, 1.0), (31, 2.0, 1.0), (32, 3.0, 1.0),
    (33, 4.0, 1.0), (34, 5.0, 1.0), (35, 6.0, 1.0), (36, 7.0, 1.0),
    (37, 8.0, 1.0), (38, 9.0, 1.0), (39, 10.0, 1.0), (45, 11.0, 1.0),
    (46, 12.0, 1.0), // ` 1–0 − =
    (42, 13.0, 2.0), // backspace
    (43, 0.0, 1.5), // tab
    (20, 1.5, 1.0), (26, 2.5, 1.0), (8, 3.5, 1.0), (21, 4.5, 1.0),
    (23, 5.5, 1.0), (28, 6.5, 1.0), (24, 7.5, 1.0), (12, 8.5, 1.0),
    (18, 9.5, 1.0), (19, 10.5, 1.0), (47, 11.5, 1.0), (48, 12.5, 1.0), // Q–P [ ]
    (49, 13.5, 1.5), // backslash
    (57, 0.0, 1.75), // caps
    (4, 1.75, 1.0), (22, 2.75, 1.0), (7, 3.75, 1.0), (9, 4.75, 1.0),
    (10, 5.75, 1.0), (11, 6.75, 1.0), (13, 7.75, 1.0), (14, 8.75, 1.0),
    (15, 9.75, 1.0), (51, 10.75, 1.0), (52, 11.75, 1.0), // A–L ; '
    (40, 12.75, 2.25), // return
    (225, 0.0, 2.25), // lshift
    (29, 2.25, 1.0), (27, 3.25, 1.0), (6, 4.25, 1.0), (25, 5.25, 1.0),
    (5, 6.25, 1.0), (17, 7.25, 1.0), (16, 8.25, 1.0), (54, 9.25, 1.0),
    (55, 10.25, 1.0), (56, 11.25, 1.0), // Z–M , . /
    (229, 12.25, 2.75), // rshift
    (255, 0.0, 1.0), (224, 1.0, 1.0), (226, 2.0, 1.0), (227, 3.0, 1.25),
    (44, 4.25, 5.0), (231, 9.25, 1.25), (230, 10.5, 1.0), (80, 11.5, 1.0),
    (81, 12.5, 1.0), (82, 13.5, 0.75), (79, 14.25, 0.75), // fn ⌃ ⌥ ⌘ space ⌘ ⌥ ←↓↑→
];

/// Per-sample normalization correction — `AudioNormalization.gain`:
/// `clamp((target/(rms×profileGain))^0.75, 0.5, min(cap, 0.65/(peak×profileGain)))`
/// where cap is 1.8, or 1.0 for the "silent" pack (kept silent).
fn norm_gain(rms: f32, peak: f32, profile_gain: f32, target: f32, silent: bool) -> f32 {
    if rms <= 0.00001 || peak <= 0.0 || profile_gain <= 0.0 {
        return 1.0;
    }
    let correction = (target / (rms * profile_gain)).powf(0.75);
    let headroom = 0.65 / (peak * profile_gain);
    let cap: f32 = if silent { 1.0 } else { 1.8 };
    // Swift `clamped(to:)` tolerates upper < lower (result = upper);
    // `f32::clamp` panics. A peak×gain > 1.3 would otherwise crash profile load.
    correction.max(0.5).min(cap.min(headroom))
}

/// `AudioSampleSelector.index`: random index ≠ previous when possible.
fn select_variant(count: usize, previous: Option<usize>) -> usize {
    match previous {
        Some(p) if count > 1 && p < count => {
            let s = fastrand::usize(..count - 1);
            if s >= p {
                s + 1
            } else {
                s
            }
        }
        _ => fastrand::usize(..count),
    }
}

/// Median of `rms × profile.gain` over a sample list (missing levels skipped).
fn median_level(ids: &[u16], p: &Profile) -> Option<f32> {
    let mut v: Vec<f32> = ids
        .iter()
        .filter_map(|id| p.levels.get(id).map(|l| l.rms * p.gain))
        .collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(f32::total_cmp);
    Some(v[v.len() / 2])
}

/// Build the normalization tables over all loaded profiles. `norm` = shared
/// correction per sample id; `ceiling` = per-sample headroom bound used to
/// keep a press/release pair on one correction (both peaks bound it).
/// Port of `AudioController.load`'s registration pass.
fn derive_normalization(profiles: &[Profile], enabled: bool) -> (HashMap<u16, f32>, HashMap<u16, f32>) {
    let mut norm = HashMap::new();
    let mut ceiling = HashMap::new();
    // target = median per-profile press level (reference packs only),
    // clamped 0.04–0.12, fallback 0.06.
    let mut medians: Vec<f32> = profiles
        .iter()
        .filter(|p| p.normalization_reference && p.id != "silent")
        .filter_map(|p| median_level(&p.presses, p))
        .collect();
    medians.sort_by(f32::total_cmp);
    let target = if medians.is_empty() {
        0.06
    } else {
        medians[medians.len() / 2].clamp(0.04, 0.12)
    };
    for p in profiles {
        let silent = p.id == "silent";
        // Press lists share the global target; release lists normalize to
        // their own median (`target ?? median×calibration` in Clicky).
        let mut lists: Vec<(&[u16], Option<f32>)> =
            vec![(&p.presses, Some(target)), (&p.releases, None)];
        for ks in p.keys.values() {
            lists.push((&ks.presses, Some(target)));
            lists.push((&ks.releases, None));
        }
        for (ids, tgt) in lists {
            let Some(t) = tgt.or_else(|| median_level(ids, p)) else {
                continue;
            };
            for &id in ids {
                let Some(l) = p.levels.get(&id) else { continue };
                let headroom = if l.peak * p.gain > 0.0 {
                    0.65 / (l.peak * p.gain)
                } else {
                    f32::INFINITY
                };
                ceiling.insert(id, headroom);
                if enabled {
                    norm.insert(id, norm_gain(l.rms, l.peak, p.gain, t, silent));
                }
            }
        }
    }
    (norm, ceiling)
}

/// A resolved-but-unpaired trigger plus its normalization bookkeeping —
/// mirrors `ProfileTrigger` in the Swift port.
struct PTrig {
    trigger: Trigger,
    norm: f32,
    ceiling: f32,
}

/// Event→trigger resolution. Not RT-bound: allocations and HashMaps are fine
/// here; the mixer side stays lock-free.
pub struct Engine {
    config: AppConfiguration,
    profiles: Vec<Profile>,
    /// Active profile index (resolved from `sound.profile_id`, thocky fallback).
    active: usize,
    producer: TriggerProducer,
    /// Release intent chosen at press: `Some(trigger)` plays on release,
    /// `None` = silent release (muted/disabled press, dead resolution).
    releases: HashMap<(u32, u8, u16), Option<Trigger>>,
    /// Last variant sample id per selection key (never-repeat).
    last_variant: HashMap<String, u16>,
    /// Per-sample-id normalization correction (empty when disabled).
    norm: HashMap<u16, f32>,
    /// Per-sample-id peak headroom ceiling for pair bounding.
    ceiling: HashMap<u16, f32>,
    /// Headphone pan scale (0.55) vs speakers (1.0). Default: headphones.
    headphones: bool,
}

impl Engine {
    pub fn new(config: AppConfiguration, profiles: Vec<Profile>, producer: TriggerProducer) -> Engine {
        let (norm, ceiling) = derive_normalization(&profiles, config.sound.normalization);
        let active = active_index(&profiles, &config.sound.profile_id);
        Engine {
            config,
            profiles,
            active,
            producer,
            releases: HashMap::new(),
            last_variant: HashMap::new(),
            norm,
            ceiling,
            headphones: true,
        }
    }

    /// Select the active profile by index; out of range is ignored.
    pub fn set_profile(&mut self, idx: usize) {
        if idx < self.profiles.len() {
            self.active = idx;
        }
    }

    /// Swap the loaded profile set (manifest reload): rebuild normalization
    /// and re-resolve the configured profile. In-flight release intents are
    /// kept — they hold frozen triggers, never sample indexes to re-resolve.
    pub fn set_profiles(&mut self, profiles: Vec<Profile>) {
        let (norm, ceiling) = derive_normalization(&profiles, self.config.sound.normalization);
        self.norm = norm;
        self.ceiling = ceiling;
        self.active = active_index(&profiles, &self.config.sound.profile_id);
        self.profiles = profiles;
        self.last_variant.clear();
    }

    /// Id of the active profile (resolved, never stale like a config copy).
    pub fn profile_id(&self) -> &str {
        self.profiles
            .get(self.active)
            .map(|p| p.id.as_str())
            .unwrap_or("")
    }


    /// Apply a new configuration. Re-derives normalization when the toggle
    /// changed, re-resolves the active profile when `profile_id` changed, and
    /// drops release intents when the engine goes disabled (muted press must
    /// not resurrect a remembered release).
    pub fn update_config(&mut self, cfg: AppConfiguration) {
        if !cfg.enabled {
            self.releases.clear();
        }
        if cfg.sound.normalization != self.config.sound.normalization {
            let (n, c) = derive_normalization(&self.profiles, cfg.sound.normalization);
            self.norm = n;
            self.ceiling = c;
        }
        if cfg.sound.profile_id != self.config.sound.profile_id {
            self.active = active_index(&self.profiles, &cfg.sound.profile_id);
        }
        self.config = cfg;
    }

    /// Headphone output narrows the pan field (×0.55, Clicky's default).
    pub fn set_headphones(&mut self, headphones: bool) {
        self.headphones = headphones;
    }

    /// Feed a normalized key event.
    pub fn on_key(&mut self, ev: &KeyEvent) {
        let key = (ev.device, ev.page, ev.usage);
        if ev.phase == Phase::Release {
            // Lookup happens even while disabled so a remembered intent can
            // never leak through a re-enable; `None` recorded at press = silent.
            let entry = self.releases.remove(&key);
            if self.config.enabled {
                if let Some(Some(t)) = entry {
                    if self.gain_factor(ev) > 0.0 {
                        self.producer.push(t);
                    }
                }
            }
            return;
        }
        if ev.page == 9 {
            // Mouse buttons aren't in the v1 keyboard path (Clicky routes them
            // to a separate mouse sound). Press records a nil intent so the
            // release can never fire; the release branch above consumes it.
            self.releases.insert(key, None);
            return;
        }
        if !self.config.enabled {
            // Muted press → remembered nil → silent release.
            self.releases.insert(key, None);
            return;
        }
        // Modifier policy applies to the modifier's OWN sound only.
        let (mgain, mpitch) = self.modifier_policy(ev);
        if mgain <= 0.0 {
            self.releases.insert(key, None);
            return;
        }
        let over = self.config.key_overrides.get(ev.keyid);
        let s = &self.config.sound;
        let pidx = match over.and_then(|o| o.profile_id.as_deref()) {
            Some(id) => self
                .profiles
                .iter()
                .position(|p| p.id == id)
                .or_else(|| self.profiles.iter().position(|p| p.id == "thocky"))
                .unwrap_or(self.active),
            None => self.active,
        };
        if self.profiles.is_empty() {
            self.releases.insert(key, None);
            return;
        }
        let home = ev.page == 7 && HOME_ROW.contains(&ev.usage);
        let volume = over.and_then(|o| o.volume).unwrap_or(s.volume);
        let gain_pre = volume * if home { 1.0 - s.home_row_softness } else { 1.0 } * mgain;
        let pitch = over.and_then(|o| o.pitch).or(mpitch).unwrap_or(s.pitch);
        let tone = over.and_then(|o| o.tone).unwrap_or(s.tone);
        let pan = self.pan(ev.usage, ev.page);
        let press = self.profile_trigger(pidx, Phase::Press, ev.keyid, tone, pitch, gain_pre, pan, None);
        let shared = press.as_ref().map(|p| p.norm);
        let release =
            self.profile_trigger(pidx, Phase::Release, ev.keyid, tone, pitch, gain_pre, pan, shared);
        let (press, release) = self.pair(press, release);
        let remembered = match press {
            Some(p) => {
                let accepted = self.producer.push(p);
                match release {
                    Some(r) if accepted && p.gain > 0.0 && r.gain > 0.0 => Some(r),
                    _ => None,
                }
            }
            None => None,
        };
        self.releases.insert(key, remembered);
    }

    /// Audition a key immediately: enqueue the resolved press then the
    /// paired release (a real hold isn't needed for a preview). Previews
    /// the PROFILE DEFAULT, like Clicky — key overrides don't apply.
    pub fn preview(&mut self, keyid: &str) {
        if self.profiles.is_empty() {
            return;
        }
        let (tone, pitch, volume) = {
            let s = &self.config.sound;
            (s.tone, s.pitch, s.volume)
        };
        let press =
            self.profile_trigger(self.active, Phase::Press, keyid, tone, pitch, volume, 0.0, None);
        let shared = press.as_ref().map(|p| p.norm);
        let release =
            self.profile_trigger(self.active, Phase::Release, keyid, tone, pitch, volume, 0.0, shared);
        let (press, release) = self.pair(press, release);
        if let Some(p) = press {
            self.producer.push(p);
        }
        if let Some(r) = release {
            self.producer.push(r);
        }
    }

    /// Modifier gain/pitch for the modifier's own sound (224–231, L and R
    /// independent — the event's own usage decides).
    fn modifier_policy(&self, ev: &KeyEvent) -> (f32, Option<f32>) {
        if !(ev.page == 7 && MODIFIER_USAGES.contains(&ev.usage)) {
            return (1.0, None);
        }
        let s = &self.config.sound;
        match s.modifier_sound_mode {
            ModifierSoundMode::Soft => (0.25, None),
            ModifierSoundMode::Silent => (0.0, None),
            ModifierSoundMode::Full => (1.0, None),
            ModifierSoundMode::Custom => (s.modifier_custom_volume, Some(s.modifier_custom_pitch)),
        }
    }

    /// The gain check Clicky applies before playing a remembered release:
    /// typing-gain factors as they'd compute NOW (suppress when muted).
    fn gain_factor(&self, ev: &KeyEvent) -> f32 {
        let s = &self.config.sound;
        let over = self.config.key_overrides.get(ev.keyid);
        let volume = over.and_then(|o| o.volume).unwrap_or(s.volume);
        let home = ev.page == 7 && HOME_ROW.contains(&ev.usage);
        let (mgain, _) = self.modifier_policy(ev);
        volume * if home { 1.0 - s.home_row_softness } else { 1.0 } * mgain
    }

    /// Pan from layout geometry × spatialWidth × headphone factor.
    /// Usages outside the layout (and non-keyboard pages) center at 0.
    fn pan(&self, usage: u16, page: u8) -> f32 {
        let s = &self.config.sound;
        if page != 7 || !s.spatial {
            return 0.0;
        }
        let Some(&(_, x, w)) = LAYOUT.iter().find(|&&(u, _, _)| u == usage) else {
            return 0.0;
        };
        let position = ((x + w / 2.0) / 15.0) * 2.0 - 1.0;
        position * s.spatial_width * if self.headphones { 0.55 } else { 1.0 }
    }

    /// Resolve one phase of a stroke into a `PTrig` — `profileTrigger` port.
    /// `shared` carries the press's correction into the release (the pair
    /// shares one normalization bounded by both peaks).
    #[allow(clippy::too_many_arguments)]
    fn profile_trigger(
        &mut self,
        pidx: usize,
        phase: Phase,
        keyid: &str,
        tone: f32,
        pitch: f32,
        gain_pre: f32,
        pan: f32,
        shared: Option<f32>,
    ) -> Option<PTrig> {
        let p = &self.profiles[pidx];
        let ks = p.keys.get(keyid);
        // keySamples replaces the generic bank per-phase; an empty list wins
        // (port of `keySet ?? generic` on the whole set).
        let ids: &[u16] = match (phase, ks) {
            (Phase::Press, Some(k)) => &k.presses,
            (Phase::Press, None) => &p.presses,
            (Phase::Release, Some(k)) => &k.releases,
            (Phase::Release, None) => &p.releases,
        };
        if ids.is_empty() {
            return None;
        }
        let variation = self.config.sound.variation;
        // Selection key: profile : generic|keyid : phase — matches Swift's
        // `id + ":" + (keySet == nil ? "generic" : keyID) + ":" + phase`.
        let sel_key = format!(
            "{}:{}:{}",
            p.id,
            if ks.is_some() { keyid } else { "generic" },
            if phase == Phase::Press { "down" } else { "up" }
        );
        let prev = self
            .last_variant
            .get(&sel_key)
            .and_then(|id| ids.iter().position(|x| x == id));
        let idx = if variation && ids.len() > 1 {
            select_variant(ids.len(), prev)
        } else {
            0
        };
        let sample = ids[idx];
        self.last_variant.insert(sel_key, sample);
        let pitch_jitter = if variation { 0.975 + fastrand::f32() * 0.05 } else { 1.0 };
        let gain_jitter = if variation { 0.94 + fastrand::f32() * 0.06 } else { 1.0 };
        let own_norm = self.norm.get(&sample).copied().unwrap_or(1.0);
        let correction = if self.config.sound.normalization {
            shared.unwrap_or(own_norm)
        } else {
            1.0
        };
        let speed = (2f32.powf(pitch.clamp(-1.0, 1.0) * 0.5) * pitch_jitter).clamp(0.25, 4.0);
        Some(PTrig {
            trigger: Trigger {
                sample,
                gain: gain_pre * p.gain * correction * gain_jitter * PLAYBACK_CAL,
                pan,
                pitch: speed,
                tone,
            },
            norm: correction,
            ceiling: self.ceiling.get(&sample).copied().unwrap_or(f32::INFINITY),
        })
    }

    /// `pairedTriggers`: press/release share one correction bounded by both
    /// peaks; recorded release ×0.7. No press → no release either.
    fn pair(&self, press: Option<PTrig>, release: Option<PTrig>) -> (Option<Trigger>, Option<Trigger>) {
        let Some(mut p) = press else {
            return (None, None);
        };
        match release {
            Some(mut r) => {
                if self.config.sound.normalization {
                    let common = p.norm.min(p.ceiling).min(r.ceiling);
                    p.trigger.gain *= common / p.norm;
                    r.trigger.gain *= common / r.norm;
                }
                r.trigger.gain *= RELEASE_GAIN;
                (Some(p.trigger), Some(r.trigger))
            }
            None => (Some(p.trigger), None),
        }
    }
}

/// `banks[profileID] ?? banks["thocky"]`, else first profile.
fn active_index(profiles: &[Profile], want: &str) -> usize {
    profiles
        .iter()
        .position(|p| p.id == want)
        .or_else(|| profiles.iter().position(|p| p.id == "thocky"))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::config::{KeyOverride, ModifierSoundMode};
    use crate::mixer::Mixer;
    use crate::profiles::{KeySamples, Level};

    fn kev(keyid: &'static str, usage: u16, phase: Phase) -> KeyEvent {
        KeyEvent {
            device: 0,
            keyid,
            usage,
            page: 7,
            phase,
            modifiers: 0,
        }
    }

    fn register(mixer: &mut Mixer, amp: f32) -> u16 {
        let id = mixer.register(Arc::from(vec![amp; 480].into_boxed_slice()), 48000);
        assert_ne!(id, u16::MAX);
        id
    }

    /// Profile with `n_press`/`n_release` flat-tone samples at `amp`.
    fn profile(mixer: &mut Mixer, id: &str, amp: f32, n_press: usize, n_release: usize) -> Profile {
        let presses: Vec<u16> = (0..n_press).map(|_| register(mixer, amp)).collect();
        let releases: Vec<u16> = (0..n_release).map(|_| register(mixer, amp)).collect();
        let level = Level { rms: amp, peak: amp };
        let levels = presses
            .iter()
            .chain(releases.iter())
            .map(|&i| (i, level))
            .collect();
        Profile {
            id: id.into(),
            name: id.into(),
            brand: None,
            subtitle: String::new(),
            color: String::new(),
            gain: 1.0,
            normalization_reference: true,
            presses,
            releases,
            keys: HashMap::new(),
            levels,
            provenance: None,
            warnings: Vec::new(),
        }
    }

    /// Deterministic tuning: normalization + variation off →
    /// gain = volume × profile.gain × 2 exactly.
    fn raw_cfg() -> AppConfiguration {
        let mut c = AppConfiguration::default();
        c.sound.normalization = false;
        c.sound.variation = false;
        c
    }

    #[test]
    fn press_enqueues_expected_gain() {
        let mut mixer = Mixer::new(48000);
        let p = profile(&mut mixer, "thocky", 0.3, 2, 1);
        let sample = p.presses[0];
        let (mut eng, mut mx) = {
            let producer = mixer.producer().unwrap();
            (Engine::new(raw_cfg(), vec![p], producer), mixer)
        };
        eng.on_key(&kev("7:30", 30, Phase::Press));
        let t = mx.drain();
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].sample, sample);
        // volume 0.4 × gain 1.0 × 2 = 0.8
        assert!((t[0].gain - 0.8).abs() < 1e-6, "gain {}", t[0].gain);
        assert_eq!(t[0].pitch, 1.0);
    }

    #[test]
    fn gain_bounds_with_norm_and_variation() {
        // Real settings: norm + variation on — gain stays in a sane band.
        let mut mixer = Mixer::new(48000);
        let p = profile(&mut mixer, "thocky", 0.3, 2, 1);
        let producer = mixer.producer().unwrap();
        let mut eng = Engine::new(AppConfiguration::default().validated(), vec![p], producer);
        for _ in 0..50 {
            eng.on_key(&kev("7:30", 30, Phase::Press));
        }
        let t = mixer.drain();
        assert_eq!(t.len(), 50);
        for g in t.iter().map(|t| t.gain) {
            assert!(g > 0.1 && g < 1.2, "gain {g} out of band");
        }
    }

    #[test]
    fn modifier_soft_quarter_gain() {
        let mut mixer = Mixer::new(48000);
        let p = profile(&mut mixer, "thocky", 0.3, 1, 1);
        let producer = mixer.producer().unwrap();
        let mut eng = Engine::new(raw_cfg(), vec![p], producer);
        eng.on_key(&kev("7:224", 224, Phase::Press));
        let t = mixer.drain();
        assert_eq!(t.len(), 1);
        assert!((t[0].gain - 0.8 * 0.25).abs() < 1e-6, "gain {}", t[0].gain);
    }

    #[test]
    fn modifier_silent_press_and_release() {
        let mut mixer = Mixer::new(48000);
        let p = profile(&mut mixer, "thocky", 0.3, 1, 1);
        let mut cfg = raw_cfg();
        cfg.sound.modifier_sound_mode = ModifierSoundMode::Silent;
        let producer = mixer.producer().unwrap();
        let mut eng = Engine::new(cfg, vec![p], producer);
        eng.on_key(&kev("7:229", 229, Phase::Press));
        eng.on_key(&kev("7:229", 229, Phase::Release));
        assert!(mixer.drain().is_empty());
    }

    #[test]
    fn modifier_custom_volume_and_pitch() {
        let mut mixer = Mixer::new(48000);
        let p = profile(&mut mixer, "thocky", 0.3, 1, 1);
        let mut cfg = raw_cfg();
        cfg.sound.modifier_sound_mode = ModifierSoundMode::Custom;
        cfg.sound.modifier_custom_volume = 0.5;
        cfg.sound.modifier_custom_pitch = 0.5;
        let producer = mixer.producer().unwrap();
        let mut eng = Engine::new(cfg, vec![p], producer);
        eng.on_key(&kev("7:224", 224, Phase::Press));
        eng.on_key(&kev("7:30", 30, Phase::Press));
        let t = mixer.drain();
        assert_eq!(t.len(), 2);
        assert!((t[0].gain - 0.8 * 0.5).abs() < 1e-6, "mod gain {}", t[0].gain);
        assert!((t[0].pitch - 2f32.powf(0.25)).abs() < 1e-5, "mod pitch {}", t[0].pitch);
        // Non-modifier key unaffected by custom modifier pitch.
        assert_eq!(t[1].pitch, 1.0);
    }

    #[test]
    fn release_is_point_seven_of_press() {
        let mut mixer = Mixer::new(48000);
        let p = profile(&mut mixer, "thocky", 0.3, 1, 1);
        let rel = p.releases[0];
        let producer = mixer.producer().unwrap();
        let mut eng = Engine::new(raw_cfg(), vec![p], producer);
        eng.on_key(&kev("7:30", 30, Phase::Press));
        eng.on_key(&kev("7:30", 30, Phase::Release));
        let t = mixer.drain();
        assert_eq!(t.len(), 2);
        assert_eq!(t[1].sample, rel);
        assert!((t[1].gain - t[0].gain * 0.7).abs() < 1e-6, "rel {}", t[1].gain);
    }

    #[test]
    fn press_only_profile_release_silent() {
        let mut mixer = Mixer::new(48000);
        let p = profile(&mut mixer, "thocky", 0.3, 1, 0);
        let producer = mixer.producer().unwrap();
        let mut eng = Engine::new(raw_cfg(), vec![p], producer);
        eng.on_key(&kev("7:30", 30, Phase::Press));
        eng.on_key(&kev("7:30", 30, Phase::Release));
        assert_eq!(mixer.drain().len(), 1);
    }

    #[test]
    fn override_volume_stacks() {
        let mut mixer = Mixer::new(48000);
        let p = profile(&mut mixer, "thocky", 0.3, 1, 1);
        let mut cfg = raw_cfg();
        cfg.key_overrides.insert(
            "7:30".into(),
            KeyOverride {
                volume: Some(0.5),
                ..Default::default()
            },
        );
        let producer = mixer.producer().unwrap();
        let mut eng = Engine::new(cfg, vec![p], producer);
        eng.on_key(&kev("7:30", 30, Phase::Press)); // overridden → 0.5×1×2 = 1.0
        eng.on_key(&kev("7:31", 31, Phase::Press)); // default → 0.8
        let t = mixer.drain();
        assert_eq!(t.len(), 2);
        assert!((t[0].gain - 1.0).abs() < 1e-6, "ovr {}", t[0].gain);
        assert!((t[1].gain - 0.8).abs() < 1e-6, "def {}", t[1].gain);
    }

    #[test]
    fn override_profile_id_routes_samples() {
        let mut mixer = Mixer::new(48000);
        let p0 = profile(&mut mixer, "thocky", 0.3, 1, 1);
        let p1 = profile(&mut mixer, "alps", 0.3, 1, 1);
        let other_sample = p1.presses[0];
        let mut cfg = raw_cfg();
        cfg.key_overrides.insert(
            "7:30".into(),
            KeyOverride {
                profile_id: Some("alps".into()),
                ..Default::default()
            },
        );
        let producer = mixer.producer().unwrap();
        let mut eng = Engine::new(cfg, vec![p0, p1], producer);
        eng.on_key(&kev("7:30", 30, Phase::Press));
        eng.on_key(&kev("7:31", 31, Phase::Press));
        let t = mixer.drain();
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].sample, other_sample);
        assert_ne!(t[1].sample, other_sample);
    }

    #[test]
    fn key_samples_bank_overrides_generic() {
        let mut mixer = Mixer::new(48000);
        let mut p = profile(&mut mixer, "thocky", 0.3, 1, 1);
        let kp = register(&mut mixer, 0.3);
        let kr = register(&mut mixer, 0.3);
        p.keys.insert(
            "7:44".into(),
            KeySamples {
                presses: vec![kp],
                releases: vec![kr],
            },
        );
        p.levels.insert(kp, Level { rms: 0.3, peak: 0.3 });
        p.levels.insert(kr, Level { rms: 0.3, peak: 0.3 });
        let producer = mixer.producer().unwrap();
        let mut eng = Engine::new(raw_cfg(), vec![p], producer);
        eng.on_key(&kev("7:44", 44, Phase::Press));
        eng.on_key(&kev("7:44", 44, Phase::Release));
        let t = mixer.drain();
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].sample, kp);
        assert_eq!(t[1].sample, kr);
    }

    #[test]
    fn variant_never_repeats_consecutively() {
        let mut mixer = Mixer::new(48000);
        let p = profile(&mut mixer, "thocky", 0.3, 3, 0);
        let producer = mixer.producer().unwrap();
        let mut cfg = AppConfiguration::default();
        cfg.sound.normalization = false; // variation stays on
        let mut eng = Engine::new(cfg.validated(), vec![p], producer);
        for _ in 0..200 {
            eng.on_key(&kev("7:30", 30, Phase::Press));
        }
        let t = mixer.drain();
        assert_eq!(t.len(), 200);
        let distinct: std::collections::HashSet<u16> = t.iter().map(|t| t.sample).collect();
        assert!(distinct.len() > 1, "single variant used");
        for w in t.windows(2) {
            assert_ne!(w[0].sample, w[1].sample, "variant repeated consecutively");
        }
    }

    #[test]
    fn disabled_press_records_silent_release() {
        let mut mixer = Mixer::new(48000);
        let p = profile(&mut mixer, "thocky", 0.3, 1, 1);
        let mut cfg = raw_cfg();
        cfg.enabled = false;
        let producer = mixer.producer().unwrap();
        let mut eng = Engine::new(cfg, vec![p], producer);
        eng.on_key(&kev("7:30", 30, Phase::Press));
        // Re-enable before release: the remembered nil keeps it silent.
        let mut on = raw_cfg();
        on.enabled = true;
        eng.update_config(on);
        eng.on_key(&kev("7:30", 30, Phase::Release));
        assert!(mixer.drain().is_empty());
    }

    #[test]
    fn disabled_mid_hold_suppresses_release() {
        let mut mixer = Mixer::new(48000);
        let p = profile(&mut mixer, "thocky", 0.3, 1, 1);
        let producer = mixer.producer().unwrap();
        let mut eng = Engine::new(raw_cfg(), vec![p], producer);
        eng.on_key(&kev("7:30", 30, Phase::Press));
        let mut off = raw_cfg();
        off.enabled = false;
        eng.update_config(off);
        eng.on_key(&kev("7:30", 30, Phase::Release));
        let t = mixer.drain();
        assert_eq!(t.len(), 1); // press only
    }

    #[test]
    fn release_locked_at_press_across_profile_switch() {
        let mut mixer = Mixer::new(48000);
        let p0 = profile(&mut mixer, "thocky", 0.3, 1, 1);
        let rel0 = p0.releases[0];
        let p1 = profile(&mut mixer, "alps", 0.3, 1, 1);
        let producer = mixer.producer().unwrap();
        let mut eng = Engine::new(raw_cfg(), vec![p0, p1], producer);
        eng.on_key(&kev("7:30", 30, Phase::Press));
        eng.set_profile(1); // switch mid-hold
        eng.on_key(&kev("7:30", 30, Phase::Release));
        let t = mixer.drain();
        assert_eq!(t.len(), 2);
        assert_eq!(t[1].sample, rel0, "release crossed to new profile");
    }

    #[test]
    fn release_is_per_device() {
        let mut mixer = Mixer::new(48000);
        let p = profile(&mut mixer, "thocky", 0.3, 1, 1);
        let producer = mixer.producer().unwrap();
        let mut eng = Engine::new(raw_cfg(), vec![p], producer);
        eng.on_key(&kev("7:30", 30, Phase::Press));
        let mut other = kev("7:30", 30, Phase::Release);
        other.device = 1;
        eng.on_key(&other);
        let t = mixer.drain();
        assert_eq!(t.len(), 1); // release on another device → nothing
    }

    #[test]
    fn pan_signs_left_and_right() {
        let mut mixer = Mixer::new(48000);
        let p = profile(&mut mixer, "thocky", 0.3, 1, 0);
        let producer = mixer.producer().unwrap();
        let mut eng = Engine::new(raw_cfg(), vec![p], producer);
        eng.on_key(&kev("7:30", 30, Phase::Press)); // "1", x=1 → pos −0.8
        eng.on_key(&kev("7:39", 39, Phase::Press)); // "0", x=10 → pos +0.4
        eng.on_key(&kev("7:999", 99, Phase::Press)); // not in layout → 0
        let t = mixer.drain();
        assert_eq!(t.len(), 3);
        assert!(t[0].pan < 0.0, "left pan {}", t[0].pan);
        assert!(t[1].pan > 0.0, "right pan {}", t[1].pan);
        assert_eq!(t[2].pan, 0.0);
        // ((1.5)/15×2−1)×0.8×0.55 = −0.352
        assert!((t[0].pan - (-0.352)).abs() < 1e-4, "pan {}", t[0].pan);
    }

    #[test]
    fn home_row_softened() {
        let mut mixer = Mixer::new(48000);
        let p = profile(&mut mixer, "thocky", 0.3, 1, 0);
        let producer = mixer.producer().unwrap();
        let mut eng = Engine::new(raw_cfg(), vec![p], producer);
        eng.on_key(&kev("7:4", 4, Phase::Press)); // home row (A)
        eng.on_key(&kev("7:10", 10, Phase::Press)); // G — not in home set
        let t = mixer.drain();
        assert!((t[0].gain - 0.8 * 0.85).abs() < 1e-6, "home {}", t[0].gain);
        assert!((t[1].gain - 0.8).abs() < 1e-6, "non-home {}", t[1].gain);
    }

    #[test]
    fn preview_enqueues_pair() {
        let mut mixer = Mixer::new(48000);
        let p = profile(&mut mixer, "thocky", 0.3, 1, 1);
        let producer = mixer.producer().unwrap();
        let mut eng = Engine::new(raw_cfg(), vec![p], producer);
        eng.preview("7:30");
        let t = mixer.drain();
        assert_eq!(t.len(), 2);
        assert!((t[1].gain - t[0].gain * 0.7).abs() < 1e-6);
        // Preview doesn't touch the release tracker.
        eng.on_key(&kev("7:30", 30, Phase::Release));
        assert!(mixer.drain().is_empty());
    }

    #[test]
    fn norm_gain_upper_below_floor_no_panic() {
        // peak×gain = 2.0 → headroom 0.325 < 0.5 floor. Rust clamp panics;
        // Swift clamped(to:) yields upper. Regression: hot sample + hot gain.
        let g = norm_gain(0.5, 1.0, 2.0, 0.06, false);
        assert!((g - 0.325).abs() < 1e-3, "norm {g}");
        // End-to-end: such a profile resolves without panic.
        let mut mixer = Mixer::new(48000);
        let mut p = profile(&mut mixer, "thocky", 0.3, 1, 1);
        let hot = register(&mut mixer, 1.0);
        p.presses = vec![hot];
        p.gain = 2.0;
        p.levels.insert(hot, Level { rms: 1.0, peak: 1.0 });
        let producer = mixer.producer().unwrap();
        let mut eng = Engine::new(AppConfiguration::default().validated(), vec![p], producer);
        eng.on_key(&kev("7:30", 30, Phase::Press));
        let t = mixer.drain();
        assert_eq!(t.len(), 1);
        assert!(t[0].gain.is_finite() && t[0].gain > 0.0);
    }

    #[test]
    fn mouse_button_events_are_silent() {
        let mut mixer = Mixer::new(48000);
        let p = profile(&mut mixer, "thocky", 0.3, 1, 1);
        let producer = mixer.producer().unwrap();
        let mut eng = Engine::new(raw_cfg(), vec![p], producer);
        let mut ev = kev("9:1", 1, Phase::Press);
        ev.page = 9;
        eng.on_key(&ev);
        ev.phase = Phase::Release;
        eng.on_key(&ev);
        assert!(mixer.drain().is_empty(), "page-9 event produced triggers");
    }

    #[test]
    fn preview_ignores_key_overrides() {
        let mut mixer = Mixer::new(48000);
        let p0 = profile(&mut mixer, "thocky", 0.3, 1, 1);
        let p1 = profile(&mut mixer, "alps", 0.3, 1, 1);
        let thocky_press = p0.presses[0];
        let mut cfg = raw_cfg();
        cfg.key_overrides.insert(
            "7:30".into(),
            KeyOverride {
                profile_id: Some("alps".into()),
                volume: Some(0.9),
                pitch: Some(-1.0),
                tone: Some(0.9),
            },
        );
        let producer = mixer.producer().unwrap();
        let mut eng = Engine::new(cfg.validated(), vec![p0, p1], producer);
        eng.preview("7:30");
        let t = mixer.drain();
        assert_eq!(t.len(), 2);
        // Clicky previews the profile default: active profile's sample,
        // slider volume/pitch/tone — none of the override values.
        assert_eq!(t[0].sample, thocky_press);
        assert!((t[0].gain - 0.8).abs() < 1e-6, "preview gain {}", t[0].gain);
        assert_eq!(t[0].pitch, 1.0);
        assert_eq!(t[0].tone, 0.0);
    }

    #[test]
    fn renders_audio_offline() {
        // Smoke: a press through the real mixer produces non-silent output.
        let mut mixer = Mixer::new(48000);
        let p = profile(&mut mixer, "thocky", 0.5, 1, 0);
        let producer = mixer.producer().unwrap();
        let mut eng = Engine::new(raw_cfg(), vec![p], producer);
        eng.on_key(&kev("7:30", 30, Phase::Press));
        let mut out = vec![0.0f32; 480 * 2];
        mixer.render(&mut out, 2);
        let rms: f32 = (out.iter().map(|s| s * s).sum::<f32>() / out.len() as f32).sqrt();
        assert!(rms > 0.01, "rendered rms {rms}");
    }
}
