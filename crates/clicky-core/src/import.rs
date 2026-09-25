//! User sound-pack import — thock `pack.json` schema.
//!
//! A pack is a directory holding `pack.json` plus its audio files:
//!
//! ```json
//! {
//!   "name": "Display name", "license": "CC0-1.0", "version": "1.0.0",
//!   "default": { "press": "generic.wav", "release": "generic_up.wav" },
//!   "keys": { "Space": { "press": "space.wav" } }
//! }
//! ```
//!
//! `default` feeds the [`Profile`]'s generic presses/releases; `keys.<name>`
//! becomes a `keySamples` entry after [`name_to_keyid`] maps the logical name
//! to a HID keyid (unknown names are dropped with a warning, never fatal).
//! Per thock semantics a key with no `release` falls back to the pack's
//! default releases — imported eagerly: the fallback ids are copied in, so
//! engine's "empty key release = silent" rule stays intact. `press`/`release`
//! accept one path or a list.
//!
//! [`import_pack`] decodes everything (same symphonia mono ≤15 s pipeline as
//! [`crate::profiles`]) before copying or registering, so a bad pack leaves no
//! files behind and no orphaned registry slots. The pack lands at
//! `dest_root/<slug>/` (`-2`, `-3`… on collision) with a converted
//! `profile.json` marker so a later startup scan can re-load it, and the
//! `user-packs.json` index gains the new entry.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::mixer::Mixer;
use crate::profiles::{self, KeySamples, Profile};

/// Import failures. Sample-path failures reuse [`profiles::Error`] verbatim
/// so callers see the same messages as manifest loads.
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    /// `pack.json` unreadable.
    #[error("cannot read {0}: {1}")]
    Read(PathBuf, #[source] std::io::Error),
    /// `pack.json` malformed or missing required fields.
    #[error("cannot parse {0}: {1}")]
    Parse(PathBuf, #[source] serde_json::Error),
    /// `name` missing/blank or slugifies to nothing.
    #[error("pack name missing or has no usable characters")]
    BadName,
    /// Any filesystem write (copy, `profile.json`, index) failed.
    #[error("{0}: {1}")]
    Io(PathBuf, #[source] std::io::Error),
    /// Referenced audio file missing/undecodable/over-length/too many samples.
    #[error(transparent)]
    Sample(#[from] profiles::Error),
}

/// Index record in `user-packs.json` — one per imported pack.
#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
pub struct UserPack {
    pub id: String,
    pub name: String,
    /// Directory containing the pack (`dest_root/<id>`).
    pub dir: String,
}

/// One sample path or a list — thock allows both for `press`/`release`.
#[derive(Deserialize)]
#[serde(untagged)]
enum Paths {
    One(String),
    Many(Vec<String>),
}

impl Paths {
    fn paths(&self) -> &[String] {
        match self {
            Paths::One(s) => std::slice::from_ref(s),
            Paths::Many(v) => v,
        }
    }
}

/// `{press?, release?}` for `default` or a `keys` entry.
#[derive(Deserialize, Default)]
struct RawSamples {
    press: Option<Paths>,
    release: Option<Paths>,
}

/// thock `pack.json` (superset tolerated — unknown fields ignored).
/// `name` is required; everything else optional.
#[derive(Deserialize)]
struct RawPack {
    name: String,
    license: Option<String>,
    version: Option<String>,
    description: Option<String>,
    author: Option<String>,
    default: Option<RawSamples>,
    keys: Option<BTreeMap<String, RawSamples>>,
}

/// Map a logical pack key name to a HID "page:usage" keyid.
///
/// Matching is case-insensitive and ignores spaces, `-` and `_`
/// ("Left-Shift" ≡ "leftshift"). Covers thock's recognized names
/// (Space/Enter/Backspace/Tab/Shift/Control/Alt/Meta + MouseLeft/Right/
/// Middle), letters/digits, F1–F24, arrows, nav/editing keys and numpad.
/// Bare modifier names resolve to the left usage ("Shift" → "7:225",
/// `LeftShift`/`RightShift` distinct). Unknown → `None`.
pub fn name_to_keyid(name: &str) -> Option<&'static str> {
    static NAMES: &[(&str, &str)] = &[
        // thock logical set.
        ("space", "7:44"), ("enter", "7:40"), ("backspace", "7:42"),
        ("tab", "7:43"), ("shift", "7:225"), ("lshift", "7:225"),
        ("control", "7:224"), ("ctrl", "7:224"), ("lctrl", "7:224"),
        ("alt", "7:226"), ("lalt", "7:226"),
        ("meta", "7:227"), ("super", "7:227"), ("cmd", "7:227"),
        ("win", "7:227"), ("gui", "7:227"), ("lmeta", "7:227"),
        ("mouseleft", "9:1"), ("mouseright", "9:2"), ("mousemiddle", "9:3"),
        // Modifiers, explicit sides.
        ("leftshift", "7:225"), ("rightshift", "7:229"), ("rshift", "7:229"),
        ("leftcontrol", "7:224"), ("leftctrl", "7:224"),
        ("rightcontrol", "7:228"), ("rightctrl", "7:228"), ("rctrl", "7:228"),
        ("leftalt", "7:226"), ("rightalt", "7:230"), ("altgr", "7:230"),
        ("ralt", "7:230"), ("leftmeta", "7:227"), ("rightmeta", "7:231"),
        ("rmeta", "7:231"),
        ("fn", "8:3"),
        // Punctuation / editing / nav.
        ("escape", "7:41"), ("esc", "7:41"), ("capslock", "7:57"),
        ("arrowup", "7:82"), ("up", "7:82"), ("arrowdown", "7:81"),
        ("down", "7:81"), ("arrowleft", "7:80"), ("left", "7:80"),
        ("arrowright", "7:79"), ("right", "7:79"),
        ("delete", "7:76"), ("del", "7:76"), ("insert", "7:73"),
        ("ins", "7:73"), ("home", "7:74"), ("end", "7:77"),
        ("pageup", "7:75"), ("pagedown", "7:78"),
        ("printscreen", "7:70"), ("scrolllock", "7:71"), ("pause", "7:72"),
        ("menu", "7:101"), ("numlock", "7:83"),
        ("minus", "7:45"), ("equal", "7:46"),
        ("leftbracket", "7:47"), ("rightbracket", "7:48"),
        ("semicolon", "7:51"), ("apostrophe", "7:52"), ("quote", "7:52"),
        ("backquote", "7:53"), ("grave", "7:53"), ("backslash", "7:49"),
        ("comma", "7:54"), ("period", "7:55"), ("dot", "7:55"),
        ("slash", "7:56"),
        // Letters.
        ("a", "7:4"), ("b", "7:5"), ("c", "7:6"), ("d", "7:7"),
        ("e", "7:8"), ("f", "7:9"), ("g", "7:10"), ("h", "7:11"),
        ("i", "7:12"), ("j", "7:13"), ("k", "7:14"), ("l", "7:15"),
        ("m", "7:16"), ("n", "7:17"), ("o", "7:18"), ("p", "7:19"),
        ("q", "7:20"), ("r", "7:21"), ("s", "7:22"), ("t", "7:23"),
        ("u", "7:24"), ("v", "7:25"), ("w", "7:26"), ("x", "7:27"),
        ("y", "7:28"), ("z", "7:29"),
        // Digits (number row).
        ("1", "7:30"), ("2", "7:31"), ("3", "7:32"), ("4", "7:33"),
        ("5", "7:34"), ("6", "7:35"), ("7", "7:36"), ("8", "7:37"),
        ("9", "7:38"), ("0", "7:39"),
        // Function keys.
        ("f1", "7:58"), ("f2", "7:59"), ("f3", "7:60"), ("f4", "7:61"),
        ("f5", "7:62"), ("f6", "7:63"), ("f7", "7:64"), ("f8", "7:65"),
        ("f9", "7:66"), ("f10", "7:67"), ("f11", "7:68"), ("f12", "7:69"),
        ("f13", "7:104"), ("f14", "7:105"), ("f15", "7:106"),
        ("f16", "7:107"), ("f17", "7:108"), ("f18", "7:109"),
        ("f19", "7:110"), ("f20", "7:111"), ("f21", "7:112"),
        ("f22", "7:113"), ("f23", "7:114"), ("f24", "7:115"),
        // Numpad.
        ("numpad0", "7:98"), ("numpad1", "7:95"), ("numpad2", "7:96"),
        ("numpad3", "7:97"), ("numpad4", "7:92"), ("numpad5", "7:93"),
        ("numpad6", "7:94"), ("numpad7", "7:89"), ("numpad8", "7:90"),
        ("numpad9", "7:91"), ("numpadenter", "7:88"),
        ("numpaddivide", "7:84"), ("numpadmultiply", "7:85"),
        ("numpadminus", "7:86"), ("numpadsubtract", "7:86"),
        ("numpadplus", "7:87"), ("numpadadd", "7:87"),
        ("numpaddecimal", "7:99"), ("numpaddot", "7:99"),
        ("numpadcomma", "7:133"), ("numpadequal", "7:103"),
    ];
    let mut norm = String::with_capacity(name.len());
    for c in name.chars() {
        if !matches!(c, ' ' | '-' | '_') {
            norm.push(c.to_ascii_lowercase());
        }
    }
    NAMES
        .iter()
        .find(|(n, _)| *n == norm)
        .map(|(_, id)| *id)
}

/// Import the pack at `src_dir` into `dest_root/<slug>/`, decode its audio
/// and register every sample in `mixer`.
///
/// Order is decode-everything → capacity pre-check → copy → write
/// `profile.json` → register → append the `user-packs.json` index. A bad
/// manifest or audio file fails before anything is copied or registered.
/// Returns the runtime [`Profile`] (mixer sample ids); non-fatal drops land
/// in `profile.warnings`.
pub fn import_pack(src_dir: &Path, dest_root: &Path, mixer: &mut Mixer) -> Result<Profile, ImportError> {
    let manifest_path = src_dir.join("pack.json");
    let text = std::fs::read(&manifest_path)
        .map_err(|e| ImportError::Read(manifest_path.clone(), e))?;
    let mut raw: RawPack =
        serde_json::from_slice(&text).map_err(|e| ImportError::Parse(manifest_path.clone(), e))?;
    let name = raw.name.trim().to_owned();
    if name.is_empty() {
        return Err(ImportError::BadName);
    }
    let base = slug(&name);
    if base.is_empty() {
        return Err(ImportError::BadName);
    }

    // Phase 1: decode every referenced file (nothing registered yet).
    let (def_press, def_release) = paths_of(raw.default.as_ref());
    let staged_presses = profiles::stage_list(src_dir, def_press)?;
    let staged_releases = profiles::stage_list(src_dir, def_release)?;
    let mut warnings = Vec::new();
    // (keyid, press paths, release paths (None = thock fallback to default),
    // staged press, staged release)
    let mut keys = Vec::new();
    for (name, ks) in raw.keys.take().unwrap_or_default() {
        let Some(keyid) = name_to_keyid(&name) else {
            warnings.push(format!("unknown key name \"{name}\" — dropped"));
            continue;
        };
        let (press_paths, rel_paths) = paths_of(Some(&ks));
        keys.push((
            keyid,
            press_paths.to_vec(),
            if ks.release.is_some() {
                Some(rel_paths.to_vec())
            } else {
                None
            },
            profiles::stage_list(src_dir, press_paths)?,
            profiles::stage_list(src_dir, rel_paths)?,
        ));
    }


    let needed = staged_presses.len()
        + staged_releases.len()
        + keys.iter().map(|(_, _, _, p, r)| p.len() + r.len()).sum::<usize>();
    if mixer.registered_count() + needed > crate::mixer::MAX_SAMPLES {
        return Err(ImportError::Sample(profiles::Error::Registry(
            src_dir.join("pack.json"),
        )));
    }

    // Copy to dest_root/<slug>/ (skip when already in place), write the
    // converted profile.json, then phase 2: register.
    let dest_dir = pick_dest(dest_root, &base, src_dir);
    // id = chosen dir name so a "same-2" pack gets a distinct runtime id.
    let id = dest_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&base)
        .to_owned();
    if !same_dir(src_dir, &dest_dir) {
        copy_dir(src_dir, &dest_dir)?;
    }
    write_profile_json(&dest_dir, &id, &name, &raw, def_press, def_release, &keys)?;

    let mut levels = std::collections::HashMap::with_capacity(needed);
    let presses = profiles::register_list(staged_presses, mixer)?;
    levels.extend(presses.iter().copied());
    let presses = presses.into_iter().map(|(id, _)| id).collect();
    let releases = profiles::register_list(staged_releases, mixer)?;
    levels.extend(releases.iter().copied());
    let releases: Vec<u16> = releases.into_iter().map(|(id, _)| id).collect();
    let mut key_map = std::collections::HashMap::with_capacity(keys.len());
    for (keyid, _, rel_paths, p, r) in keys {
        let p = profiles::register_list(p, mixer)?;
        let r = profiles::register_list(r, mixer)?;
        levels.extend(p.iter().chain(r.iter()).copied());
        let r: Vec<u16> = if rel_paths.is_none() {
            releases.clone() // thock: missing release falls back to default
        } else {
            r.into_iter().map(|(id, _)| id).collect()
        };
        key_map.insert(
            keyid.to_string(),
            KeySamples {
                presses: p.into_iter().map(|(id, _)| id).collect(),
                releases: r,
            },
        );
    }

    let profile = Profile {
        id,
        name,
        brand: None,
        subtitle: String::new(),
        color: String::new(),
        gain: 1.0,
        normalization_reference: false,
        presses,
        releases,
        keys: key_map,
        levels,
        provenance: Some(provenance(&raw)),
        warnings,
    };
    append_user_pack_index(dest_root, &profile)?;
    Ok(profile)
}

/// Append `profile` to `dest_root/user-packs.json` (creating it); an existing
/// entry with the same id is replaced. The startup scan uses the index to
/// find each pack's directory under `dest_root`.
pub fn append_user_pack_index(dest_root: &Path, profile: &Profile) -> Result<(), ImportError> {
    let path = dest_root.join("user-packs.json");
    let mut packs: Vec<UserPack> = match std::fs::read(&path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|e| ImportError::Parse(path.clone(), e))?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(ImportError::Read(path.clone(), e)),
    };
    packs.retain(|p| p.id != profile.id);
    packs.push(UserPack {
        id: profile.id.clone(),
        name: profile.name.clone(),
        dir: profile.id.clone(),
    });
    let json = serde_json::to_string_pretty(&packs)
        .map_err(|e| ImportError::Parse(path.clone(), e))?;
    std::fs::write(&path, json).map_err(|e| ImportError::Io(path, e))
}

/// `(press paths, release paths)` from a `RawSamples` (`None` → two empties).
fn paths_of(s: Option<&RawSamples>) -> (&[String], &[String]) {
    let s = match s {
        Some(s) => s,
        None => return (&[], &[]),
    };
    (
        s.press.as_ref().map_or(&[], Paths::paths),
        s.release.as_ref().map_or(&[], Paths::paths),
    )
}

/// Keep license/version/description/author as provenance on the Profile.
fn provenance(raw: &RawPack) -> serde_json::Value {
    let mut v = serde_json::Map::new();
    if let Some(x) = &raw.license {
        v.insert("license".into(), x.clone().into());
    }
    if let Some(x) = &raw.version {
        v.insert("version".into(), x.clone().into());
    }
    if let Some(x) = &raw.description {
        v.insert("description".into(), x.clone().into());
    }
    if let Some(x) = &raw.author {
        v.insert("author".into(), x.clone().into());
    }
    v.insert("source".into(), "pack.json".into());
    serde_json::Value::Object(v)
}

/// Write `dest_dir/profile.json` in the `profiles.json` entry shape so the
/// startup scan can re-load imported packs through `load_manifest`.
fn write_profile_json(
    dest_dir: &Path,
    id: &str,
    name: &str,
    raw: &RawPack,
    def_press: &[String],
    def_release: &[String],
    keys: &[(&str, Vec<String>, Option<Vec<String>>, profiles::Staged, profiles::Staged)],
) -> Result<(), ImportError> {
    let mut key_samples = serde_json::Map::new();
    for (keyid, press, release, _, _) in keys {
        // `None` release = thock default fallback → write the default paths so
        // the reloaded manifest resolves identically.
        let release = release.as_deref().unwrap_or(def_release);
        let mut e = serde_json::Map::new();
        e.insert("samples".into(), press.clone().into());
        if !release.is_empty() {
            e.insert("releaseSamples".into(), release.to_vec().into());
        }
        key_samples.insert((*keyid).to_string(), e.into());
    }
    let mut entry = serde_json::json!({
        "id": id,
        "name": name,
        "brand": raw.author,
        "subtitle": raw.description.as_deref().unwrap_or("user pack"),
        "color": "",
        "samples": def_press,
        "gain": 1.0,
        "normalizationReference": false,
        "provenance": provenance(raw),
    });
    let obj = entry.as_object_mut().expect("json! object");
    if !def_release.is_empty() {
        obj.insert("releaseSamples".into(), def_release.into());
    }
    if !key_samples.is_empty() {
        obj.insert("keySamples".into(), key_samples.into());
    }
    let path = dest_dir.join("profile.json");
    std::fs::write(&path, serde_json::to_string_pretty(&entry).unwrap())
        .map_err(|e| ImportError::Io(path, e))
}

/// `dest_root/<slug>`; on collision `<slug>-2`, `-3`, … An existing dir that
/// IS `src_dir` (re-import in place) isn't a collision.
fn pick_dest(dest_root: &Path, base: &str, src_dir: &Path) -> PathBuf {
    for n in 0.. {
        let id = if n == 0 { base.to_string() } else { format!("{base}-{}", n + 1) };
        let dir = dest_root.join(&id);
        if !dir.exists() || same_dir(&dir, src_dir) {
            return dir;
        }
    }
    unreachable!()
}

/// True when `a` and `b` resolve to the same directory.
fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Lowercase alnum runs joined by `-` ("Cherry MX Blue" → "cherry-mx-blue").
fn slug(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut dash = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            dash = false;
        } else if !out.is_empty() && !dash {
            out.push('-');
            dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Recursive copy of a directory's regular files (subdirs included, links
/// and specials skipped — packs are plain file trees).
fn copy_dir(src: &Path, dest: &Path) -> Result<(), ImportError> {
    std::fs::create_dir_all(dest).map_err(|e| ImportError::Io(dest.to_path_buf(), e))?;
    for entry in std::fs::read_dir(src).map_err(|e| ImportError::Read(src.to_path_buf(), e))? {
        let entry = entry.map_err(|e| ImportError::Read(src.to_path_buf(), e))?;
        let ty = entry
            .file_type()
            .map_err(|e| ImportError::Read(entry.path(), e))?;
        let to = dest.join(entry.file_name());
        if ty.is_dir() {
            copy_dir(&entry.path(), &to)?;
        } else if ty.is_file() {
            std::fs::copy(entry.path(), &to).map_err(|e| ImportError::Io(to.clone(), e))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal PCM WAV writer — enough for symphonia to probe.
    fn write_wav(path: &Path, rate: u32, samples: &[i16]) {
        let data = (samples.len() * 2) as u32;
        let mut w = Vec::with_capacity(44 + data as usize);
        w.extend_from_slice(b"RIFF");
        w.extend_from_slice(&(36 + data).to_le_bytes());
        w.extend_from_slice(b"WAVEfmt ");
        w.extend_from_slice(&16u32.to_le_bytes());
        w.extend_from_slice(&1u16.to_le_bytes()); // PCM
        w.extend_from_slice(&1u16.to_le_bytes()); // mono
        w.extend_from_slice(&rate.to_le_bytes());
        w.extend_from_slice(&(rate * 2).to_le_bytes());
        w.extend_from_slice(&2u16.to_le_bytes());
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
            let p =
                std::env::temp_dir().join(format!("clicky-import-{}-{name}", std::process::id()));
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
        write_wav(&dir.join(name), 48000, &vec![1000i16; n]);
    }

    /// src pack dir + dest root under one scratch root.
    fn dirs(name: &str) -> (TestDir, PathBuf, PathBuf) {
        let d = TestDir::new(name);
        let src = d.0.join("src");
        let dest = d.0.join("packs");
        std::fs::create_dir_all(&src).unwrap();
        (d, src, dest)
    }

    #[test]
    fn imports_pack_registers_and_writes_marker() {
        let (_d, src, dest) = dirs("ok");
        for f in ["gen.wav", "gen_up.wav", "sp.wav", "ent.wav"] {
            wav(&src, f, 480);
        }
        std::fs::write(
            src.join("pack.json"),
            r#"{
                "name": "My Pack",
                "license": "CC0-1.0",
                "version": "1.2.3",
                "author": "Tester",
                "unknownField": [1, 2],
                "default": { "press": "gen.wav", "release": "gen_up.wav" },
                "keys": {
                    "Space": { "press": "sp.wav" },
                    "Enter": { "press": ["ent.wav"], "release": "gen_up.wav" }
                }
            }"#,
        )
        .unwrap();

        let mut mixer = Mixer::new(48000);
        let p = import_pack(&src, &dest, &mut mixer).unwrap();
        assert_eq!(p.id, "my-pack");
        assert_eq!(p.name, "My Pack");
        assert_eq!(p.presses.len(), 1);
        assert_eq!(p.releases.len(), 1);
        // Space had no release → falls back to the default release id.
        let ks = &p.keys["7:44"];
        assert_eq!(ks.presses.len(), 1);
        assert_eq!(ks.releases, p.releases);
        // Enter declared its own release → a separate registered copy.
        let ent = &p.keys["7:40"];
        assert_eq!(ent.presses.len(), 1);
        assert_eq!(ent.releases.len(), 1);
        assert_ne!(ent.releases, p.releases);
        assert_eq!(p.provenance.as_ref().unwrap()["license"], "CC0-1.0");
        assert!(p.warnings.is_empty());
        // Copied into packs/my-pack/, marker written, index updated.
        let pack_dir = dest.join("my-pack");
        for f in ["gen.wav", "sp.wav", "pack.json", "profile.json"] {
            assert!(pack_dir.join(f).exists(), "{f} missing in dest");
        }
        let marker: serde_json::Value =
            serde_json::from_slice(&std::fs::read(pack_dir.join("profile.json")).unwrap()).unwrap();
        assert_eq!(marker["id"], "my-pack");
        assert_eq!(marker["samples"], serde_json::json!(["gen.wav"]));
        assert!(marker["keySamples"]["7:44"]["releaseSamples"].is_array());
        let index: serde_json::Value = serde_json::from_slice(
            &std::fs::read(dest.join("user-packs.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(index[0]["id"], "my-pack");
    }

    #[test]
    fn bad_manifest_errors() {
        let (_d, src, dest) = dirs("bad");
        std::fs::write(src.join("pack.json"), b"{not json").unwrap();
        let mut mixer = Mixer::new(48000);
        assert!(matches!(
            import_pack(&src, &dest, &mut mixer),
            Err(ImportError::Parse(..))
        ));
        // Missing required `name`.
        std::fs::write(src.join("pack.json"), r#"{"default":{}}"#).unwrap();
        assert!(matches!(
            import_pack(&src, &dest, &mut mixer),
            Err(ImportError::Parse(..))
        ));
        // Name with no usable chars.
        std::fs::write(src.join("pack.json"), r#"{"name":"-_.-"}"#).unwrap();
        assert!(matches!(
            import_pack(&src, &dest, &mut mixer),
            Err(ImportError::BadName)
        ));
        // Missing manifest entirely.
        let empty = src.join("nope");
        std::fs::create_dir_all(&empty).unwrap();
        assert!(matches!(
            import_pack(&empty, &dest, &mut mixer),
            Err(ImportError::Read(..))
        ));
        // Nothing copied on failure.
        assert!(!dest.exists());
    }

    #[test]
    fn overlong_sample_rejected() {
        let (_d, src, dest) = dirs("long");
        // 16 s at 48 kHz exceeds the 15 s cap.
        write_wav(&src.join("gen.wav"), 48000, &vec![1000i16; 48000 * 16]);
        std::fs::write(
            src.join("pack.json"),
            r#"{"name":"Long","default":{"press":"gen.wav"}}"#,
        )
        .unwrap();
        let mut mixer = Mixer::new(48000);
        match import_pack(&src, &dest, &mut mixer) {
            Err(ImportError::Sample(profiles::Error::TooLong(_, s))) => {
                assert!((s - 16.0).abs() < 0.1)
            }
            other => panic!("expected TooLong, got {other:?}"),
        }
        assert!(!dest.join("long").exists());
        assert_eq!(mixer.registered_count(), 0);
    }

    #[test]
    fn unknown_key_dropped_with_warning() {
        let (_d, src, dest) = dirs("warn");
        for f in ["gen.wav", "sp.wav", "x.wav"] {
            wav(&src, f, 480);
        }
        std::fs::write(
            src.join("pack.json"),
            r#"{
                "name": "Warn",
                "default": { "press": "gen.wav" },
                "keys": {
                    "Space": { "press": "sp.wav" },
                    "Bogus": { "press": "x.wav" }
                }
            }"#,
        )
        .unwrap();
        let mut mixer = Mixer::new(48000);
        let p = import_pack(&src, &dest, &mut mixer).unwrap();
        assert_eq!(p.keys.len(), 1);
        assert!(p.keys.contains_key("7:44"));
        assert_eq!(p.warnings.len(), 1);
        assert!(p.warnings[0].contains("Bogus"));
    }

    #[test]
    fn name_collision_gets_suffix() {
        let (_d, src, dest) = dirs("collide");
        wav(&src, "gen.wav", 480);
        std::fs::write(
            src.join("pack.json"),
            r#"{"name":"Same","default":{"press":"gen.wav"}}"#,
        )
        .unwrap();
        let mut mixer = Mixer::new(48000);
        let p1 = import_pack(&src, &dest, &mut mixer).unwrap();
        let p2 = import_pack(&src, &dest, &mut mixer).unwrap();
        assert_eq!(p1.id, "same");
        assert_eq!(p2.id, "same-2");
        assert!(dest.join("same").exists());
        assert!(dest.join("same-2").exists());
    }

    #[test]
    fn index_appended_and_deduped() {
        let (_d, src, dest) = dirs("index");
        wav(&src, "gen.wav", 480);
        std::fs::write(
            src.join("pack.json"),
            r#"{"name":"Idx","default":{"press":"gen.wav"}}"#,
        )
        .unwrap();
        let mut mixer = Mixer::new(48000);
        let p = import_pack(&src, &dest, &mut mixer).unwrap();
        // Re-adding the same id replaces, doesn't duplicate.
        append_user_pack_index(&dest, &p).unwrap();
        let index: serde_json::Value = serde_json::from_slice(
            &std::fs::read(dest.join("user-packs.json")).unwrap(),
        )
        .unwrap();
        let arr = index.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["id"], "idx");
        assert_eq!(arr[0]["name"], "Idx");
        assert_eq!(arr[0]["dir"], "idx");
    }

    #[test]
    fn key_name_mapping() {
        assert_eq!(name_to_keyid("Space"), Some("7:44"));
        assert_eq!(name_to_keyid("Enter"), Some("7:40"));
        assert_eq!(name_to_keyid("Backspace"), Some("7:42"));
        assert_eq!(name_to_keyid("Shift"), Some("7:225"));
        assert_eq!(name_to_keyid("right-shift"), Some("7:229"));
        assert_eq!(name_to_keyid("MouseLeft"), Some("9:1"));
        assert_eq!(name_to_keyid("ArrowUp"), Some("7:82"));
        assert_eq!(name_to_keyid("Escape"), Some("7:41"));
        assert_eq!(name_to_keyid("CapsLock"), Some("7:57"));
        assert_eq!(name_to_keyid("a"), Some("7:4"));
        assert_eq!(name_to_keyid("F12"), Some("7:69"));
        assert_eq!(name_to_keyid("7:44"), None);
        assert_eq!(name_to_keyid("Nope"), None);
    }
}
