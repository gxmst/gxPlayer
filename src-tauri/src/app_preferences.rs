use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use gx_audio::engine::{AudioMode, DspControlState};
use serde::{Deserialize, Serialize};

const PREFERENCES_VERSION: u32 = 2;
const MAX_DEVICE_NAME_BYTES: usize = 500;
const MAX_CUSTOM_EQ_PRESETS: usize = 24;
const MAX_CUSTOM_EQ_NAME_CHARS: usize = 40;
/// The product EQ is a fixed 10-band dictionary, so a stored curve carries exactly
/// that many gains. Mirrors `DspControlState::validate_product`, which caps bands at
/// 10 and gain at +-12 dB; a curve outside that would be rejected on restore.
const CUSTOM_EQ_BAND_COUNT: usize = 10;
const MAX_CUSTOM_EQ_GAIN_DB: f32 = 12.0;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CloseBehavior {
    #[default]
    HideToTray,
    Exit,
}

/// A user-saved EQ curve. Only the gains are stored: frequencies, Q and filter kind
/// are fixed by the product EQ, so a stored curve cannot smuggle in a band shape the
/// preset path would never build.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CustomEqPreset {
    pub name: String,
    pub gains_db: Vec<f32>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AppPreferences {
    pub version: u32,
    pub close_behavior: CloseBehavior,
    pub close_to_tray_notice_shown: bool,
    pub volume: f32,
    pub output_device: Option<String>,
    pub dsp_control: DspControlState,
    pub custom_eq_presets: Vec<CustomEqPreset>,
    /// Storefront for the public chart feed, validated against the supported list.
    pub chart_region: String,
    /// Whether opening the home view may fetch the chart without being asked.
    pub chart_auto_load: bool,
}

impl Default for AppPreferences {
    fn default() -> Self {
        Self {
            version: PREFERENCES_VERSION,
            close_behavior: CloseBehavior::HideToTray,
            close_to_tray_notice_shown: false,
            volume: 1.0,
            output_device: None,
            dsp_control: DspControlState::default(),
            custom_eq_presets: Vec::new(),
            chart_region: gx_metadata::DEFAULT_CHART_REGION.to_owned(),
            chart_auto_load: true,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawAppPreferences {
    version: Option<u32>,
    close_behavior: Option<CloseBehavior>,
    close_to_tray_notice_shown: Option<bool>,
    volume: Option<f32>,
    output_device: Option<String>,
    dsp_control: Option<serde_json::Value>,
    audio_mode: Option<serde_json::Value>,
    custom_eq_presets: Option<Vec<serde_json::Value>>,
    chart_region: Option<String>,
    chart_auto_load: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseAction {
    Exit,
    Hide,
    Explain,
}

struct PreferencesInner {
    preferences: AppPreferences,
    newer_version: Option<u64>,
}

pub struct AppPreferencesState {
    path: PathBuf,
    inner: Mutex<PreferencesInner>,
    dsp_transaction: Mutex<()>,
}

impl AppPreferencesState {
    pub fn open(app_data: &Path) -> Self {
        let path = app_data.join("app-preferences.json");
        let loaded = load_preferences(&path);
        Self {
            path,
            inner: Mutex::new(PreferencesInner {
                preferences: loaded.preferences,
                newer_version: loaded.newer_version,
            }),
            dsp_transaction: Mutex::new(()),
        }
    }

    pub fn get(&self) -> AppPreferences {
        self.inner.lock().unwrap().preferences.clone()
    }

    pub fn close_action(&self) -> CloseAction {
        let preferences = &self.inner.lock().unwrap().preferences;
        match preferences.close_behavior {
            CloseBehavior::Exit => CloseAction::Exit,
            CloseBehavior::HideToTray if preferences.close_to_tray_notice_shown => {
                CloseAction::Hide
            }
            CloseBehavior::HideToTray => CloseAction::Explain,
        }
    }

    pub fn set_close_behavior(&self, behavior: CloseBehavior) -> Result<AppPreferences, String> {
        self.update(|preferences| preferences.close_behavior = behavior)
    }

    pub fn mark_close_notice_shown(&self) -> Result<AppPreferences, String> {
        self.update(|preferences| preferences.close_to_tray_notice_shown = true)
    }

    pub fn set_volume(&self, volume: f32) -> Result<AppPreferences, String> {
        if !volume.is_finite() {
            return Err("volume must be finite".into());
        }
        self.update(|preferences| preferences.volume = volume.clamp(0.0, 1.0))
    }

    pub fn set_output_device(&self, name: Option<String>) -> Result<AppPreferences, String> {
        let name = normalize_device_name(name);
        self.update(|preferences| preferences.output_device = name)
    }

    pub fn set_dsp_control(&self, dsp_control: DspControlState) -> Result<AppPreferences, String> {
        dsp_control
            .validate_product()
            .map_err(|error| error.to_string())?;
        self.update(|preferences| preferences.dsp_control = dsp_control)
    }

    /// Save a curve under `name`, replacing an existing entry with the same name so
    /// re-saving edits in place rather than accumulating duplicates.
    pub fn save_custom_eq_preset(&self, preset: CustomEqPreset) -> Result<AppPreferences, String> {
        let preset = sanitize_custom_eq_preset(preset)?;
        let existing = self.get().custom_eq_presets;
        let replaces = existing.iter().any(|entry| entry.name == preset.name);
        if !replaces && existing.len() >= MAX_CUSTOM_EQ_PRESETS {
            return Err(format!(
                "cannot store more than {MAX_CUSTOM_EQ_PRESETS} custom presets"
            ));
        }
        self.update(|preferences| {
            match preferences
                .custom_eq_presets
                .iter_mut()
                .find(|entry| entry.name == preset.name)
            {
                Some(entry) => entry.gains_db = preset.gains_db,
                None => preferences.custom_eq_presets.push(preset),
            }
        })
    }

    pub fn set_chart_region(&self, region: &str) -> Result<AppPreferences, String> {
        let normalized = gx_metadata::normalize_chart_region(Some(region));
        if normalized != region.trim().to_ascii_lowercase() {
            return Err("unsupported chart region".into());
        }
        self.update(|preferences| preferences.chart_region = normalized.to_owned())
    }

    pub fn set_chart_auto_load(&self, enabled: bool) -> Result<AppPreferences, String> {
        self.update(|preferences| preferences.chart_auto_load = enabled)
    }

    pub fn delete_custom_eq_preset(&self, name: &str) -> Result<AppPreferences, String> {
        let name = name.trim();
        if !self
            .get()
            .custom_eq_presets
            .iter()
            .any(|entry| entry.name == name)
        {
            return Err("no custom preset with that name".into());
        }
        self.update(|preferences| {
            preferences
                .custom_eq_presets
                .retain(|entry| entry.name != name);
        })
    }

    pub(crate) fn lock_dsp_transaction(&self) -> std::sync::MutexGuard<'_, ()> {
        self.dsp_transaction.lock().unwrap()
    }

    pub fn clear_output_device_if_matches(&self, expected: &str) -> Result<bool, String> {
        let mut inner = self.inner.lock().unwrap();
        ensure_writable(&inner)?;
        if inner.preferences.output_device.as_deref() != Some(expected) {
            return Ok(false);
        }
        let mut next = inner.preferences.clone();
        next.output_device = None;
        persist_preferences(&self.path, &next)?;
        inner.preferences = next;
        Ok(true)
    }

    fn update(&self, mutate: impl FnOnce(&mut AppPreferences)) -> Result<AppPreferences, String> {
        let mut inner = self.inner.lock().unwrap();
        ensure_writable(&inner)?;
        let mut next = inner.preferences.clone();
        mutate(&mut next);
        normalize_preferences(&mut next);
        if next == inner.preferences {
            return Ok(next);
        }
        persist_preferences(&self.path, &next)?;
        inner.preferences = next.clone();
        Ok(next)
    }
}

fn ensure_writable(inner: &PreferencesInner) -> Result<(), String> {
    if let Some(version) = inner.newer_version {
        return Err(format!(
            "偏好文件来自更新版本（v{version}），当前版本不会覆盖它"
        ));
    }
    Ok(())
}

struct LoadedPreferences {
    preferences: AppPreferences,
    newer_version: Option<u64>,
}

fn load_preferences(path: &Path) -> LoadedPreferences {
    match read_preferences(path) {
        ReadPreferences::Loaded(preferences) => LoadedPreferences {
            preferences,
            newer_version: None,
        },
        ReadPreferences::Newer(version) => LoadedPreferences {
            preferences: AppPreferences::default(),
            newer_version: Some(version),
        },
        ReadPreferences::MissingOrInvalid => match read_preferences(&backup_path(path)) {
            ReadPreferences::Loaded(preferences) => LoadedPreferences {
                preferences,
                newer_version: None,
            },
            ReadPreferences::Newer(version) => LoadedPreferences {
                preferences: AppPreferences::default(),
                newer_version: Some(version),
            },
            ReadPreferences::MissingOrInvalid => LoadedPreferences {
                preferences: AppPreferences::default(),
                newer_version: None,
            },
        },
    }
}

enum ReadPreferences {
    Loaded(AppPreferences),
    Newer(u64),
    MissingOrInvalid,
}

fn read_preferences(path: &Path) -> ReadPreferences {
    let Ok(bytes) = fs::read(path) else {
        return ReadPreferences::MissingOrInvalid;
    };
    let Ok(document) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return ReadPreferences::MissingOrInvalid;
    };
    let version = document
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    if version > u64::from(PREFERENCES_VERSION) {
        return ReadPreferences::Newer(version);
    }
    let Ok(raw) = serde_json::from_value::<RawAppPreferences>(document) else {
        return ReadPreferences::MissingOrInvalid;
    };
    if u64::from(raw.version.unwrap_or(0)) != version {
        return ReadPreferences::MissingOrInvalid;
    }
    let mut preferences = AppPreferences {
        version: PREFERENCES_VERSION,
        close_behavior: raw.close_behavior.unwrap_or_default(),
        close_to_tray_notice_shown: raw.close_to_tray_notice_shown.unwrap_or(false),
        volume: raw.volume.unwrap_or(1.0),
        output_device: raw.output_device,
        dsp_control: load_dsp_control(raw.dsp_control, raw.audio_mode),
        custom_eq_presets: load_custom_eq_presets(raw.custom_eq_presets),
        chart_region: gx_metadata::normalize_chart_region(raw.chart_region.as_deref()).to_owned(),
        chart_auto_load: raw.chart_auto_load.unwrap_or(true),
    };
    normalize_preferences(&mut preferences);
    ReadPreferences::Loaded(preferences)
}

/// Drop individually unusable entries instead of failing the whole file: a single bad
/// saved curve should not cost the user every other preset, or their whole settings.
fn load_custom_eq_presets(raw: Option<Vec<serde_json::Value>>) -> Vec<CustomEqPreset> {
    let Some(raw) = raw else {
        return Vec::new();
    };
    raw.into_iter()
        .filter_map(|value| serde_json::from_value::<CustomEqPreset>(value).ok())
        .filter_map(|preset| sanitize_custom_eq_preset(preset).ok())
        .take(MAX_CUSTOM_EQ_PRESETS)
        .collect()
}

fn normalize_preferences(preferences: &mut AppPreferences) {
    preferences.version = PREFERENCES_VERSION;
    preferences.volume = if preferences.volume.is_finite() {
        preferences.volume.clamp(0.0, 1.0)
    } else {
        1.0
    };
    preferences.output_device = normalize_device_name(preferences.output_device.take());
    // Re-validate on every load and save: the region reaches a URL path.
    preferences.chart_region =
        gx_metadata::normalize_chart_region(Some(&preferences.chart_region)).to_owned();
}

fn load_dsp_control(
    serialized: Option<serde_json::Value>,
    legacy_audio_mode: Option<serde_json::Value>,
) -> DspControlState {
    serialized
        .and_then(|value| serde_json::from_value::<DspControlState>(value).ok())
        .filter(|control| control.validate_product().is_ok())
        .or_else(|| {
            legacy_audio_mode
                .and_then(|value| serde_json::from_value::<AudioMode>(value).ok())
                .map(DspControlState::from_audio_mode)
        })
        .unwrap_or_default()
}

/// Normalize a saved curve and reject anything the engine would refuse on restore.
fn sanitize_custom_eq_preset(mut preset: CustomEqPreset) -> Result<CustomEqPreset, String> {
    preset.name = preset.name.trim().to_owned();
    if preset.name.is_empty() {
        return Err("preset name cannot be empty".into());
    }
    if preset.name.chars().count() > MAX_CUSTOM_EQ_NAME_CHARS {
        return Err(format!(
            "preset name cannot exceed {MAX_CUSTOM_EQ_NAME_CHARS} characters"
        ));
    }
    if preset.gains_db.len() != CUSTOM_EQ_BAND_COUNT {
        return Err(format!(
            "preset must carry exactly {CUSTOM_EQ_BAND_COUNT} band gains"
        ));
    }
    if preset
        .gains_db
        .iter()
        .any(|gain| !gain.is_finite() || gain.abs() > MAX_CUSTOM_EQ_GAIN_DB)
    {
        return Err(format!(
            "preset gains must be finite and within +-{MAX_CUSTOM_EQ_GAIN_DB} dB"
        ));
    }
    Ok(preset)
}

fn normalize_device_name(name: Option<String>) -> Option<String> {
    name.map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty() && value.len() <= MAX_DEVICE_NAME_BYTES)
}

fn persist_preferences(path: &Path, preferences: &AppPreferences) -> Result<(), String> {
    if path.is_dir() {
        return Err("app preferences path is a directory".into());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let bytes = serde_json::to_vec_pretty(preferences).map_err(|error| error.to_string())?;
    let temporary = path.with_extension("json.tmp");
    let backup = backup_path(path);
    let mut file = File::create(&temporary).map_err(|error| error.to_string())?;
    file.write_all(&bytes).map_err(|error| error.to_string())?;
    file.flush().map_err(|error| error.to_string())?;
    file.sync_data().map_err(|error| error.to_string())?;
    drop(file);

    match fs::rename(&temporary, path) {
        Ok(()) => {
            let _ = remove_if_exists(&backup);
            Ok(())
        }
        Err(_) if path.exists() => {
            remove_if_exists(&backup)?;
            fs::rename(path, &backup).map_err(|error| error.to_string())?;
            if let Err(error) = fs::rename(&temporary, path) {
                let _ = fs::rename(&backup, path);
                return Err(error.to_string());
            }
            // The new main file is already committed. A stale backup is preferable to
            // reporting failure while disk and in-memory preferences now disagree.
            let _ = remove_if_exists(&backup);
            Ok(())
        }
        Err(error) => {
            let _ = fs::remove_file(temporary);
            Err(error.to_string())
        }
    }
}

fn backup_path(path: &Path) -> PathBuf {
    path.with_extension("json.bak")
}

fn remove_if_exists(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    fn test_root(name: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "gxplayer-app-preferences-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn defaults_and_legacy_audio_modes_migrate_to_dsp_control() {
        let root = test_root("migration");
        let path = root.join("app-preferences.json");
        assert_eq!(
            load_preferences(&path).preferences,
            AppPreferences::default()
        );

        fs::create_dir_all(&root).unwrap();
        fs::write(&path, br#"{"audioMode":"music"}"#).unwrap();
        assert_eq!(
            load_preferences(&path).preferences.dsp_control,
            DspControlState::from_audio_mode(AudioMode::Music)
        );

        fs::write(
            &path,
            br#"{"version":1,"volume":1.5,"outputDevice":"  USB DAC  ","audioMode":"cinema_game"}"#,
        )
        .unwrap();
        let loaded = load_preferences(&path).preferences;
        assert_eq!(loaded.volume, 1.0);
        assert_eq!(loaded.output_device.as_deref(), Some("USB DAC"));
        assert_eq!(loaded.close_behavior, CloseBehavior::HideToTray);
        assert_eq!(
            loaded.dsp_control,
            DspControlState::from_audio_mode(AudioMode::CinemaGame)
        );

        persist_preferences(&path, &loaded).unwrap();
        let serialized = fs::read_to_string(&path).unwrap();
        assert!(!serialized.contains("audioMode"));
        assert!(serialized.contains("dspControl"));
        assert!(serialized.contains(r#""version": 2"#));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dsp_control_round_trips_and_valid_v2_state_wins_over_malformed_legacy_mode() {
        let root = test_root("dsp-round-trip");
        let path = root.join("app-preferences.json");
        let expected = DspControlState::from_audio_mode(AudioMode::CinemaGame);
        let preferences = AppPreferences {
            volume: 0.6,
            output_device: Some("USB DAC".into()),
            dsp_control: expected.clone(),
            ..AppPreferences::default()
        };

        persist_preferences(&path, &preferences).unwrap();
        assert_eq!(load_preferences(&path).preferences, preferences);

        let mut serialized = serde_json::to_value(&preferences).unwrap();
        serialized
            .as_object_mut()
            .unwrap()
            .insert("audioMode".into(), serde_json::json!({ "broken": true }));
        fs::write(&path, serde_json::to_vec_pretty(&serialized).unwrap()).unwrap();
        assert_eq!(load_preferences(&path).preferences.dsp_control, expected);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn malformed_or_invalid_dsp_control_only_falls_back_dsp_fields() {
        let root = test_root("invalid-dsp");
        let path = root.join("app-preferences.json");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            &path,
            br#"{"version":2,"volume":0.4,"outputDevice":"  USB DAC  ","dspControl":{"broken":true}}"#,
        )
        .unwrap();
        let malformed = load_preferences(&path).preferences;
        assert_eq!(malformed.volume, 0.4);
        assert_eq!(malformed.output_device.as_deref(), Some("USB DAC"));
        assert_eq!(malformed.dsp_control, DspControlState::default());

        fs::write(
            &path,
            br#"{"version":1,"volume":0.3,"outputDevice":"  Legacy DAC  ","audioMode":{"broken":true}}"#,
        )
        .unwrap();
        let malformed_legacy = load_preferences(&path).preferences;
        assert_eq!(malformed_legacy.volume, 0.3);
        assert_eq!(
            malformed_legacy.output_device.as_deref(),
            Some("Legacy DAC")
        );
        assert_eq!(malformed_legacy.dsp_control, DspControlState::default());

        let mut invalid = serde_json::to_value(DspControlState::default()).unwrap();
        invalid
            .as_object_mut()
            .unwrap()
            .insert("intensity".into(), serde_json::json!(99.0));
        let invalid_control = serde_json::from_value::<DspControlState>(invalid.clone()).unwrap();
        let state = AppPreferencesState::open(&root);
        let before_rejected_update = fs::read_to_string(&path).unwrap();
        assert!(state.set_dsp_control(invalid_control).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), before_rejected_update);

        let document = serde_json::json!({
            "version": 2,
            "volume": 0.25,
            "audioMode": "cinema_game",
            "dspControl": invalid,
        });
        fs::write(&path, serde_json::to_vec_pretty(&document).unwrap()).unwrap();
        let invalid = load_preferences(&path).preferences;
        assert_eq!(invalid.volume, 0.25);
        assert_eq!(
            invalid.dsp_control,
            DspControlState::from_audio_mode(AudioMode::CinemaGame)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn corrupt_main_recovers_from_backup_and_future_versions_are_read_only() {
        let root = test_root("backup");
        let path = root.join("app-preferences.json");
        fs::create_dir_all(&root).unwrap();
        fs::write(&path, b"broken").unwrap();
        fs::write(
            backup_path(&path),
            br#"{"version":1,"closeBehavior":"exit","volume":0.4}"#,
        )
        .unwrap();
        let recovered = load_preferences(&path);
        assert_eq!(recovered.preferences.close_behavior, CloseBehavior::Exit);
        assert_eq!(recovered.preferences.volume, 0.4);
        assert_eq!(
            recovered.preferences.dsp_control,
            DspControlState::default()
        );

        let future_document = r#"{"version":99,"closeBehavior":"ask_each_time","volume":0.2,"dspControl":{"future":true}}"#;
        fs::write(&path, future_document).unwrap();
        let state = AppPreferencesState::open(&root);
        assert!(state.set_volume(0.5).is_err());
        assert!(
            state
                .set_dsp_control(DspControlState::from_audio_mode(AudioMode::CinemaGame))
                .is_err()
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), future_document);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_persist_keeps_runtime_values_and_compare_clear_is_safe() {
        let root = test_root("atomic");
        let state = AppPreferencesState::open(&root);
        let spatial = DspControlState::from_audio_mode(AudioMode::CinemaGame);
        state.set_output_device(Some("USB DAC".into())).unwrap();
        state.set_dsp_control(spatial.clone()).unwrap();
        assert!(!state.clear_output_device_if_matches("Speakers").unwrap());
        assert_eq!(state.get().output_device.as_deref(), Some("USB DAC"));

        fs::remove_file(root.join("app-preferences.json")).unwrap();
        fs::create_dir_all(root.join("app-preferences.json")).unwrap();
        assert!(state.set_volume(0.25).is_err());
        assert!(state.set_dsp_control(DspControlState::default()).is_err());
        assert_eq!(state.get().volume, 1.0);
        assert_eq!(state.get().dsp_control, spatial);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn set_dsp_control_persists_for_the_next_process() {
        let root = test_root("dsp-persistence");
        let expected = DspControlState::from_audio_mode(AudioMode::CinemaGame);
        let state = AppPreferencesState::open(&root);
        let saved = state.set_dsp_control(expected.clone()).unwrap();
        assert_eq!(saved.dsp_control, expected);
        drop(state);

        let reopened = AppPreferencesState::open(&root);
        assert_eq!(reopened.get().dsp_control, expected);
        fs::remove_dir_all(root).unwrap();
    }

    fn curve(gain: f32) -> Vec<f32> {
        vec![gain; CUSTOM_EQ_BAND_COUNT]
    }

    #[test]
    fn custom_eq_presets_persist_and_resave_in_place() {
        let root = test_root("custom-eq-persist");
        let state = AppPreferencesState::open(&root);
        state
            .save_custom_eq_preset(CustomEqPreset {
                name: "  夜听  ".into(),
                gains_db: curve(2.0),
            })
            .unwrap();
        // The name is trimmed, so re-saving the trimmed form edits rather than adds.
        let saved = state
            .save_custom_eq_preset(CustomEqPreset {
                name: "夜听".into(),
                gains_db: curve(3.0),
            })
            .unwrap();
        assert_eq!(saved.custom_eq_presets.len(), 1);
        assert_eq!(saved.custom_eq_presets[0].name, "夜听");
        assert_eq!(saved.custom_eq_presets[0].gains_db, curve(3.0));
        drop(state);

        let reopened = AppPreferencesState::open(&root);
        assert_eq!(reopened.get().custom_eq_presets, saved.custom_eq_presets);
        let after_delete = reopened.delete_custom_eq_preset("夜听").unwrap();
        assert!(after_delete.custom_eq_presets.is_empty());
        assert!(reopened.delete_custom_eq_preset("夜听").is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn custom_eq_presets_reject_what_the_engine_would_refuse() {
        let root = test_root("custom-eq-reject");
        let state = AppPreferencesState::open(&root);
        for (label, preset) in [
            (
                "empty name",
                CustomEqPreset {
                    name: "   ".into(),
                    gains_db: curve(1.0),
                },
            ),
            (
                "wrong band count",
                CustomEqPreset {
                    name: "短".into(),
                    gains_db: vec![1.0; 9],
                },
            ),
            (
                "gain past the product limit",
                CustomEqPreset {
                    name: "过响".into(),
                    gains_db: curve(12.5),
                },
            ),
            (
                "non-finite gain",
                CustomEqPreset {
                    name: "非数".into(),
                    gains_db: curve(f32::NAN),
                },
            ),
        ] {
            assert!(
                state.save_custom_eq_preset(preset).is_err(),
                "{label} should be rejected"
            );
        }
        assert!(state.get().custom_eq_presets.is_empty());

        // A curve at exactly the limit is valid, and the engine agrees.
        state
            .save_custom_eq_preset(CustomEqPreset {
                name: "边界".into(),
                gains_db: curve(MAX_CUSTOM_EQ_GAIN_DB),
            })
            .unwrap();
        let control = DspControlState {
            settings: gx_dsp::DspSettings {
                enabled: true,
                eq_enabled: true,
                eq_bands: (0..CUSTOM_EQ_BAND_COUNT)
                    .map(|index| {
                        gx_dsp::EqBand::peak(
                            125.0 * (index as f32 + 1.0),
                            MAX_CUSTOM_EQ_GAIN_DB,
                            1.0,
                        )
                    })
                    .collect(),
                limiter: gx_dsp::LimiterSettings {
                    enabled: true,
                    ..Default::default()
                },
                ..Default::default()
            },
            active_preset_id: gx_audio::engine::DspPresetId::Custom,
            intensity: 0.5,
            spatial_amount: 0.5,
        };
        control.validate_product().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_single_corrupt_saved_curve_does_not_cost_the_rest() {
        let root = test_root("custom-eq-corrupt");
        let state = AppPreferencesState::open(&root);
        state
            .save_custom_eq_preset(CustomEqPreset {
                name: "好".into(),
                gains_db: curve(1.5),
            })
            .unwrap();
        drop(state);

        // Hand-edit the file to include one unusable entry alongside the good one.
        let path = root.join("app-preferences.json");
        let mut document: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        document["customEqPresets"] = serde_json::json!([
            { "name": "坏", "gainsDb": [1.0, 2.0] },
            { "name": "好", "gainsDb": curve(1.5) },
            { "name": "", "gainsDb": curve(1.0) },
        ]);
        fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();

        let reopened = AppPreferencesState::open(&root);
        let presets = reopened.get().custom_eq_presets;
        assert_eq!(presets.len(), 1);
        assert_eq!(presets[0].name, "好");
        // The rest of the settings survived too.
        assert_eq!(reopened.get().volume, 1.0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn close_decision_requires_one_explanation_then_hides() {
        let root = test_root("close");
        let state = AppPreferencesState::open(&root);
        assert_eq!(state.close_action(), CloseAction::Explain);
        state.mark_close_notice_shown().unwrap();
        assert_eq!(state.close_action(), CloseAction::Hide);
        state.set_close_behavior(CloseBehavior::Exit).unwrap();
        assert_eq!(state.close_action(), CloseAction::Exit);
        fs::remove_dir_all(root).unwrap();
    }
}
