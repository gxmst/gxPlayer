use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

const PREFERENCES_VERSION: u32 = 1;
const MAX_DEVICE_NAME_BYTES: usize = 500;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CloseBehavior {
    #[default]
    HideToTray,
    Exit,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AppPreferences {
    pub version: u32,
    pub close_behavior: CloseBehavior,
    pub close_to_tray_notice_shown: bool,
    pub volume: f32,
    pub output_device: Option<String>,
}

impl Default for AppPreferences {
    fn default() -> Self {
        Self {
            version: PREFERENCES_VERSION,
            close_behavior: CloseBehavior::HideToTray,
            close_to_tray_notice_shown: false,
            volume: 1.0,
            output_device: None,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseAction {
    Exit,
    Hide,
    Explain,
}

struct PreferencesInner {
    preferences: AppPreferences,
    newer_version: Option<u32>,
}

pub struct AppPreferencesState {
    path: PathBuf,
    inner: Mutex<PreferencesInner>,
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
    newer_version: Option<u32>,
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
    Newer(u32),
    MissingOrInvalid,
}

fn read_preferences(path: &Path) -> ReadPreferences {
    let Ok(bytes) = fs::read(path) else {
        return ReadPreferences::MissingOrInvalid;
    };
    let Ok(raw) = serde_json::from_slice::<RawAppPreferences>(&bytes) else {
        return ReadPreferences::MissingOrInvalid;
    };
    let version = raw.version.unwrap_or(0);
    if version > PREFERENCES_VERSION {
        return ReadPreferences::Newer(version);
    }
    let mut preferences = AppPreferences {
        version: PREFERENCES_VERSION,
        close_behavior: raw.close_behavior.unwrap_or_default(),
        close_to_tray_notice_shown: raw.close_to_tray_notice_shown.unwrap_or(false),
        volume: raw.volume.unwrap_or(1.0),
        output_device: raw.output_device,
    };
    normalize_preferences(&mut preferences);
    ReadPreferences::Loaded(preferences)
}

fn normalize_preferences(preferences: &mut AppPreferences) {
    preferences.version = PREFERENCES_VERSION;
    preferences.volume = if preferences.volume.is_finite() {
        preferences.volume.clamp(0.0, 1.0)
    } else {
        1.0
    };
    preferences.output_device = normalize_device_name(preferences.output_device.take());
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
            remove_if_exists(&backup)?;
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
    fn defaults_and_v0_fields_migrate_without_persisting_audio_mode() {
        let root = test_root("migration");
        let path = root.join("app-preferences.json");
        assert_eq!(
            load_preferences(&path).preferences,
            AppPreferences::default()
        );

        fs::create_dir_all(&root).unwrap();
        fs::write(
            &path,
            br#"{"volume":1.5,"outputDevice":"  USB DAC  ","audioMode":"cinema_game"}"#,
        )
        .unwrap();
        let loaded = load_preferences(&path).preferences;
        assert_eq!(loaded.volume, 1.0);
        assert_eq!(loaded.output_device.as_deref(), Some("USB DAC"));
        assert_eq!(loaded.close_behavior, CloseBehavior::HideToTray);

        persist_preferences(&path, &loaded).unwrap();
        let serialized = fs::read_to_string(&path).unwrap();
        assert!(!serialized.contains("audioMode"));
        assert!(!serialized.contains("dsp"));
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

        fs::write(&path, br#"{"version":99,"volume":0.2}"#).unwrap();
        let state = AppPreferencesState::open(&root);
        assert!(state.set_volume(0.5).is_err());
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            r#"{"version":99,"volume":0.2}"#
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_persist_keeps_runtime_values_and_compare_clear_is_safe() {
        let root = test_root("atomic");
        let state = AppPreferencesState::open(&root);
        state.set_output_device(Some("USB DAC".into())).unwrap();
        assert!(!state.clear_output_device_if_matches("Speakers").unwrap());
        assert_eq!(state.get().output_device.as_deref(), Some("USB DAC"));

        fs::remove_file(root.join("app-preferences.json")).unwrap();
        fs::create_dir_all(root.join("app-preferences.json")).unwrap();
        assert!(state.set_volume(0.25).is_err());
        assert_eq!(state.get().volume, 1.0);
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
