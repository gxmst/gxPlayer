use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use gx_source::network_policy::{self, ProxyMode, ProxyStatus};
use serde::{Deserialize, Serialize};
use tauri::{State, WebviewWindow};

use crate::require_window;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedNetworkSettings {
    proxy_mode: ProxyMode,
}

pub struct NetworkSettingsState {
    path: PathBuf,
    proxy_mode: Mutex<ProxyMode>,
}

impl NetworkSettingsState {
    pub fn open(app_data: &Path) -> Self {
        let path = app_data.join("network-settings.json");
        let proxy_mode = load(&path).proxy_mode;
        network_policy::set_mode(proxy_mode);
        Self {
            path,
            proxy_mode: Mutex::new(proxy_mode),
        }
    }

    fn status(&self) -> ProxyStatus {
        let mode = *self.proxy_mode.lock().unwrap();
        let detected = network_policy::system_proxy_detected();
        ProxyStatus {
            mode,
            detected,
            effective: mode != ProxyMode::Off && detected,
        }
    }

    fn set_proxy_mode(&self, mode: ProxyMode) -> Result<ProxyStatus, String> {
        let mut current = self.proxy_mode.lock().unwrap();
        persist(&self.path, mode)?;
        *current = mode;
        network_policy::set_mode(mode);
        drop(current);
        Ok(self.status())
    }
}

fn load(path: &Path) -> PersistedNetworkSettings {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn persist(path: &Path, proxy_mode: ProxyMode) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let bytes = serde_json::to_vec_pretty(&PersistedNetworkSettings { proxy_mode })
        .map_err(|error| error.to_string())?;
    fs::write(path, bytes).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn network_proxy_status(
    window: WebviewWindow,
    state: State<'_, NetworkSettingsState>,
) -> Result<ProxyStatus, String> {
    require_window(&window, "main")?;
    Ok(state.status())
}

#[tauri::command]
pub fn network_set_proxy_mode(
    window: WebviewWindow,
    state: State<'_, NetworkSettingsState>,
    mode: ProxyMode,
) -> Result<ProxyStatus, String> {
    require_window(&window, "main")?;
    state.set_proxy_mode(mode)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    fn test_root(name: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "gxplayer-network-settings-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn missing_or_invalid_settings_default_to_auto() {
        let root = test_root("defaults");
        let path = root.join("network-settings.json");
        assert_eq!(load(&path).proxy_mode, ProxyMode::Auto);

        fs::create_dir_all(&root).unwrap();
        fs::write(&path, b"not json").unwrap();
        assert_eq!(load(&path).proxy_mode, ProxyMode::Auto);
        fs::write(&path, br#"{"proxyMode":"other"}"#).unwrap();
        assert_eq!(load(&path).proxy_mode, ProxyMode::Auto);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn manual_modes_and_auto_round_trip() {
        let root = test_root("round-trip");
        let path = root.join("network-settings.json");
        for mode in [ProxyMode::On, ProxyMode::Off, ProxyMode::Auto] {
            persist(&path, mode).unwrap();
            assert_eq!(load(&path).proxy_mode, mode);
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_persist_keeps_the_previous_runtime_mode() {
        let root = test_root("failed-persist");
        let state = NetworkSettingsState::open(&root);
        fs::create_dir_all(root.join("network-settings.json")).unwrap();

        assert!(state.set_proxy_mode(ProxyMode::Off).is_err());
        assert_eq!(*state.proxy_mode.lock().unwrap(), ProxyMode::Auto);
        assert_eq!(network_policy::mode(), ProxyMode::Auto);
        fs::remove_dir_all(root).unwrap();
    }
}
