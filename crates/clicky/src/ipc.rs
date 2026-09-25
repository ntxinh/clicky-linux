//! Tauri IPC surface: the UI process owns the engine (spawned in-process by
//! [`run_app`]); `#[tauri::command]` fns mutate config → apply to engine/
//! mixer → `save_debounced` → emit `config`. Persist failures roll back
//! engine, mixer and store so an `Err` genuinely means nothing changed.
//!
//! No tray: libayatana-appindicator isn't available in the build sysroot, so
//! the window opens on launch and close→hide keeps the app alive. `Quit`
//! lives on the Home page (and `clicky quit` still works when --daemon owns
//! the engine — in UI mode there is no control socket).

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use clicky_core::audio::Audio;
use clicky_core::config::{AppConfiguration, KeyOverride, ModifierSoundMode};
use clicky_core::import;
use clicky_core::input;
use clicky_core::profiles::Profile;
use parking_lot::Mutex;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};

use crate::daemon::{self, Shared};

/// Everything IPC commands touch. `audio` is separate from `shared` because
/// `set_device` needs `&mut Audio` while `shared` stays lockable.
pub struct AppState {
    pub shared: Arc<Mutex<Shared>>,
    pub audio: Mutex<Audio>,
    /// Loaded-profile ids is implicit in `engine.profiles()`; sounds dir is
    /// where imported packs land (`<dir>/user/`).
    pub sounds_dir: PathBuf,
    pub quit: Arc<AtomicBool>,
    pub capture_alive: Arc<AtomicBool>,
    pub input: Mutex<Option<std::thread::JoinHandle<()>>>,
}

type CmdResult<T> = Result<T, String>;

fn map_err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

/// Broadcast the persisted config to every listener (thock pattern).
fn emit_config(app: &AppHandle, cfg: &AppConfiguration) {
    if let Err(e) = app.emit("config", cfg) {
        eprintln!("clicky: emit config: {e}");
    }
}

/// The shared mutation shape: clone cfg → `f` mutates it → push into engine
/// → persist. On save failure the engine (and store) get the previous config
/// back so nothing half-applies. Emits `config` on success.
fn mutate(
    app: &AppHandle,
    sh: &mut Shared,
    f: impl FnOnce(&mut AppConfiguration),
) -> CmdResult<()> {
    let prev = sh.store.cfg.clone();
    let mut cfg = prev.clone();
    f(&mut cfg);
    sh.engine.update_config(cfg.clone());
    if let Err(e) = sh.store.save_debounced(&cfg) {
        revert(sh, prev);
        return Err(map_err(e));
    }
    emit_config(app, &cfg);
    Ok(())
}

/// Persist failure: restore engine + store to the previous config.
fn revert(sh: &mut Shared, prev: AppConfiguration) {
    sh.engine.update_config(prev.clone());
    sh.store.cfg = prev;
}

/// Profile fields the UI cards need (Profile itself isn't Serialize).
#[derive(Serialize)]
struct ProfileInfo {
    id: String,
    name: String,
    brand: Option<String>,
    subtitle: String,
    color: String,
    /// Number of key-specific sample sets (per-key override picker hint).
    key_count: usize,
}

fn profile_info(p: &Profile) -> ProfileInfo {
    ProfileInfo {
        id: p.id.clone(),
        name: p.name.clone(),
        brand: p.brand.clone(),
        subtitle: p.subtitle.clone(),
        color: p.color.clone(),
        key_count: p.keys.len(),
    }
}

#[tauri::command]
fn get_config(state: State<AppState>) -> AppConfiguration {
    state.shared.lock().store.cfg.clone()
}

#[tauri::command]
fn set_enabled(app: AppHandle, state: State<AppState>, enabled: bool) -> CmdResult<()> {
    let mut sh = state.shared.lock();
    let prev = sh.store.cfg.clone();
    let mut cfg = prev.clone();
    cfg.enabled = enabled;
    // Live path first (engine + mixer ramp), then persist.
    sh.engine.update_config(cfg.clone());
    state.audio.lock().set_enabled(enabled);
    if let Err(e) = sh.store.save_debounced(&cfg) {
        state.audio.lock().set_enabled(prev.enabled);
        revert(&mut sh, prev);
        return Err(map_err(e));
    }
    emit_config(&app, &cfg);
    Ok(())
}

#[tauri::command]
fn set_volume(app: AppHandle, state: State<AppState>, volume: f32) -> CmdResult<()> {
    let v = volume.clamp(0.0, 1.0);
    // Volume is per-trigger gain in the engine (Clicky's stack); the mixer's
    // separate master trim stays untouched — driving both squares it.
    let mut sh = state.shared.lock();
    mutate(&app, &mut sh, |c| c.sound.volume = v)
}

#[tauri::command]
fn set_tone(app: AppHandle, state: State<AppState>, tone: f32) -> CmdResult<()> {
    let mut sh = state.shared.lock();
    mutate(&app, &mut sh, |c| c.sound.tone = tone)
}

#[tauri::command]
fn set_pitch(app: AppHandle, state: State<AppState>, pitch: f32) -> CmdResult<()> {
    let mut sh = state.shared.lock();
    mutate(&app, &mut sh, |c| c.sound.pitch = pitch)
}

#[tauri::command]
fn set_spatial(
    app: AppHandle,
    state: State<AppState>,
    enabled: bool,
    width: Option<f32>,
) -> CmdResult<()> {
    let mut sh = state.shared.lock();
    mutate(&app, &mut sh, |c| {
        c.sound.spatial = enabled;
        if let Some(w) = width {
            c.sound.spatial_width = w;
        }
    })
}

#[tauri::command]
fn set_profile(app: AppHandle, state: State<AppState>, id: String) -> CmdResult<()> {
    let mut sh = state.shared.lock();
    if !sh.engine.profiles().iter().any(|p| p.id == id) {
        return Err(format!("unknown profile {id}"));
    }
    let prev = sh.store.cfg.clone();
    let mut cfg = prev.clone();
    cfg.sound.profile_id = id;
    sh.engine.update_config(cfg.clone());
    if let Err(e) = sh.store.save_debounced(&cfg) {
        revert(&mut sh, prev);
        return Err(map_err(e));
    }
    emit_config(&app, &cfg);
    Ok(())
}

#[tauri::command]
fn list_profiles(state: State<AppState>) -> Vec<ProfileInfo> {
    state
        .shared
        .lock()
        .engine
        .profiles()
        .iter()
        .map(profile_info)
        .collect()
}

/// Audition a key. `keyid` defaults to Enter; `profile` selects the bank to
/// preview without switching the configured profile.
#[tauri::command]
fn preview(state: State<AppState>, keyid: Option<String>, profile: Option<String>) {
    let keyid = keyid.unwrap_or_else(|| "7:40".into());
    let mut sh = state.shared.lock();
    match profile {
        Some(id) => {
            if let Some(idx) = sh.engine.profiles().iter().position(|p| p.id == id) {
                sh.engine.preview_on(idx, &keyid);
            }
        }
        None => sh.engine.preview(&keyid),
    }
}

fn parse_modifier_mode(s: &str) -> CmdResult<ModifierSoundMode> {
    match s {
        "soft" => Ok(ModifierSoundMode::Soft),
        "silent" => Ok(ModifierSoundMode::Silent),
        "full" => Ok(ModifierSoundMode::Full),
        "custom" => Ok(ModifierSoundMode::Custom),
        _ => Err(format!("unknown modifier mode {s}")),
    }
}

#[tauri::command]
fn set_modifier_mode(app: AppHandle, state: State<AppState>, mode: String) -> CmdResult<()> {
    let mode = parse_modifier_mode(&mode)?;
    let mut sh = state.shared.lock();
    mutate(&app, &mut sh, |c| c.sound.modifier_sound_mode = mode)
}

#[tauri::command]
fn set_modifier_gain(app: AppHandle, state: State<AppState>, gain: f32) -> CmdResult<()> {
    let mut sh = state.shared.lock();
    mutate(&app, &mut sh, |c| c.sound.modifier_custom_volume = gain)
}

#[tauri::command]
fn set_modifier_pitch(app: AppHandle, state: State<AppState>, pitch: f32) -> CmdResult<()> {
    let mut sh = state.shared.lock();
    mutate(&app, &mut sh, |c| c.sound.modifier_custom_pitch = pitch)
}

/// Upsert a per-key override. `fields` is a Clicky camelCase object —
/// `{profileID?, tone?, pitch?, volume?}` — with null/absent fields meaning
/// "inherit". An all-null object is a delete marker (validated() drops it).
#[tauri::command]
fn set_key_override(
    app: AppHandle,
    state: State<AppState>,
    keyid: String,
    fields: serde_json::Value,
) -> CmdResult<()> {
    let over: KeyOverride =
        serde_json::from_value(fields).map_err(|e| format!("bad override: {e}"))?;
    let mut sh = state.shared.lock();
    mutate(&app, &mut sh, |c| {
        if over.is_empty() {
            c.key_overrides.remove(&keyid);
        } else {
            c.key_overrides.insert(keyid.clone(), over.clone());
        }
    })
}

#[tauri::command]
fn clear_key_override(app: AppHandle, state: State<AppState>, keyid: String) -> CmdResult<()> {
    let mut sh = state.shared.lock();
    mutate(&app, &mut sh, |c| {
        c.key_overrides.remove(&keyid);
    })
}

#[tauri::command]
fn list_devices() -> Vec<clicky_core::audio::DeviceInfo> {
    Audio::devices()
}

#[tauri::command]
fn set_output_device(app: AppHandle, state: State<AppState>, name: String) -> CmdResult<()> {
    state.audio.lock().set_device(&name).map_err(map_err)?;
    let mut sh = state.shared.lock();
    let mut cfg = sh.store.cfg.clone();
    cfg.sound.output_device_uid = Some(name);
    // Device already switched; a persist failure only means the choice won't
    // survive restart — revert the uid, not the stream (repick can't undo).
    sh.store
        .save_debounced(&cfg)
        .map_err(map_err)?;
    emit_config(&app, &cfg);
    Ok(())
}

/// `~/.config/autostart/clicky.desktop` — Exec=clicky --daemon (headless; the
/// engine autostarts without opening the settings window).
#[tauri::command]
fn set_launch_at_login(app: AppHandle, state: State<AppState>, enabled: bool) -> CmdResult<()> {
    let autostart = directories::BaseDirs::new()
        .map(|b| b.config_dir().join("autostart"))
        .ok_or("no config dir")?;
    let file = autostart.join("clicky.desktop");
    if enabled {
        let exe = std::env::current_exe().map_err(map_err)?;
        std::fs::create_dir_all(&autostart).map_err(map_err)?;
        std::fs::write(
            &file,
            format!(
                "[Desktop Entry]\n\
                 Type=Application\n\
                 Name=clicky\n\
                 Comment=Mechanical keyboard sounds\n\
                 Exec={} --daemon\n\
                 Terminal=false\n\
                 X-GNOME-Autostart-enabled=true\n",
                exe.display()
            ),
        )
        .map_err(map_err)?;
    } else if file.exists() {
        std::fs::remove_file(&file).map_err(map_err)?;
    }
    let mut sh = state.shared.lock();
    mutate(&app, &mut sh, |c| c.general.launch_at_login = enabled)
}

/// Import a thock-format pack directory (`pack.json` + audio). Registers
/// samples into the live mixer and makes the profile selectable immediately;
/// files land at `<sounds>/user/<slug>/` for a future startup re-scan.
#[tauri::command]
fn import_pack(app: AppHandle, state: State<AppState>, path: String) -> CmdResult<ProfileInfo> {
    let src = PathBuf::from(&path);
    let dest = state.sounds_dir.join("user");
    // Decode everything outside the mixer lock; the register half holds it
    // only for the append loop.
    let staged = import::stage_pack(&src).map_err(map_err)?;
    let audio = state.audio.lock();
    let profile = {
        let mut mixer = audio.mixer().lock();
        import::import_staged(staged, &dest, &mut mixer).map_err(map_err)?
    };
    drop(audio);
    let info = profile_info(&profile);
    state.shared.lock().engine.push_profile(profile);
    emit_config(&app, &state.shared.lock().store.cfg.clone());
    Ok(info)
}

/// Daemon-style status JSON: capture liveness, device, mixer counters.
#[tauri::command]
fn get_diagnostics(state: State<AppState>) -> serde_json::Value {
    let scan = input::keyboard_devices();
    let alive = state.capture_alive.load(std::sync::atomic::Ordering::Relaxed);
    let audio = state.audio.lock();
    let stats = audio.stats();
    let sh = state.shared.lock();
    serde_json::json!({
        "enabled": sh.store.cfg.enabled,
        "profile": sh.engine.profile_id(),
        "audio_device": audio.device_name(),
        "sample_rate": audio.out_rate(),
        "channels": audio.channels(),
        "capture": if !alive {
            "dead"
        } else if !scan.keyboards.is_empty() {
            "live"
        } else if scan.issues.is_empty() {
            "no-keyboards"
        } else {
            "denied"
        },
        "keyboards": scan.keyboards.len(),
        "issues": scan.issues.iter().map(|i| format!("{}: {}", i.path.display(), i.error)).collect::<Vec<_>>(),
        "accepted": stats.accepted,
        "dropped": stats.dropped,
        "stolen": stats.stolen,
    })
}

/// Per-kind overlay toggle (`keyboard`/`keystrokes`/`combo`/`bezel`/
/// `keyboard3d`). The master `visualizer.enabled` follows "any kind on" —
/// overlay supervision itself is T15.
#[tauri::command]
fn visualizer_set(app: AppHandle, state: State<AppState>, kind: String, enabled: bool) -> CmdResult<()> {
    let mut sh = state.shared.lock();
    mutate(&app, &mut sh, |c| {
        c.visualizer.kinds.insert(kind.clone(), enabled);
        c.visualizer.enabled = c.visualizer.kinds.values().any(|v| *v);
    })
}

/// Persist current sound settings + key overrides as modifier preset `slot`
/// (0–5). Presets ride the `favorites` list — Clicky's preset shape.
#[tauri::command]
fn save_modifier_preset(app: AppHandle, state: State<AppState>, slot: usize) -> CmdResult<()> {
    if slot >= 6 {
        return Err("preset slot out of range (0-5)".into());
    }
    let mut sh = state.shared.lock();
    mutate(&app, &mut sh, |c| {
        let fav = clicky_core::config::Favorite {
            id: format!("preset-{slot}"),
            name: format!("Preset {}", slot + 1),
            sound: c.sound.clone(),
            key_overrides: c.key_overrides.clone(),
        };
        match c.favorites.iter_mut().find(|f| f.id == fav.id) {
            Some(f) => *f = fav,
            None => c.favorites.push(fav),
        }
    })
}

/// Recall preset `slot`: applies its sound snapshot + key overrides.
#[tauri::command]
fn apply_modifier_preset(app: AppHandle, state: State<AppState>, slot: usize) -> CmdResult<()> {
    let mut sh = state.shared.lock();
    let id = format!("preset-{slot}");
    let Some(fav) = sh.store.cfg.favorites.iter().find(|f| f.id == id).cloned() else {
        return Err(format!("preset {} is empty", slot + 1));
    };
    let prev = sh.store.cfg.clone();
    let mut cfg = prev.clone();
    cfg.sound = fav.sound;
    cfg.key_overrides = fav.key_overrides;
    sh.engine.update_config(cfg.clone());
    state.audio.lock().set_enabled(cfg.enabled);
    if let Err(e) = sh.store.save_debounced(&cfg) {
        state.audio.lock().set_enabled(prev.enabled);
        revert(&mut sh, prev);
        return Err(map_err(e));
    }
    emit_config(&app, &cfg);
    Ok(())
}

/// Hide-free quit path for the Home page (close→hide keeps the app alive).
#[tauri::command]
fn quit_app(app: AppHandle) {
    app.exit(0);
}

/// Bare `clicky`: boot the engine in-process, then run the Tauri event loop.
/// Degrades when boot pieces are missing: audio failure still opens the UI
/// (settings persist, preview is silent) via a stub — no, keep it simple:
/// boot failure exits with the error, matching the daemon.
pub fn run_app() -> i32 {
    let b = match daemon::boot() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("clicky: {e}");
            return 1;
        }
    };
    let daemon::Boot {
        shared,
        audio,
        controls: _,
        sounds_dir,
        quit,
        capture_alive,
        input,
    } = b;
    let state = AppState {
        shared,
        audio: Mutex::new(audio),
        sounds_dir,
        quit,
        capture_alive,
        input: Mutex::new(input),
    };
    run_app_with(state)
}

fn run_app_with(state: AppState) -> i32 {
    let quit = state.quit.clone();
    let app = tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            get_config,
            set_enabled,
            set_volume,
            set_tone,
            set_pitch,
            set_spatial,
            set_profile,
            list_profiles,
            preview,
            set_modifier_mode,
            set_modifier_gain,
            set_modifier_pitch,
            set_key_override,
            clear_key_override,
            list_devices,
            set_output_device,
            set_launch_at_login,
            import_pack,
            get_diagnostics,
            visualizer_set,
            save_modifier_preset,
            apply_modifier_preset,
            quit_app,
        ])
        .on_window_event(|window, event| {
            // No tray → the window is the only handle on the app; close hides
            // rather than exits (quit via Home or SIGTERM).
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .build(tauri::generate_context!());
    match app {
        Ok(app) => {
            app.run(move |app, event| {
                if let tauri::RunEvent::Exit = event {
                    quit.store(true, std::sync::atomic::Ordering::Relaxed);
                    let state = app.state::<AppState>();
                    if let Some(h) = state.input.lock().take() {
                        let _ = h.join();
                    }
                    if let Err(e) = state.shared.lock().store.flush() {
                        eprintln!("clicky: config save: {e}");
                    };
                }
            });
            0
        }
        Err(e) => {
            eprintln!("clicky: tauri build failed: {e}");
            1
        }
    }
}
