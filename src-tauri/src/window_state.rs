//! Persist main-window position/size and restore on next launch.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, LogicalPosition, LogicalSize, Manager, PhysicalPosition, PhysicalSize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WindowState {
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub width: Option<f64>,
    pub height: Option<f64>,
    pub maximized: bool,
    pub always_on_top: bool,
    pub mini_mode: bool,
}

fn state_path(app_data: &Path) -> PathBuf {
    app_data.join("window-state.json")
}

pub fn load(app_data: &Path) -> WindowState {
    let path = state_path(app_data);
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn save(app_data: &Path, state: &WindowState) -> Result<(), String> {
    let path = state_path(app_data);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let bytes = serde_json::to_vec_pretty(state).map_err(|e| e.to_string())?;
    fs::write(path, bytes).map_err(|e| e.to_string())
}

pub fn apply_to_window(window: &tauri::WebviewWindow, state: &WindowState) {
    if state.mini_mode {
        let _ = window.set_size(LogicalSize::new(360.0, 120.0));
        let _ = window.set_always_on_top(true);
    } else {
        if let (Some(width), Some(height)) = (state.width, state.height)
            && width >= 480.0
            && height >= 320.0
        {
            let _ = window.set_size(LogicalSize::new(width, height));
        }
        if let (Some(x), Some(y)) = (state.x, state.y) {
            let _ = window.set_position(LogicalPosition::new(x, y));
        }
        let _ = window.set_always_on_top(state.always_on_top);
        if state.maximized {
            let _ = window.maximize();
        }
    }
}

pub fn capture_from_window(window: &tauri::WebviewWindow, mini_mode: bool) -> WindowState {
    let maximized = window.is_maximized().unwrap_or(false);
    let always_on_top = window.is_always_on_top().unwrap_or(false);
    let mut state = WindowState {
        x: None,
        y: None,
        width: None,
        height: None,
        maximized,
        always_on_top,
        mini_mode,
    };
    if let Ok(position) = window.outer_position() {
        let PhysicalPosition { x, y } = position;
        if let Ok(scale) = window.scale_factor() {
            state.x = Some(x as f64 / scale);
            state.y = Some(y as f64 / scale);
        }
    }
    if let Ok(size) = window.outer_size() {
        let PhysicalSize { width, height } = size;
        if let Ok(scale) = window.scale_factor() {
            state.width = Some(width as f64 / scale);
            state.height = Some(height as f64 / scale);
        }
    }
    state
}

pub fn app_data_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map_err(|e| e.to_string())
}
