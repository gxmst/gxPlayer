//! Product-facing commands: history, covers, window chrome, backup files, sleep is frontend-only.

use std::fs;
use std::path::PathBuf;

use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use gx_library::{HistoryEntry, LibraryStore, LibraryTrack, NewHistoryEntry};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State, WebviewWindow};

use crate::require_window;
use crate::transport::{TransportAction, dispatch};
use crate::window_state::{self, WindowState};

const MAX_LOCAL_PATH_CHECKS: usize = 10_000;
const MAX_LOCAL_PATH_BYTES: usize = 32 * 1024;
const MAX_LOCAL_PATH_BATCH_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverPayload {
    pub mime: String,
    pub data_url: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryRecordRequest {
    pub kind: String,
    pub title: String,
    pub artist: String,
    pub path: Option<String>,
    pub provider_id: Option<String>,
    pub provider_track_id: Option<String>,
    pub quality: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LocalPathAvailability {
    pub path: String,
    pub available: bool,
}

fn validate_local_path_batch(paths: &[String]) -> Result<(), String> {
    if paths.len() > MAX_LOCAL_PATH_CHECKS {
        return Err(format!("单次最多检查 {MAX_LOCAL_PATH_CHECKS} 个本地路径"));
    }
    let mut total_bytes = 0usize;
    for path in paths {
        if path.trim().is_empty() {
            return Err("本地路径不能为空".into());
        }
        if path.len() > MAX_LOCAL_PATH_BYTES {
            return Err("本地路径过长".into());
        }
        total_bytes = total_bytes
            .checked_add(path.len())
            .ok_or_else(|| "本地路径总长度超出限制".to_owned())?;
        if total_bytes > MAX_LOCAL_PATH_BATCH_BYTES {
            return Err("本地路径总长度超出限制".into());
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn library_check_local_paths(
    window: WebviewWindow,
    paths: Vec<String>,
) -> Result<Vec<LocalPathAvailability>, String> {
    require_window(&window, "main")?;
    validate_local_path_batch(&paths)?;
    tauri::async_runtime::spawn_blocking(move || {
        paths
            .into_iter()
            .map(|path| LocalPathAvailability {
                available: PathBuf::from(&path).is_file(),
                path,
            })
            .collect()
    })
    .await
    .map_err(|error| format!("本地路径检查任务失败: {error}"))
}

#[tauri::command]
pub fn library_scan_missing(
    window: WebviewWindow,
    library: State<'_, LibraryStore>,
) -> Result<Vec<LibraryTrack>, String> {
    require_window(&window, "main")?;
    library.scan_missing().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_path_batch_limits_count_individual_and_total_size() {
        assert!(validate_local_path_batch(&["C:/Music/song.flac".into()]).is_ok());
        assert!(validate_local_path_batch(&[String::new()]).is_err());
        assert!(validate_local_path_batch(&vec!["x".into(); MAX_LOCAL_PATH_CHECKS + 1]).is_err());
        assert!(validate_local_path_batch(&["x".repeat(MAX_LOCAL_PATH_BYTES + 1)]).is_err());
        assert!(
            validate_local_path_batch(&vec![
                "x".repeat(MAX_LOCAL_PATH_BYTES);
                MAX_LOCAL_PATH_BATCH_BYTES / MAX_LOCAL_PATH_BYTES + 1
            ])
            .is_err()
        );
    }
}

#[tauri::command]
pub fn library_history(
    window: WebviewWindow,
    library: State<'_, LibraryStore>,
    limit: Option<usize>,
) -> Result<Vec<HistoryEntry>, String> {
    require_window(&window, "main")?;
    library
        .list_history(limit.unwrap_or(100))
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn library_clear_history(
    window: WebviewWindow,
    library: State<'_, LibraryStore>,
) -> Result<(), String> {
    require_window(&window, "main")?;
    library.clear_history().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn library_record_history(
    window: WebviewWindow,
    library: State<'_, LibraryStore>,
    entry: HistoryRecordRequest,
) -> Result<(), String> {
    require_window(&window, "main")?;
    library
        .record_history(NewHistoryEntry {
            kind: &entry.kind,
            title: &entry.title,
            artist: &entry.artist,
            path: entry.path.as_deref(),
            provider_id: entry.provider_id.as_deref(),
            provider_track_id: entry.provider_track_id.as_deref(),
            quality: entry.quality.as_deref(),
        })
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn library_embedded_cover(
    window: WebviewWindow,
    path: String,
) -> Result<Option<CoverPayload>, String> {
    require_window(&window, "main")?;
    if path.len() > 1024 {
        return Err("path too long".into());
    }
    let cover =
        gx_audio::extract_embedded_cover(PathBuf::from(&path)).map_err(|e| e.to_string())?;
    Ok(cover.map(|c| CoverPayload {
        mime: c.mime.clone(),
        data_url: format!("data:{};base64,{}", c.mime, B64.encode(&c.data)),
    }))
}

#[tauri::command]
pub fn window_get_state(window: WebviewWindow, app: AppHandle) -> Result<WindowState, String> {
    require_window(&window, "main")?;
    let app_data = window_state::app_data_dir(&app)?;
    Ok(window_state::load(&app_data))
}

#[tauri::command]
pub fn window_save_state(
    window: WebviewWindow,
    app: AppHandle,
    mode: State<'_, window_state::WindowModeState>,
    mini_mode: Option<bool>,
) -> Result<WindowState, String> {
    require_window(&window, "main")?;
    let _ = mini_mode; // retained for IPC compatibility; backend runtime state is authoritative.
    let app_data = window_state::app_data_dir(&app)?;
    let previous = window_state::load(&app_data);
    // A mini-mode resize event must never overwrite the remembered normal geometry. The backend
    // state is authoritative even if the frontend event closure still holds the old boolean.
    if mode.mini_mode() {
        let mut state = previous;
        state.mini_mode = true;
        window_state::save(&app_data, &state)?;
        return Ok(state);
    }
    // Skip save when minimized/hidden/off-screen so we never persist garbage coords.
    if let Some(state) = window_state::capture_from_window(&window, false) {
        window_state::save(&app_data, &state)?;
        Ok(state)
    } else {
        Ok(previous)
    }
}

/// Recover a missing window: center on primary display and clear bad geometry.
#[tauri::command]
pub fn window_force_show(
    window: WebviewWindow,
    app: AppHandle,
    mode: State<'_, window_state::WindowModeState>,
) -> Result<(), String> {
    require_window(&window, "main")?;
    let app_data = window_state::app_data_dir(&app)?;
    mode.set_mini_mode(false);
    window_state::force_show_main(&window, &app_data);
    Ok(())
}

#[tauri::command]
pub fn window_set_always_on_top(
    window: WebviewWindow,
    app: AppHandle,
    mode: State<'_, window_state::WindowModeState>,
    enabled: bool,
) -> Result<(), String> {
    require_window(&window, "main")?;
    window
        .set_always_on_top(enabled || mode.mini_mode())
        .map_err(|e| e.to_string())?;
    let app_data = window_state::app_data_dir(&app)?;
    let mut state = window_state::load(&app_data);
    state.always_on_top = enabled;
    window_state::save(&app_data, &state)?;
    Ok(())
}

#[tauri::command]
pub fn window_set_mini_mode(
    window: WebviewWindow,
    app: AppHandle,
    mode: State<'_, window_state::WindowModeState>,
    enabled: bool,
) -> Result<(), String> {
    require_window(&window, "main")?;
    let app_data = window_state::app_data_dir(&app)?;
    let mut state = window_state::load(&app_data);
    if enabled {
        if !state.mini_mode {
            // Remember normal size before shrinking.
            if let Some(captured) = window_state::capture_from_window(&window, false) {
                state.x = captured.x.or(state.x);
                state.y = captured.y.or(state.y);
                state.width = captured.width.or(state.width);
                state.height = captured.height.or(state.height);
            }
        }
        let previous_runtime_mode = mode.mini_mode();
        let _ = window.unmaximize();
        window_state::apply_minimum_size(&window, true)?;
        // Resize events can fire before this command returns. Mark mini mode first so those events
        // never overwrite the remembered normal geometry with the compact dimensions.
        mode.set_mini_mode(true);
        if let Err(error) = window.set_size(tauri::LogicalSize::new(
            window_state::MINI_DEFAULT_WIDTH,
            window_state::MINI_DEFAULT_HEIGHT,
        )) {
            mode.set_mini_mode(previous_runtime_mode);
            let _ = window_state::apply_minimum_size(&window, previous_runtime_mode);
            return Err(error.to_string());
        }
        if let Err(error) = window.set_always_on_top(true) {
            mode.set_mini_mode(previous_runtime_mode);
            let _ = window_state::apply_minimum_size(&window, previous_runtime_mode);
            return Err(error.to_string());
        }
        let _ = window.center();
        state.mini_mode = true;
    } else {
        state.mini_mode = false;
        let width = state
            .width
            .unwrap_or(window_state::DEFAULT_WIDTH)
            .max(window_state::NORMAL_MIN_WIDTH);
        let height = state
            .height
            .unwrap_or(window_state::DEFAULT_HEIGHT)
            .max(window_state::NORMAL_MIN_HEIGHT);
        window
            .set_size(tauri::LogicalSize::new(width, height))
            .map_err(|e| e.to_string())?;
        window_state::apply_minimum_size(&window, false)?;
        window
            .set_always_on_top(state.always_on_top)
            .map_err(|e| e.to_string())?;
        if let (Some(x), Some(y)) = (state.x, state.y) {
            let _ = window.set_position(tauri::LogicalPosition::new(x, y));
        } else {
            let _ = window.center();
        }
        if state.maximized {
            window.maximize().map_err(|e| e.to_string())?;
        }
        // Keep mini authoritative throughout the resize above so its event cannot persist the
        // compact/transition geometry as the normal window size.
        mode.set_mini_mode(false);
    }
    window_state::save(&app_data, &state)?;
    Ok(())
}

#[tauri::command]
pub fn backup_write_file(
    window: WebviewWindow,
    path: String,
    content: String,
) -> Result<(), String> {
    require_window(&window, "main")?;
    if path.len() > 1024 || content.len() > 32 * 1024 * 1024 {
        return Err("备份内容或路径超出限制".into());
    }
    let path = PathBuf::from(path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::write(path, content).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn backup_read_file(window: WebviewWindow, path: String) -> Result<String, String> {
    require_window(&window, "main")?;
    if path.len() > 1024 {
        return Err("路径过长".into());
    }
    let bytes = fs::read(PathBuf::from(path)).map_err(|e| e.to_string())?;
    if bytes.len() > 32 * 1024 * 1024 {
        return Err("备份文件过大".into());
    }
    String::from_utf8(bytes).map_err(|e| e.to_string())
}

/// Soft media-command bridge used by future SMTC / hotkeys.
#[tauri::command]
pub fn player_media_action(
    window: WebviewWindow,
    app: AppHandle,
    action: String,
) -> Result<(), String> {
    require_window(&window, "main")?;
    dispatch(&app, TransportAction::try_from(action.as_str())?)
}
