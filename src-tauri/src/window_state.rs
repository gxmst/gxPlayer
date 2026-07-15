//! Persist main-window position/size and restore on next launch.
//!
//! Guards against the classic "saved while minimized / off-screen" bug that
//! makes the process appear running with no visible UI.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use tauri::{
    AppHandle, LogicalPosition, LogicalSize, Manager, Monitor, PhysicalPosition, PhysicalSize,
    WebviewWindow,
};

pub const NORMAL_MIN_WIDTH: f64 = 320.0;
pub const NORMAL_MIN_HEIGHT: f64 = 560.0;
pub const MINI_MIN_WIDTH: f64 = 320.0;
pub const MINI_MIN_HEIGHT: f64 = 120.0;
pub const MINI_DEFAULT_WIDTH: f64 = 380.0;
pub const MINI_DEFAULT_HEIGHT: f64 = 140.0;
pub const DEFAULT_WIDTH: f64 = 1024.0;
pub const DEFAULT_HEIGHT: f64 = 640.0;

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

/// Process-local source of truth for mini mode. Geometry events must not trust a React closure
/// because the resize generated while entering mini mode can arrive before frontend state updates.
pub struct WindowModeState {
    mini_mode: AtomicBool,
}

impl WindowModeState {
    pub fn new(mini_mode: bool) -> Self {
        Self {
            mini_mode: AtomicBool::new(mini_mode),
        }
    }

    pub fn mini_mode(&self) -> bool {
        self.mini_mode.load(Ordering::Acquire)
    }

    pub fn set_mini_mode(&self, enabled: bool) {
        self.mini_mode.store(enabled, Ordering::Release);
    }
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

/// Reset persisted geometry so the next launch uses the safe centered default.
pub fn clear_geometry(app_data: &Path) -> Result<(), String> {
    let mut state = load(app_data);
    state.x = None;
    state.y = None;
    state.width = None;
    state.height = None;
    state.maximized = false;
    state.mini_mode = false;
    save(app_data, &state)
}

fn monitors_for(window: &WebviewWindow) -> Vec<Monitor> {
    window.available_monitors().ok().unwrap_or_default()
}

/// True if at least `min_visible` logical pixels of the window sit on some monitor.
fn geometry_is_visible(
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    monitors: &[Monitor],
    min_visible: f64,
) -> bool {
    if !x.is_finite() || !y.is_finite() || !width.is_finite() || !height.is_finite() {
        return false;
    }
    // Classic minimized / iconified sentinel coordinates on Windows.
    if x < -10_000.0 || y < -10_000.0 {
        return false;
    }
    if width < 200.0 || height < 120.0 {
        return false;
    }
    if monitors.is_empty() {
        return true;
    }
    let win_l = x;
    let win_t = y;
    let win_r = x + width;
    let win_b = y + height;
    for monitor in monitors {
        let scale = monitor.scale_factor();
        let pos = monitor.position();
        let size = monitor.size();
        let mon_l = pos.x as f64 / scale;
        let mon_t = pos.y as f64 / scale;
        let mon_r = mon_l + size.width as f64 / scale;
        let mon_b = mon_t + size.height as f64 / scale;
        let inter_l = win_l.max(mon_l);
        let inter_t = win_t.max(mon_t);
        let inter_r = win_r.min(mon_r);
        let inter_b = win_b.min(mon_b);
        let inter_w = (inter_r - inter_l).max(0.0);
        let inter_h = (inter_b - inter_t).max(0.0);
        if inter_w * inter_h >= min_visible * min_visible {
            return true;
        }
    }
    false
}

fn clamp_size(width: f64, height: f64, mini_mode: bool) -> (f64, f64) {
    if mini_mode {
        (
            width.clamp(MINI_MIN_WIDTH, 520.0),
            height.clamp(MINI_MIN_HEIGHT, 200.0),
        )
    } else {
        (
            width.clamp(NORMAL_MIN_WIDTH, 4096.0),
            height.clamp(NORMAL_MIN_HEIGHT, 2160.0),
        )
    }
}

fn minimum_size(mini_mode: bool) -> LogicalSize<f64> {
    if mini_mode {
        LogicalSize::new(MINI_MIN_WIDTH, MINI_MIN_HEIGHT)
    } else {
        LogicalSize::new(NORMAL_MIN_WIDTH, NORMAL_MIN_HEIGHT)
    }
}

fn default_size_for_monitor(logical_width: f64, logical_height: f64) -> LogicalSize<f64> {
    let aspect_ratio = DEFAULT_WIDTH / DEFAULT_HEIGHT;
    let mut width = (logical_width * 0.88).min(DEFAULT_WIDTH);
    let mut height = width / aspect_ratio;
    let maximum_height = logical_height * 0.86;
    if height > maximum_height {
        height = maximum_height;
        width = height * aspect_ratio;
    }

    LogicalSize::new(
        width.max(NORMAL_MIN_WIDTH).floor(),
        height.max(NORMAL_MIN_HEIGHT).floor(),
    )
}

pub fn apply_minimum_size(window: &WebviewWindow, mini_mode: bool) -> Result<(), String> {
    window
        .set_min_size(Some(minimum_size(mini_mode)))
        .map_err(|error| error.to_string())
}

pub fn apply_default_placement(window: &WebviewWindow) {
    let _ = apply_minimum_size(window, false);
    if let Ok(Some(monitor)) = window
        .current_monitor()
        .or_else(|_| window.primary_monitor())
    {
        let scale = monitor.scale_factor();
        let physical = monitor.size();
        let logical_width = physical.width as f64 / scale;
        let logical_height = physical.height as f64 / scale;

        let _ = window.set_size(default_size_for_monitor(logical_width, logical_height));
    } else {
        let _ = window.set_size(LogicalSize::new(DEFAULT_WIDTH, DEFAULT_HEIGHT));
    }
    let _ = window.center();
    let _ = window.set_always_on_top(false);
}

pub fn apply_to_window(window: &WebviewWindow, state: &WindowState) {
    let monitors = monitors_for(window);
    let mini = state.mini_mode;
    let _ = apply_minimum_size(window, mini);
    let (width, height) = clamp_size(
        state.width.unwrap_or(DEFAULT_WIDTH),
        state.height.unwrap_or(DEFAULT_HEIGHT),
        mini,
    );

    let position_ok = match (state.x, state.y) {
        (Some(x), Some(y)) => geometry_is_visible(x, y, width, height, &monitors, 80.0),
        _ => false,
    };

    if !position_ok && !state.maximized {
        // Bad / missing coordinates — safe centered default (still apply always-on-top flag).
        if mini {
            let _ = window.set_size(LogicalSize::new(MINI_DEFAULT_WIDTH, MINI_DEFAULT_HEIGHT));
            let _ = window.set_always_on_top(true);
            let _ = window.center();
        } else {
            apply_default_placement(window);
            let _ = window.set_always_on_top(state.always_on_top);
        }
        return;
    }

    if mini {
        let _ = window.set_size(LogicalSize::new(MINI_DEFAULT_WIDTH, MINI_DEFAULT_HEIGHT));
        let _ = window.set_always_on_top(true);
    } else {
        let _ = window.set_size(LogicalSize::new(width, height));
        let _ = window.set_always_on_top(state.always_on_top);
    }

    if let (Some(x), Some(y)) = (state.x, state.y)
        && position_ok
    {
        let _ = window.set_position(LogicalPosition::new(x, y));
    } else {
        let _ = window.center();
    }

    if state.maximized && !mini {
        let _ = window.maximize();
    }
}

/// Capture geometry only when the window is visible and not minimized.
pub fn capture_from_window(window: &WebviewWindow, mini_mode: bool) -> Option<WindowState> {
    if window.is_minimized().unwrap_or(false) {
        return None;
    }
    if !window.is_visible().unwrap_or(false) {
        return None;
    }

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

    let scale = window.scale_factor().ok()?;
    if let Ok(position) = window.outer_position() {
        let PhysicalPosition { x, y } = position;
        state.x = Some(x as f64 / scale);
        state.y = Some(y as f64 / scale);
    }
    if let Ok(size) = window.outer_size() {
        let PhysicalSize { width, height } = size;
        state.width = Some(width as f64 / scale);
        state.height = Some(height as f64 / scale);
    }

    // Refuse to persist garbage (minimized sentinels / off-screen).
    if let (Some(x), Some(y), Some(w), Some(h)) = (state.x, state.y, state.width, state.height) {
        let monitors = monitors_for(window);
        if !geometry_is_visible(x, y, w, h, &monitors, 40.0) && !maximized {
            return None;
        }
    }
    Some(state)
}

pub fn app_data_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path().app_data_dir().map_err(|e| e.to_string())
}

/// Force the main window onto the primary work area (recovery command).
pub fn force_show_main(window: &WebviewWindow, app_data: &Path) {
    let _ = window.unminimize();
    let _ = apply_minimum_size(window, false);
    apply_default_placement(window);
    let _ = clear_geometry(app_data);
    let _ = window.show();
    let _ = window.set_focus();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_and_mini_sizes_use_distinct_constraints() {
        assert_eq!(clamp_size(100.0, 100.0, false), (320.0, 560.0));
        assert_eq!(clamp_size(100.0, 100.0, true), (320.0, 120.0));
        assert_eq!(
            clamp_size(MINI_DEFAULT_WIDTH, MINI_DEFAULT_HEIGHT, true),
            (MINI_DEFAULT_WIDTH, MINI_DEFAULT_HEIGHT)
        );
    }

    #[test]
    fn a_saved_320px_normal_window_is_not_expanded_back_to_720px() {
        assert_eq!(clamp_size(320.0, 560.0, false), (320.0, 560.0));
    }

    #[test]
    fn a_saved_normal_window_size_is_not_replaced_by_the_first_launch_default() {
        assert_eq!(clamp_size(1180.0, 720.0, false), (1180.0, 720.0));
    }

    #[test]
    fn minimum_size_matches_the_active_window_mode() {
        assert_eq!(
            minimum_size(false),
            LogicalSize::new(NORMAL_MIN_WIDTH, NORMAL_MIN_HEIGHT)
        );
        assert_eq!(
            minimum_size(true),
            LogicalSize::new(MINI_MIN_WIDTH, MINI_MIN_HEIGHT)
        );
    }

    #[test]
    fn first_launch_uses_the_compact_default_on_a_large_monitor() {
        assert_eq!(
            default_size_for_monitor(1920.0, 1080.0),
            LogicalSize::new(DEFAULT_WIDTH, DEFAULT_HEIGHT)
        );
    }

    #[test]
    fn first_launch_default_scales_down_to_fit_a_short_monitor() {
        assert_eq!(
            default_size_for_monitor(1280.0, 720.0),
            LogicalSize::new(990.0, 619.0)
        );
    }
}
