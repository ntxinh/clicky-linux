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
//! [`LoadReport::errors`] and excluded — it never fails the whole load.

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
/// (see [`Mixer::register`]), not file paths.
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
    /// Optional provenance block, passed through untouched.
    pub provenance: Option<serde_json::Value>,
}

/// Mixer sample ids for one keyid's press/release override.
#[derive(Debug, Default)]
pub struct KeySamples {
    pub presses: Vec<u16>,
    pub releases: Vec<u16>,
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
    #[serde(default)]
    key_samples: HashMap<String, RawKeySamples>,
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

/// Read `dir/profiles.json` and resolve every entry. Manifest-level failures
/// (file unreadable, top-level JSON not an array) are fatal `Err`; per-entry
/// failures land in `errors` and that profile is excluded.
pub fn load_manifest(dir: &Path, mixer: &mut Mixer) -> Result<LoadReport, Error> {
    let path = dir.join("profiles.json");
    let text = std::fs::read(&path).map_err(|e| Error::Read(path.clone(), e))?;
    let entries: Vec<serde_json::Value> =
        serde_json::from_slice(&text).map_err(|e| Error::Parse(path.clone(), e))?;

    let mut report = LoadReport::default();
    for (i, entry) in entries.into_iter().enumerate() {
        let id = entry
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("<index {i}>"));
        let raw: RawProfile = match serde_json::from_value(entry) {
            Ok(r) => r,
            Err(e) => {
                report
                    .errors
                    .push((id, Error::Parse(path.join("<entry>"), e)));
                continue;
            }
        };
        match load_profile(dir, raw, mixer) {
            Ok(p) => report.profiles.push(p),
            Err(e) => report.errors.push((id, e)),
        }
    }
    Ok(report)
}

/// Resolve one profile: validate + decode + register every sample.
fn load_profile(dir: &Path, raw: RawProfile, mixer: &mut Mixer) -> Result<Profile, Error> {
    let presses = load_list(dir, &raw.samples, mixer)?;
    let releases = match &raw.release_samples {
        Some(paths) => load_list(dir, paths, mixer)?,
        None => Vec::new(),
    };
    let mut keys = HashMap::with_capacity(raw.key_samples.len());
    for (keyid, ks) in raw.key_samples {
        keys.insert(
            keyid,
            KeySamples {
                presses: load_list(dir, &ks.samples, mixer)?,
                releases: match &ks.release_samples {
                    Some(paths) => load_list(dir, paths, mixer)?,
                    None => Vec::new(),
                },
            },
        );
    }
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
        provenance: raw.provenance,
    })
}

/// Decode + register a list of sample paths; any failure aborts the profile.
fn load_list(dir: &Path, paths: &[String], mixer: &mut Mixer) -> Result<Vec<u16>, Error> {
    paths
        .iter()
        .map(|rel| {
            let path = resolve(dir, rel);
            let (pcm, rate) = decode(&path)?;
            match mixer.register(pcm, rate) {
                u16::MAX => Err(Error::Registry(path)),
                id => Ok(id),
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
                "samples":["x.wav"],"releaseSamples":null}]"#,
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
        // L=+0.5, L=-0.5 → mono 0.0; check via decode() directly.
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
    }
}
