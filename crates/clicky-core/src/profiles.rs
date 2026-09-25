//! `profiles.json` manifest loading — Clicky schema, verbatim.
//!
//! The manifest lives at `sounds_root/profiles.json`. Sample paths inside it
//! are Clicky-style (`Sounds/<id>/x.wav`); our layout is `<id>/x.wav` under
//! the sounds root — `resolve` tries the literal path first, then strips a
//! leading `Sounds/` component.
//!
//! Every sample is decoded via symphonia (wav/mp3/flac/ogg — whatever the
//! enabled codecs cover), mixed down to mono f32, and registered into the
//! [`Mixer`] at its source rate (the mixer resamples per-voice). Samples over
//! [`MAX_SECS`] are rejected. A bad profile is collected into
//! [`LoadReport::errors`] and excluded — it never fails the whole load, and
//! its samples are never registered (two-phase load: decode all, then
//! register all — no orphaned registry slots).

use std::collections::HashMap;
use std::fs::File;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::mixer::Mixer;

/// Sample duration limit, seconds.
const MAX_SECS: f64 = 15.0;

/// Load failures: missing/unreadable files, decode errors, over-length
/// samples, full registry. All variants name the offending path.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("cannot read {0}: {1}")]
    Read(PathBuf, #[source] std::io::Error),
    #[error("cannot parse {0}: {1}")]
    Parse(PathBuf, #[source] serde_json::Error),
    #[error("{0}: {1}")]
    Decode(PathBuf, #[source] SymError),
    #[error("{0}: no default audio track")]
    NoTrack(PathBuf),
    #[error("{0}: decodes to empty PCM")]
    Empty(PathBuf),
    #[error("{0}: {1:.1}s exceeds the 15s limit")]
    TooLong(PathBuf, f64),
    #[error("{0}: mixer sample registry full or sample unusable")]
    Registry(PathBuf),
}

/// A loaded sound profile. `presses`/`releases`/`keys` hold mixer sample ids
/// (see [`Mixer::register`]), not file paths. `levels` carries per-sample
/// rms/peak measured at load for the engine's normalization table.
#[derive(Debug)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub brand: Option<String>,
    pub subtitle: String,
    pub color: String,
    pub gain: f32,
    pub normalization_reference: bool,
    /// Press-sample ids, from `samples`.
    pub presses: Vec<u16>,
    /// Release-sample ids, from `releaseSamples` (nullable → empty).
    pub releases: Vec<u16>,
    /// Per-key overrides keyed by the HID keyid string verbatim ("7:44").
    pub keys: HashMap<String, KeySamples>,
    /// Per-sample rms/peak measured at decode, keyed by mixer sample id —
    /// feeds the engine's normalization table.
    pub levels: HashMap<u16, Level>,
    /// Optional provenance block, passed through untouched.
    pub provenance: Option<serde_json::Value>,
    /// Non-fatal load/import warnings (e.g. unknown pack key names dropped).
    /// Always empty for `profiles.json`-loaded profiles.
    pub warnings: Vec<String>,
}

/// Mixer sample ids for one keyid's press/release override.
#[derive(Debug, Default)]
pub struct KeySamples {
    pub presses: Vec<u16>,
    pub releases: Vec<u16>,
}

/// Load-time measurement of one registered sample (mono PCM).
#[derive(Debug, Clone, Copy)]
pub struct Level {
    pub rms: f32,
    pub peak: f32,
}

/// Result of [`load_manifest`]: the profiles that loaded plus one error per
/// excluded entry (keyed by profile id, or `"<index N>"` when the entry had
/// no readable id).
#[derive(Debug, Default)]
pub struct LoadReport {
    pub profiles: Vec<Profile>,
    pub errors: Vec<(String, Error)>,
}

fn def_gain() -> f32 {
    1.0
}
fn def_true() -> bool {
    true
}

/// Raw manifest entry — Clicky `profiles.json` schema verbatim.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawProfile {
    id: String,
    name: String,
    brand: Option<String>,
    subtitle: String,
    color: String,
    samples: Vec<String>,
    release_samples: Option<Vec<String>>,
    /// Nullable in the wild — missing or null both mean "no overrides".
    #[serde(default)]
    key_samples: Option<HashMap<String, RawKeySamples>>,
    #[serde(default = "def_gain")]
    gain: f32,
    provenance: Option<serde_json::Value>,
    #[serde(default = "def_true")]
    normalization_reference: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawKeySamples {
    samples: Vec<String>,
    release_samples: Option<Vec<String>>,
}

/// Read `dir/profiles.json` and decode every entry — touches no mixer state,
/// so callers holding a shared mixer (`Audio::mixer()`) can do the slow
/// decode OUTSIDE the lock. Pair with [`register_manifest`], which only needs
/// the mixer for the append loop. `load_manifest` = stage + register.
///
/// Manifest-level failures (file unreadable, top-level JSON not an array) are
/// fatal `Err`; per-entry decode failures land in `errors` and that profile
/// is excluded.
pub fn stage_manifest(dir: &Path) -> Result<StagedManifest, Error> {
    let path = dir.join("profiles.json");
    let text = std::fs::read(&path).map_err(|e| Error::Read(path.clone(), e))?;
    let entries: Vec<serde_json::Value> =
        serde_json::from_slice(&text).map_err(|e| Error::Parse(path.clone(), e))?;

    let mut staged = Vec::new();
    let mut errors = Vec::new();
    for (i, entry) in entries.into_iter().enumerate() {
        let id = entry
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("<index {i}>"));
        let raw: RawProfile = match serde_json::from_value(entry) {
            Ok(r) => r,
            Err(e) => {
                // Name manifest + entry id distinctly: logs stay greppable.
                errors.push((id.clone(), Error::Parse(path.join(format!("entry[{id}]")), e)));
                continue;
            }
        };
        match stage_profile(dir, raw) {
            Ok(p) => staged.push(p),
            Err(e) => errors.push((id, e)),
        }
    }
    Ok(StagedManifest { dir: dir.to_path_buf(), staged, errors })
}

/// Register every profile staged by [`stage_manifest`], producing the same
/// [`LoadReport`] shape as [`load_manifest`]. This is the only mixer-touching
/// half — hold the shared mixer lock just for this call (ms, not seconds).
pub fn register_manifest(staged: StagedManifest, mixer: &mut Mixer) -> LoadReport {
    let mut report = LoadReport { profiles: Vec::new(), errors: staged.errors };
    for p in staged.staged {
        let id = p.raw.id.clone();
        match register_profile(&staged.dir, p, mixer) {
            Ok(p) => report.profiles.push(p),
            Err(e) => report.errors.push((id, e)),
        }
    }
    report
}

/// Read `dir/profiles.json` and resolve every entry: `stage_manifest` +
/// `register_manifest` in one call (fine at startup, before the stream runs
/// and the mixer lock is uncontended).
pub fn load_manifest(dir: &Path, mixer: &mut Mixer) -> Result<LoadReport, Error> {
    Ok(register_manifest(stage_manifest(dir)?, mixer))
}

/// Manifest decoded to PCM but not yet registered — the lock-free half of a
/// [`load_manifest`]. `errors` carries entries that failed decode/parse.
pub struct StagedManifest {
    dir: PathBuf,
    staged: Vec<StagedProfile>,
    errors: Vec<(String, Error)>,
}

/// One profile decoded to PCM but not yet registered.
struct StagedProfile {
    raw: RawProfile,
    presses: Staged,
    releases: Staged,
    keys: Vec<(String, Staged, Staged)>,
}

/// Decoded-but-unregistered sample: `(resolved path, mono pcm, src_rate)`.
pub(crate) type Staged = Vec<(PathBuf, Arc<[f32]>, u32)>;

/// Decode + validate every file of one profile — no mixer touched. All
/// failures happen before a single `register` call, so a failed profile
/// leaves zero orphaned slots in the registry.
fn stage_profile(dir: &Path, mut raw: RawProfile) -> Result<StagedProfile, Error> {
    let presses = stage_list(dir, &raw.samples)?;
    let releases = match &raw.release_samples {
        Some(paths) => stage_list(dir, paths)?,
        None => Vec::new(),
    };
    let mut keys = Vec::with_capacity(raw.key_samples.as_ref().map_or(0, HashMap::len));
    for (keyid, ks) in raw.key_samples.take().unwrap_or_default() {
        keys.push((
            keyid,
            stage_list(dir, &ks.samples)?,
            match &ks.release_samples {
                Some(paths) => stage_list(dir, paths)?,
                None => Vec::new(),
            },
        ));
    }
    Ok(StagedProfile { raw, presses, releases, keys })
}

/// Capacity pre-check + register a staged profile. Call under the mixer lock;
/// it holds it only for the append loop.
fn register_profile(dir: &Path, staged: StagedProfile, mixer: &mut Mixer) -> Result<Profile, Error> {
    let StagedProfile { raw, presses, releases, keys } = staged;
    // Capacity pre-check: fail before any register() call so a too-big
    // profile can't orphan slots already claimed for it.
    let needed = presses.len()
        + releases.len()
        + keys.iter().map(|(_, p, r)| p.len() + r.len()).sum::<usize>();
    if mixer.registered_count() + needed > crate::mixer::MAX_SAMPLES {
        return Err(Error::Registry(dir.join(&raw.id)));
    }
    let mut levels = HashMap::with_capacity(needed);
    let presses = register_list(presses, mixer)?;
    levels.extend(presses.iter().copied());
    let presses = presses.into_iter().map(|(id, _)| id).collect();
    let releases = register_list(releases, mixer)?;
    levels.extend(releases.iter().copied());
    let releases = releases.into_iter().map(|(id, _)| id).collect();
    let keys = keys
        .into_iter()
        .map(|(keyid, p, r)| {
            let p = register_list(p, mixer)?;
            let r = register_list(r, mixer)?;
            levels.extend(p.iter().chain(r.iter()).copied());
            Ok((
                keyid,
                KeySamples {
                    presses: p.into_iter().map(|(id, _)| id).collect(),
                    releases: r.into_iter().map(|(id, _)| id).collect(),
                },
            ))
        })
        .collect::<Result<HashMap<_, _>, Error>>()?;
    Ok(Profile {
        id: raw.id,
        name: raw.name,
        brand: raw.brand,
        subtitle: raw.subtitle,
        color: raw.color,
        gain: raw.gain,
        normalization_reference: raw.normalization_reference,
        presses,
        releases,
        keys,
        levels,
        provenance: raw.provenance,
        warnings: Vec::new(),
    })
}

/// Resolve + decode a list of sample paths; any failure aborts the profile
/// before anything is registered.
pub(crate) fn stage_list(dir: &Path, paths: &[String]) -> Result<Staged, Error> {
    paths
        .iter()
        .map(|rel| {
            let path = resolve(dir, rel);
            let (pcm, rate) = decode(&path)?;
            Ok((path, pcm, rate))
        })
        .collect()
}

/// Register staged PCM into the mixer; returns `(id, measured level)` pairs
/// (rms/peak computed over the mono PCM, consumed by engine normalization).
pub(crate) fn register_list(staged: Staged, mixer: &mut Mixer) -> Result<Vec<(u16, Level)>, Error> {
    staged
        .into_iter()
        .map(|(path, pcm, rate)| {
            let mut sum = 0.0f32;
            let mut peak = 0.0f32;
            for &s in pcm.iter() {
                sum += s * s;
                peak = peak.max(s.abs());
            }
            let level = Level {
                rms: (sum / pcm.len().max(1) as f32).sqrt(),
                peak,
            };
            match mixer.register(pcm, rate) {
                u16::MAX => Err(Error::Registry(path)),
                id => Ok((id, level)),
            }
        })
        .collect()
}

/// `dir/rel` if it exists, else `dir/rel` with a leading `Sounds/` component
/// stripped. Falls back to the literal path so errors name the manifest's
/// original path.
fn resolve(dir: &Path, rel: &str) -> PathBuf {
    let literal = dir.join(rel);
    if literal.exists() {
        return literal;
    }
    let mut comps = Path::new(rel).components();
    if let Some(Component::Normal(first)) = comps.next() {
        if first.to_str().is_some_and(|s| s.eq_ignore_ascii_case("sounds")) {
            let stripped = dir.join(comps.as_path());
            if stripped.exists() {
                return stripped;
            }
        }
    }
    literal
}

/// Decode `path` to mono f32 via symphonia; returns `(pcm, src_rate)`.
/// Multi-channel input is averaged down; unreadable, trackless, empty, and
/// >[`MAX_SECS`] files are rejected.
fn decode(path: &Path) -> Result<(Arc<[f32]>, u32), Error> {
    let file = File::open(path).map_err(|e| Error::Read(path.to_path_buf(), e))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| Error::Decode(path.to_path_buf(), e))?;
    let mut format = probed.format;
    let track = format
        .default_track()
        .ok_or_else(|| Error::NoTrack(path.to_path_buf()))?;
    let track_id = track.id;
    // Cheap pre-check: reject over-length files from the declared duration
    // before burning decode CPU/RAM. The post-decode frame count below stays
    // the authoritative guard for formats that lie.
    if let (Some(n), Some(tb)) = (track.codec_params.n_frames, track.codec_params.time_base) {
        let t = tb.calc_time(n);
        let secs = t.seconds as f64 + t.frac;
        if secs > MAX_SECS {
            return Err(Error::TooLong(path.to_path_buf(), secs));
        }
    }
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| Error::Decode(path.to_path_buf(), e))?;

    let mut pcm: Vec<f32> = Vec::new();
    let mut rate = 0u32;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            // EOF and demuxer resets both end the stream for our purposes.
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymError::ResetRequired) => break,
            Err(e) => return Err(Error::Decode(path.to_path_buf(), e)),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let buf = match decoder.decode(&packet) {
            Ok(b) => b,
            Err(SymError::DecodeError(_)) => continue, // skip corrupt packets
            Err(e) => return Err(Error::Decode(path.to_path_buf(), e)),
        };
        let spec = *buf.spec();
        rate = spec.rate;
        let channels = spec.channels.count().max(1);
        let mut mono = SampleBuffer::<f32>::new(buf.capacity() as u64, spec);
        mono.copy_interleaved_ref(buf);
        for frame in mono.samples().chunks(channels) {
            pcm.push(frame.iter().sum::<f32>() / channels as f32);
        }
    }

    if rate == 0 || pcm.is_empty() {
        return Err(Error::Empty(path.to_path_buf()));
    }
    let secs = pcm.len() as f64 / f64::from(rate);
    if secs > MAX_SECS {
        return Err(Error::TooLong(path.to_path_buf(), secs));
    }
    Ok((Arc::from(pcm.into_boxed_slice()), rate))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal PCM WAV writer — enough for symphonia to probe.
    fn write_wav(path: &Path, rate: u32, channels: u16, samples: &[i16]) {
        let data = (samples.len() * 2) as u32;
        let mut w = Vec::with_capacity(44 + data as usize);
        w.extend_from_slice(b"RIFF");
        w.extend_from_slice(&(36 + data).to_le_bytes());
        w.extend_from_slice(b"WAVEfmt ");
        w.extend_from_slice(&16u32.to_le_bytes());
        w.extend_from_slice(&1u16.to_le_bytes()); // PCM
        w.extend_from_slice(&channels.to_le_bytes());
        w.extend_from_slice(&rate.to_le_bytes());
        w.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
        w.extend_from_slice(&(channels * 2).to_le_bytes());
        w.extend_from_slice(&16u16.to_le_bytes());
        w.extend_from_slice(b"data");
        w.extend_from_slice(&data.to_le_bytes());
        for s in samples {
            w.extend_from_slice(&s.to_le_bytes());
        }
        std::fs::write(path, w).unwrap();
    }

    struct TestDir(PathBuf);
    impl TestDir {
        fn new(name: &str) -> Self {
            let p = std::env::temp_dir().join(format!("clicky-profiles-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn wav(dir: &Path, name: &str, n: usize) {
        write_wav(&dir.join(name), 48000, 1, &vec![1000i16; n]);
    }

    /// Registry watermark: id the next registered sample would get.
    fn registry_len(mixer: &mut Mixer) -> u16 {
        let id = mixer.register(Arc::from(vec![0.0f32; 8].into_boxed_slice()), 48000);
        assert_ne!(id, u16::MAX);
        id // ids are sequential → this equals count before this register
    }

    #[test]
    fn parses_manifest_and_registers_samples() {
        let d = TestDir::new("full");
        std::fs::create_dir_all(d.0.join("alps")).unwrap();
        for f in ["p1.wav", "p2.wav", "r1.wav", "sp.wav", "sr.wav"] {
            wav(&d.0.join("alps"), f, 480);
        }
        std::fs::write(
            d.0.join("profiles.json"),
            r#"[{
                "id": "alps", "name": "Alps", "brand": "Alps",
                "subtitle": "tplai", "color": "D3BB6F",
                "samples": ["Sounds/alps/p1.wav", "Sounds/alps/p2.wav"],
                "releaseSamples": ["Sounds/alps/r1.wav"],
                "keySamples": {"7:44": {
                    "samples": ["Sounds/alps/sp.wav"],
                    "releaseSamples": ["Sounds/alps/sr.wav"]
                }},
                "gain": 0.8, "normalizationReference": false,
                "provenance": {"author": "tplai"}
            }]"#,
        )
        .unwrap();

        let mut mixer = Mixer::new(48000);
        let report = load_manifest(&d.0, &mut mixer).unwrap();
        assert!(report.errors.is_empty());
        assert_eq!(report.profiles.len(), 1);
        let p = &report.profiles[0];
        assert_eq!(p.id, "alps");
        assert_eq!(p.brand.as_deref(), Some("Alps"));
        assert_eq!(p.presses.len(), 2);
        assert_eq!(p.releases.len(), 1);
        assert!(!p.normalization_reference);
        assert_eq!(p.provenance.as_ref().unwrap()["author"], "tplai");
        let ks = &p.keys["7:44"];
        assert_eq!(ks.presses.len(), 1);
        assert_eq!(ks.releases.len(), 1);
        // All ids distinct, registered, below MAX.
        let mut ids: Vec<u16> = p
            .presses
            .iter()
            .chain(&p.releases)
            .chain(&ks.presses)
            .chain(&ks.releases)
            .copied()
            .collect();
        assert!(ids.iter().all(|&i| i != u16::MAX));
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 5);
    }

    #[test]
    fn literal_paths_and_defaults() {
        let d = TestDir::new("literal");
        wav(&d.0, "x.wav", 480);
        std::fs::write(
            d.0.join("profiles.json"),
            r#"[{"id":"t","name":"T","subtitle":"s","color":"FFF",
                "samples":["x.wav"],"releaseSamples":null,"keySamples":null}]"#,
        )
        .unwrap();
        let mut mixer = Mixer::new(48000);
        let report = load_manifest(&d.0, &mut mixer).unwrap();
        let p = &report.profiles[0];
        assert!(report.errors.is_empty());
        assert_eq!(p.gain, 1.0);
        assert!(p.normalization_reference);
        assert!(p.releases.is_empty());
        assert!(p.keys.is_empty());
        assert!(p.brand.is_none());
        assert!(p.provenance.is_none());
    }

    #[test]
    fn missing_file_excludes_profile_and_names_path() {
        let d = TestDir::new("missing");
        wav(&d.0, "ok.wav", 480);
        std::fs::write(
            d.0.join("profiles.json"),
            r#"[
                {"id":"good","name":"G","subtitle":"s","color":"F","samples":["ok.wav"]},
                {"id":"bad","name":"B","subtitle":"s","color":"F","samples":["gone.wav"]}
            ]"#,
        )
        .unwrap();
        let mut mixer = Mixer::new(48000);
        let report = load_manifest(&d.0, &mut mixer).unwrap();
        assert_eq!(report.profiles.len(), 1);
        assert_eq!(report.profiles[0].id, "good");
        assert_eq!(report.errors.len(), 1);
        assert_eq!(report.errors[0].0, "bad");
        let msg = report.errors[0].1.to_string();
        assert!(msg.contains("gone.wav"), "error should name file: {msg}");
    }

    #[test]
    fn failed_profile_leaves_no_registry_orphans() {
        let d = TestDir::new("orphans");
        // 3-sample profile whose 3rd file is missing — first two must not
        // leak into the registry.
        wav(&d.0, "a.wav", 480);
        wav(&d.0, "b.wav", 480);
        std::fs::write(
            d.0.join("profiles.json"),
            r#"[
                {"id":"good","name":"G","subtitle":"s","color":"F","samples":["a.wav"]},
                {"id":"leaky","name":"L","subtitle":"s","color":"F",
                 "samples":["a.wav","b.wav","gone.wav"]}
            ]"#,
        )
        .unwrap();
        let mut mixer = Mixer::new(48000);
        let report = load_manifest(&d.0, &mut mixer).unwrap();
        assert_eq!(report.profiles.len(), 1);
        assert_eq!(report.errors.len(), 1);
        // Only "good"'s one sample registered → next id is 1, not 3.
        assert_eq!(registry_len(&mut mixer), 1);
    }

    #[test]
    fn over_15s_rejected() {
        let d = TestDir::new("long");
        write_wav(&d.0.join("long.wav"), 8000, 1, &vec![1000i16; 16 * 8000]); // 16s @ 8kHz
        std::fs::write(
            d.0.join("profiles.json"),
            r#"[{"id":"t","name":"T","subtitle":"s","color":"F","samples":["long.wav"]}]"#,
        )
        .unwrap();
        let mut mixer = Mixer::new(48000);
        let report = load_manifest(&d.0, &mut mixer).unwrap();
        assert!(report.profiles.is_empty());
        assert_eq!(report.errors.len(), 1);
        assert!(matches!(report.errors[0].1, Error::TooLong(_, _)));
        assert!(report.errors[0].1.to_string().contains("long.wav"));
    }

    #[test]
    fn stereo_is_mixed_down() {
        let d = TestDir::new("stereo");
        // L=+0.5, R=-0.5 → mono 0.0; check via decode() directly.
        write_wav(&d.0.join("st.wav"), 48000, 2, &[16384, -16384, 16384, -16384]);
        let (pcm, rate) = decode(&d.0.join("st.wav")).unwrap();
        assert_eq!(rate, 48000);
        assert_eq!(pcm.len(), 2);
        assert!(pcm.iter().all(|&s| s.abs() < 1e-4));
    }

    #[test]
    fn malformed_entry_excluded_not_fatal() {
        let d = TestDir::new("malformed");
        wav(&d.0, "ok.wav", 480);
        std::fs::write(
            d.0.join("profiles.json"),
            r#"[
                {"id":"broken"},
                {"id":"good","name":"G","subtitle":"s","color":"F","samples":["ok.wav"]}
            ]"#,
        )
        .unwrap();
        let mut mixer = Mixer::new(48000);
        let report = load_manifest(&d.0, &mut mixer).unwrap();
        assert_eq!(report.profiles.len(), 1);
        assert_eq!(report.errors.len(), 1);
        assert_eq!(report.errors[0].0, "broken");
        // Error path names manifest + entry id for greppable logs.
        assert!(report.errors[0]
            .1
            .to_string()
            .contains("entry[broken]"));
    }
}
