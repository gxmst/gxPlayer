//! Product-facing commands: history, covers, window chrome, backup files, sleep is frontend-only.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

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
const MAX_BACKUP_FILE_BYTES: usize = 32 * 1024 * 1024;
const MAX_STAGING_PATH_ATTEMPTS: usize = 128;
static BACKUP_STAGING_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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
    use std::io::Cursor;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    fn backup_test_root(label: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "gxplayer-product-backup-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

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

    #[test]
    fn atomic_backup_write_replaces_content_without_leaving_staging_files() {
        let root = backup_test_root("replace");
        let path = root.join("gxplayer-backup.json");
        fs::create_dir_all(&root).unwrap();
        fs::write(&path, b"old backup").unwrap();

        write_backup_file_atomic(&path, b"new backup").unwrap();

        assert_eq!(read_backup_file_limited(&path).unwrap(), "new backup");
        let entries = fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(entries, [path.file_name().unwrap().to_os_string()]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn atomic_replacement_uses_one_namespace_operation() {
        let root = backup_test_root("single-rename");
        let path = root.join("gxplayer-backup.json");
        let staged = root.join("staged.tmp");
        fs::create_dir_all(&root).unwrap();
        fs::write(&path, b"old backup").unwrap();
        fs::write(&staged, b"new backup").unwrap();
        let mut rename_count = 0;

        replace_staged_file_with(&staged, &path, |from, to| {
            rename_count += 1;
            fs::rename(from, to)
        })
        .unwrap();

        assert_eq!(rename_count, 1);
        assert_eq!(fs::read(&path).unwrap(), b"new backup");
        assert!(!staged.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_atomic_replacement_keeps_the_previous_backup() {
        let root = backup_test_root("failed-rename");
        let path = root.join("gxplayer-backup.json");
        let staged = root.join("staged.tmp");
        fs::create_dir_all(&root).unwrap();
        fs::write(&path, b"old backup").unwrap();
        fs::write(&staged, b"new backup").unwrap();
        let mut rename_count = 0;

        let error = replace_staged_file_with(&staged, &path, |from, to| {
            assert_eq!(from, staged);
            assert_eq!(to, path);
            rename_count += 1;
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "replace denied",
            ))
        })
        .unwrap_err();

        assert_eq!(rename_count, 1);
        assert!(error.contains("目标未改动"));
        assert_eq!(fs::read(&path).unwrap(), b"old backup");
        assert!(!staged.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn backup_write_refuses_to_replace_a_non_file_target() {
        let root = backup_test_root("non-file");
        let path = root.join("gxplayer-backup.json");
        let sentinel = path.join("keep.txt");
        fs::create_dir_all(&path).unwrap();
        fs::write(&sentinel, b"keep").unwrap();

        let error = write_backup_file_atomic(&path, b"new backup").unwrap_err();

        assert!(error.contains("普通文件"));
        assert_eq!(fs::read(&sentinel).unwrap(), b"keep");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn backup_read_checks_metadata_before_allocating() {
        let root = backup_test_root("oversized");
        let path = root.join("oversized.json");
        fs::create_dir_all(&root).unwrap();
        File::create(&path)
            .unwrap()
            .set_len(MAX_BACKUP_FILE_BYTES as u64 + 1)
            .unwrap();

        assert!(
            read_backup_file_limited(&path)
                .unwrap_err()
                .contains("过大")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn limited_reader_detects_a_file_that_grows_after_metadata() {
        let error = read_bytes_limited(Cursor::new(b"123456789"), 8, 8).unwrap_err();
        assert!(error.contains("读取时超过"));
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

fn backup_staging_path(target: &Path, label: &str) -> Result<PathBuf, String> {
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let target_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("backup");

    for _ in 0..MAX_STAGING_PATH_ATTEMPTS {
        let sequence = BACKUP_STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(
            ".{target_name}.gxplayer-{label}-{}-{sequence}.tmp",
            std::process::id()
        ));
        match fs::symlink_metadata(&candidate) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(candidate),
            Ok(_) => continue,
            Err(error) => return Err(format!("无法检查备份临时路径: {error}")),
        }
    }

    Err("无法创建唯一的备份临时路径".into())
}

fn create_backup_staging_file(target: &Path) -> Result<(PathBuf, File), String> {
    for _ in 0..MAX_STAGING_PATH_ATTEMPTS {
        let path = backup_staging_path(target, "staging")?;
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("无法创建备份临时文件: {error}")),
        }
    }

    Err("无法创建唯一的备份临时文件".into())
}

fn remove_staging_file(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("无法清理备份临时文件 {}: {error}", path.display())),
    }
}

fn cleanup_staging_after_error(message: String, staged: &Path) -> String {
    match remove_staging_file(staged) {
        Ok(()) => message,
        Err(cleanup_error) => format!("{message}；{cleanup_error}"),
    }
}

fn replace_staged_file_with<F>(staged: &Path, target: &Path, mut rename: F) -> Result<(), String>
where
    F: FnMut(&Path, &Path) -> io::Result<()>,
{
    match fs::symlink_metadata(target) {
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err(cleanup_staging_after_error(
                "备份目标不是普通文件，已拒绝覆盖".into(),
                staged,
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(cleanup_staging_after_error(
                format!("无法检查现有备份: {error}"),
                staged,
            ));
        }
    }

    match rename(staged, target) {
        Ok(()) => Ok(()),
        Err(error) => Err(cleanup_staging_after_error(
            format!("无法原子提交备份文件，目标未改动: {error}"),
            staged,
        )),
    }
}

fn replace_staged_file(staged: &Path, target: &Path) -> Result<(), String> {
    replace_staged_file_with(staged, target, |from, to| fs::rename(from, to))
}

fn write_backup_file_atomic(path: &Path, content: &[u8]) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err("备份目标不是普通文件，已拒绝覆盖".into());
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("无法检查备份目标: {error}")),
    }

    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|error| format!("无法创建备份目录: {error}"))?;
    }

    let (staged_path, mut staged_file) = create_backup_staging_file(path)?;
    let write_result = (|| -> io::Result<()> {
        staged_file.write_all(content)?;
        staged_file.flush()?;
        staged_file.sync_all()
    })();
    drop(staged_file);

    if let Err(error) = write_result {
        return Err(cleanup_staging_after_error(
            format!("无法完整写入备份临时文件: {error}"),
            &staged_path,
        ));
    }

    replace_staged_file(&staged_path, path)
}

fn read_bytes_limited<R: Read>(
    reader: R,
    metadata_len: u64,
    limit: usize,
) -> Result<Vec<u8>, String> {
    if metadata_len > limit as u64 {
        return Err("备份文件过大".into());
    }

    let mut bytes = Vec::with_capacity(metadata_len as usize);
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("无法读取备份文件: {error}"))?;
    if bytes.len() > limit {
        return Err("备份文件读取时超过大小限制".into());
    }
    Ok(bytes)
}

fn read_backup_file_limited(path: &Path) -> Result<String, String> {
    let path_metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !path_metadata.file_type().is_file() {
        return Err("备份路径不是普通文件".into());
    }
    if path_metadata.len() > MAX_BACKUP_FILE_BYTES as u64 {
        return Err("备份文件过大".into());
    }

    let file = File::open(path).map_err(|error| error.to_string())?;
    let file_metadata = file.metadata().map_err(|error| error.to_string())?;
    if !file_metadata.file_type().is_file() {
        return Err("备份路径不是普通文件".into());
    }
    let bytes = read_bytes_limited(file, file_metadata.len(), MAX_BACKUP_FILE_BYTES)?;
    String::from_utf8(bytes).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn backup_write_file(
    window: WebviewWindow,
    path: String,
    content: String,
) -> Result<(), String> {
    require_window(&window, "main")?;
    if path.len() > 1024 || content.len() > MAX_BACKUP_FILE_BYTES {
        return Err("备份内容或路径超出限制".into());
    }
    let path = PathBuf::from(path);
    write_backup_file_atomic(&path, content.as_bytes())
}

#[tauri::command]
pub fn backup_read_file(window: WebviewWindow, path: String) -> Result<String, String> {
    require_window(&window, "main")?;
    if path.len() > 1024 {
        return Err("路径过长".into());
    }
    read_backup_file_limited(Path::new(&path))
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
