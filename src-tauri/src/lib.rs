use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use gx_audio::engine::{AudioMode, EngineSnapshot, LocalAudioEngine, PlayMode};
use gx_contracts::ResolvedMediaRequest;
use gx_dsp::DspSettings;
use gx_library::{LibraryBackup, LibraryStore, LibraryTrack, NewTrack, PlaylistSummary};
use gx_source::{SourceStore, safe_http};

mod app_preferences;
mod artwork;
mod backup_commands;
mod cache_commands;
mod diagnostic_log;
mod media_session;
mod metadata_commands;
mod network_settings;
mod product_commands;
mod source_commands;
mod source_runtime;
mod taskbar_toolbar;
mod transport;
mod window_state;
mod windows_identity;

use app_preferences::{AppPreferences, AppPreferencesState, CloseAction, CloseBehavior};
use artwork::artwork_get;
use backup_commands::{backup_preview_restore, backup_restore_atomic};
use cache_commands::{
    cache_clear, cache_list_entries, cache_online_favorites, cache_remove_by_quality,
    cache_remove_entries, cache_remove_entry, cache_reset_directory, cache_set_directory,
    cache_set_limit, cache_set_online_favorite, cache_status, player_play_cache_entry,
};
use diagnostic_log::{
    DiagnosticLogState, diagnostic_log_clear, diagnostic_log_export, diagnostic_log_recent,
    diagnostic_log_set_enabled, diagnostic_log_status,
};
use metadata_commands::{
    maybe_start_phase3_smoke, metadata_chart, metadata_find_replacements, metadata_lyrics,
    metadata_play_preview, metadata_search,
};
use network_settings::{network_proxy_status, network_set_proxy_mode};
use product_commands::{
    backup_read_file, backup_write_file, library_check_local_paths, library_clear_history,
    library_embedded_cover, library_history, library_record_history, library_scan_missing,
    player_media_action, window_force_show, window_get_state, window_save_state,
    window_set_always_on_top, window_set_mini_mode,
};
use source_commands::{
    ResolveCancellationRegistry, lx_http_request, lx_runtime_failure, lx_runtime_result, lx_send,
    player_cancel_resolve, player_play_online_track, source_activate, source_export_backup,
    source_get_config, source_get_fallback_config, source_import_file, source_import_url,
    source_list, source_reimport, source_reload, source_remove, source_resolve,
    source_restore_backup, source_set_config, source_set_enabled, source_set_fallback_config,
    source_set_order, source_set_updates_enabled, source_status,
};
use source_runtime::SourceRuntime;

pub(crate) const SANDBOX_LABEL: &str = "lx-sandbox";

#[derive(Default)]
struct AppCloseRequestState {
    notice_pending: AtomicBool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OutputDeviceStatus {
    devices: Vec<String>,
    default_device: Option<String>,
    selected_device: Option<String>,
}

pub(crate) fn isolated_smoke_data_root() -> Option<PathBuf> {
    (std::env::var_os("GX_PHASE1_LX_POC").is_some()
        || std::env::var_os("GX_PHASE2_AUTO_EXIT").is_some()
        || std::env::var_os("GX_PHASE3_AUTO_EXIT").is_some())
    .then(|| {
        std::env::temp_dir()
            .join("gxplayer-smoke")
            .join(std::process::id().to_string())
    })
}

pub(crate) struct LxPocState {
    pub(crate) script_path: PathBuf,
    progress: Mutex<LxPocProgress>,
}

#[derive(Default)]
struct LxPocProgress {
    music_url_passed: bool,
    crypto_passed: bool,
    security_passed: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LxHttpResponse {
    pub(crate) status_code: u16,
    pub(crate) headers: std::collections::BTreeMap<String, String>,
    pub(crate) body: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SecurityResults {
    main_command_blocked: bool,
    source_command_blocked: bool,
    opener_blocked: bool,
    new_window_blocked: bool,
    file_blocked: bool,
    shell_blocked: bool,
    clipboard_blocked: bool,
    ssrf_blocked: bool,
}

pub(crate) fn require_window(window: &WebviewWindow, expected: &str) -> Result<(), String> {
    if window.label() == expected {
        Ok(())
    } else {
        Err(format!(
            "window '{}' is not authorized for this command",
            window.label()
        ))
    }
}

#[tauri::command]
fn main_only_probe(window: WebviewWindow) -> Result<&'static str, String> {
    require_window(&window, "main")?;
    Ok("main-only")
}

#[tauri::command]
fn ui_ready(window: WebviewWindow, app: AppHandle) -> Result<(), String> {
    require_window(&window, "main")?;
    println!("GX_PHASE0_UI_READY");
    if std::env::var_os("GX_PHASE0_UI_SMOKE").is_some() {
        app.exit(0);
    }
    maybe_start_phase3_smoke(&app);
    Ok(())
}

#[tauri::command]
fn player_set_transport_capabilities(
    window: WebviewWindow,
    state: tauri::State<transport::TransportState>,
    capabilities: transport::TransportCapabilities,
) -> Result<(), String> {
    require_window(&window, "main")?;
    state.set_capabilities(capabilities);
    Ok(())
}

#[tauri::command]
fn player_load_local(
    window: WebviewWindow,
    engine: tauri::State<LocalAudioEngine>,
    paths: Vec<String>,
    start_index: Option<usize>,
) -> Result<(), String> {
    require_window(&window, "main")?;
    let paths = paths.into_iter().map(PathBuf::from).collect::<Vec<_>>();
    if paths.is_empty() {
        return Err("至少需要一首本地音频".into());
    }
    let start_index = start_index.unwrap_or(0);
    if start_index >= paths.len() {
        return Err(format!(
            "start_index {start_index} 超出队列长度 {}",
            paths.len()
        ));
    }
    engine
        .load_at(paths, start_index)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn player_enqueue_local(
    window: WebviewWindow,
    engine: tauri::State<LocalAudioEngine>,
    paths: Vec<String>,
) -> Result<(), String> {
    require_window(&window, "main")?;
    let paths = paths.into_iter().map(PathBuf::from).collect::<Vec<_>>();
    if paths.is_empty() {
        return Err("至少需要一首本地音频".into());
    }
    engine.enqueue(paths).map_err(|error| error.to_string())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LibraryImportFailure {
    path: String,
    error: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LibraryImportResult {
    imported: Vec<LibraryTrack>,
    failures: Vec<LibraryImportFailure>,
}

#[tauri::command]
async fn library_import_files(
    window: WebviewWindow,
    paths: Vec<String>,
) -> Result<LibraryImportResult, String> {
    require_window(&window, "main")?;
    if paths.len() > 10_000 {
        return Err("单次最多导入 10000 个本地音频文件".into());
    }
    let app = window.app_handle().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let library = app.state::<LibraryStore>();
        import_local_files(&library, paths)
    })
    .await
    .map_err(|error| format!("本地音乐导入任务失败: {error}"))?
}

#[tauri::command]
async fn library_relink_track(
    window: WebviewWindow,
    old_path: String,
    new_path: String,
) -> Result<LibraryTrack, String> {
    require_window(&window, "main")?;
    if old_path.trim().is_empty() || new_path.trim().is_empty() {
        return Err("原路径和新路径不能为空".into());
    }
    if old_path.len() > 32 * 1024 || new_path.len() > 32 * 1024 {
        return Err("本地音频路径过长".into());
    }

    let app = window.app_handle().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let path = PathBuf::from(&new_path);
        let info = gx_audio::probe_local_file(&path).map_err(|error| error.to_string())?;
        let replacement = new_library_track(&path, info);
        app.state::<LibraryStore>()
            .relink_track(&old_path, &replacement)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("本地音乐重新定位任务失败: {error}"))?
}

fn import_local_files(
    library: &LibraryStore,
    paths: Vec<String>,
) -> Result<LibraryImportResult, String> {
    let mut seen = HashSet::new();
    let mut accepted = Vec::new();
    let mut failures = Vec::new();
    for raw_path in paths {
        if raw_path.trim().is_empty() {
            failures.push(LibraryImportFailure {
                path: raw_path,
                error: "文件路径为空".into(),
            });
            continue;
        }
        if !seen.insert(local_path_key(&raw_path)) {
            continue;
        }
        let path = PathBuf::from(&raw_path);
        let info = match gx_audio::probe_local_file(&path) {
            Ok(info) => info,
            Err(error) => {
                failures.push(LibraryImportFailure {
                    path: raw_path,
                    error: error.to_string(),
                });
                continue;
            }
        };
        accepted.push(new_library_track(&path, info));
    }

    library
        .upsert_tracks(&accepted)
        .map_err(|error| error.to_string())?;
    let imported = accepted
        .iter()
        .map(|track| {
            library
                .track_by_path(&track.path)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("导入后未能读取曲目: {}", track.path))
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(LibraryImportResult { imported, failures })
}

fn new_library_track(path: &Path, info: gx_audio::LocalMediaInfo) -> NewTrack {
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("未命名曲目");
    let (filename_artist, filename_title) = stem
        .split_once(" - ")
        .map_or(("", stem), |(artist, title)| (artist, title));
    NewTrack {
        path: path.display().to_string(),
        title: info.title.unwrap_or_else(|| filename_title.to_owned()),
        artist: info.artist.unwrap_or_else(|| filename_artist.to_owned()),
        album: info.album.unwrap_or_default(),
        duration_seconds: info.duration_seconds,
    }
}

fn local_path_key(path: &str) -> String {
    if cfg!(windows) {
        path.replace('/', "\\").to_lowercase()
    } else {
        path.to_owned()
    }
}

#[tauri::command]
fn player_jump(
    window: WebviewWindow,
    engine: tauri::State<LocalAudioEngine>,
    index: usize,
) -> Result<(), String> {
    require_window(&window, "main")?;
    engine.jump(index).map_err(|error| error.to_string())
}

#[tauri::command]
fn player_remove_queue_item(
    window: WebviewWindow,
    engine: tauri::State<LocalAudioEngine>,
    index: usize,
) -> Result<(), String> {
    require_window(&window, "main")?;
    engine
        .remove_queue_item(index)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn player_reorder_queue(
    window: WebviewWindow,
    engine: tauri::State<LocalAudioEngine>,
    from: usize,
    to: usize,
) -> Result<(), String> {
    require_window(&window, "main")?;
    engine
        .reorder_queue(from, to)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn player_clear_queue(
    window: WebviewWindow,
    engine: tauri::State<LocalAudioEngine>,
) -> Result<(), String> {
    require_window(&window, "main")?;
    engine.clear_queue().map_err(|error| error.to_string())
}

#[tauri::command]
fn player_set_play_mode(
    window: WebviewWindow,
    engine: tauri::State<LocalAudioEngine>,
    mode: PlayMode,
) -> Result<(), String> {
    require_window(&window, "main")?;
    engine
        .set_play_mode(mode)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn library_tracks(
    window: WebviewWindow,
    library: tauri::State<LibraryStore>,
) -> Result<Vec<LibraryTrack>, String> {
    require_window(&window, "main")?;
    library
        .list_tracks(10_000)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn library_favorites(
    window: WebviewWindow,
    library: tauri::State<LibraryStore>,
) -> Result<Vec<LibraryTrack>, String> {
    require_window(&window, "main")?;
    library.list_favorites().map_err(|error| error.to_string())
}

#[tauri::command]
fn library_set_favorite(
    window: WebviewWindow,
    library: tauri::State<LibraryStore>,
    track_id: i64,
    favorite: bool,
) -> Result<(), String> {
    require_window(&window, "main")?;
    library
        .set_favorite(track_id, favorite)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn library_playlists(
    window: WebviewWindow,
    library: tauri::State<LibraryStore>,
) -> Result<Vec<PlaylistSummary>, String> {
    require_window(&window, "main")?;
    library.list_playlists().map_err(|error| error.to_string())
}

#[tauri::command]
fn library_create_playlist(
    window: WebviewWindow,
    library: tauri::State<LibraryStore>,
    name: String,
) -> Result<PlaylistSummary, String> {
    require_window(&window, "main")?;
    library
        .create_playlist(&name)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn library_delete_playlist(
    window: WebviewWindow,
    library: tauri::State<LibraryStore>,
    playlist_id: i64,
) -> Result<(), String> {
    require_window(&window, "main")?;
    library
        .delete_playlist(playlist_id)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn library_playlist_tracks(
    window: WebviewWindow,
    library: tauri::State<LibraryStore>,
    playlist_id: i64,
) -> Result<Vec<LibraryTrack>, String> {
    require_window(&window, "main")?;
    library
        .playlist_tracks(playlist_id)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn library_add_to_playlist(
    window: WebviewWindow,
    library: tauri::State<LibraryStore>,
    playlist_id: i64,
    track_id: i64,
) -> Result<(), String> {
    require_window(&window, "main")?;
    library
        .add_to_playlist(playlist_id, track_id)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn library_remove_from_playlist(
    window: WebviewWindow,
    library: tauri::State<LibraryStore>,
    playlist_id: i64,
    track_id: i64,
) -> Result<(), String> {
    require_window(&window, "main")?;
    library
        .remove_from_playlist(playlist_id, track_id)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn library_export_backup(
    window: WebviewWindow,
    library: tauri::State<LibraryStore>,
) -> Result<LibraryBackup, String> {
    require_window(&window, "main")?;
    library.export_backup().map_err(|error| error.to_string())
}

#[tauri::command]
fn library_restore_backup(
    window: WebviewWindow,
    library: tauri::State<LibraryStore>,
    backup: LibraryBackup,
) -> Result<(), String> {
    require_window(&window, "main")?;
    library
        .restore_backup(&backup)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn player_load_resolved(
    window: WebviewWindow,
    engine: tauri::State<LocalAudioEngine>,
    request: ResolvedMediaRequest,
    title: String,
) -> Result<(), String> {
    require_window(&window, "main")?;
    engine
        .load_resolved(request, title)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn player_play(
    window: WebviewWindow,
    engine: tauri::State<LocalAudioEngine>,
) -> Result<(), String> {
    require_window(&window, "main")?;
    engine.play().map_err(|error| error.to_string())
}

#[tauri::command]
fn player_pause(
    window: WebviewWindow,
    engine: tauri::State<LocalAudioEngine>,
) -> Result<(), String> {
    require_window(&window, "main")?;
    engine.pause().map_err(|error| error.to_string())
}

#[tauri::command]
fn player_seek(
    window: WebviewWindow,
    engine: tauri::State<LocalAudioEngine>,
    seconds: f64,
) -> Result<(), String> {
    require_window(&window, "main")?;
    engine.seek(seconds).map_err(|error| error.to_string())
}

#[tauri::command]
fn player_set_volume(
    window: WebviewWindow,
    engine: tauri::State<LocalAudioEngine>,
    volume: f32,
) -> Result<(), String> {
    require_window(&window, "main")?;
    engine.set_volume(volume).map_err(|error| error.to_string())
}

#[tauri::command]
fn player_commit_volume(
    window: WebviewWindow,
    engine: tauri::State<LocalAudioEngine>,
    preferences: tauri::State<AppPreferencesState>,
    volume: f32,
) -> Result<AppPreferences, String> {
    require_window(&window, "main")?;
    let previous = preferences.get().volume;
    engine
        .set_volume(volume)
        .map_err(|error| error.to_string())?;
    match preferences.set_volume(volume) {
        Ok(next) => Ok(next),
        Err(error) => {
            let _ = engine.set_volume(previous);
            Err(error)
        }
    }
}

#[tauri::command]
fn player_set_dsp_settings(
    window: WebviewWindow,
    engine: tauri::State<LocalAudioEngine>,
    settings: DspSettings,
) -> Result<(), String> {
    require_window(&window, "main")?;
    engine
        .set_dsp_settings(settings)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn player_set_audio_mode(
    window: WebviewWindow,
    engine: tauri::State<LocalAudioEngine>,
    mode: AudioMode,
) -> Result<(), String> {
    require_window(&window, "main")?;
    engine
        .set_audio_mode(mode)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn player_next(
    window: WebviewWindow,
    engine: tauri::State<LocalAudioEngine>,
) -> Result<(), String> {
    require_window(&window, "main")?;
    engine.next().map_err(|error| error.to_string())
}

#[tauri::command]
fn player_previous(
    window: WebviewWindow,
    engine: tauri::State<LocalAudioEngine>,
) -> Result<(), String> {
    require_window(&window, "main")?;
    engine.previous().map_err(|error| error.to_string())
}

#[tauri::command]
fn player_snapshot(
    window: WebviewWindow,
    engine: tauri::State<LocalAudioEngine>,
) -> Result<EngineSnapshot, String> {
    require_window(&window, "main")?;
    Ok(engine.snapshot())
}

#[tauri::command]
fn player_output_devices(
    window: WebviewWindow,
    engine: tauri::State<LocalAudioEngine>,
) -> Result<Vec<String>, String> {
    require_window(&window, "main")?;
    engine.output_devices().map_err(|error| error.to_string())
}

fn output_device_status(
    engine: &LocalAudioEngine,
    preferences: &AppPreferencesState,
) -> Result<OutputDeviceStatus, String> {
    Ok(OutputDeviceStatus {
        devices: engine.output_devices().map_err(|error| error.to_string())?,
        default_device: engine
            .default_output_device_name()
            .map_err(|error| error.to_string())?,
        selected_device: preferences.get().output_device,
    })
}

#[tauri::command]
fn player_refresh_output_devices(
    window: WebviewWindow,
    engine: tauri::State<LocalAudioEngine>,
    preferences: tauri::State<AppPreferencesState>,
) -> Result<OutputDeviceStatus, String> {
    require_window(&window, "main")?;
    output_device_status(&engine, &preferences)
}

#[tauri::command]
fn player_set_output_device(
    window: WebviewWindow,
    engine: tauri::State<LocalAudioEngine>,
    preferences: tauri::State<AppPreferencesState>,
    name: Option<String>,
) -> Result<OutputDeviceStatus, String> {
    require_window(&window, "main")?;
    let devices = engine.output_devices().map_err(|error| error.to_string())?;
    if let Some(name) = name.as_deref()
        && !devices.iter().any(|device| device == name)
    {
        return Err(format!("输出设备“{name}”当前不可用，请刷新后重试"));
    }
    let previous = preferences.get().output_device;
    preferences.set_output_device(name.clone())?;
    if let Err(error) = engine.set_output_device(name) {
        let _ = preferences.set_output_device(previous);
        return Err(error.to_string());
    }
    output_device_status(&engine, &preferences)
}

#[tauri::command]
fn app_preferences_get(
    window: WebviewWindow,
    preferences: tauri::State<AppPreferencesState>,
) -> Result<AppPreferences, String> {
    require_window(&window, "main")?;
    Ok(preferences.get())
}

#[tauri::command]
fn app_preferences_set_close_behavior(
    window: WebviewWindow,
    preferences: tauri::State<AppPreferencesState>,
    behavior: CloseBehavior,
) -> Result<AppPreferences, String> {
    require_window(&window, "main")?;
    preferences.set_close_behavior(behavior)
}

#[tauri::command]
fn app_close_notice_confirm(
    window: WebviewWindow,
    preferences: tauri::State<AppPreferencesState>,
    close_request: tauri::State<AppCloseRequestState>,
) -> Result<AppPreferences, String> {
    require_window(&window, "main")?;
    let next = preferences.mark_close_notice_shown()?;
    close_request.notice_pending.store(false, Ordering::Release);
    save_main_window_state(window.app_handle());
    window.hide().map_err(|error| error.to_string())?;
    Ok(next)
}

#[tauri::command]
fn app_close_notice_cancel(
    window: WebviewWindow,
    close_request: tauri::State<AppCloseRequestState>,
) -> Result<(), String> {
    require_window(&window, "main")?;
    close_request.notice_pending.store(false, Ordering::Release);
    Ok(())
}

#[tauri::command]
fn sandbox_ready(
    window: WebviewWindow,
    runtime: tauri::State<SourceRuntime>,
    poc: tauri::State<LxPocState>,
) -> Result<(), String> {
    require_window(&window, SANDBOX_LABEL)?;
    source_commands::sandbox_became_ready(&window, &runtime, &poc)
}

pub(crate) fn phase1_http_mock(url: &str, options: &Value) -> Result<LxHttpResponse, String> {
    let parsed = reqwest::Url::parse(url).map_err(|error| format!("invalid URL: {error}"))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err("only HTTP(S) is allowed".into());
    }
    if parsed.username() != "" || parsed.password().is_some() {
        return Err("credentials in URLs are not allowed".into());
    }
    if options.to_string().len() > 64 * 1024 {
        return Err("HTTP options exceed the Phase-1 size limit".into());
    }
    safe_http::validate_and_resolve(&parsed)
        .or_else(|error| {
            if parsed.host_str() == Some("gx.invalid") {
                Ok("192.0.2.1:80".parse().unwrap())
            } else {
                Err(error)
            }
        })
        .map_err(|error| error.to_string())?;
    if parsed.host_str() != Some("gx.invalid") {
        return Err("Phase-1 sandbox HTTP is restricted to the deterministic mock host".into());
    }

    let body = if parsed.path() == "/" {
        json!({
            "version": "phase-1",
            "summary": { "StartAt": 1700000000, "Accessn": 1, "Request": 1, "Success": 1 },
            "msg": "Hello~::^-^::~v1~",
            "script": { "ver": "1.1.0", "url": "", "force": false, "log": "" },
            "auth": { "apikey": false },
            "source": { "wy": ["128k", "320k", "flac"] }
        })
    } else if parsed.path().starts_with("/url/wy/") {
        let media_url = if std::env::var_os("GX_PHASE2_LX_MOCK").is_some() {
            "https://www.soundhelix.com/examples/mp3/SoundHelix-Song-1.mp3"
        } else {
            "https://media.example/phase-1.mp3"
        };
        json!({
            "code": 0,
            "msg": "ok",
            "data": media_url
        })
    } else {
        return Err(format!("unexpected Phase-1 mock path: {}", parsed.path()));
    };

    Ok(LxHttpResponse {
        status_code: 200,
        headers: std::collections::BTreeMap::from([(
            "content-type".into(),
            "application/json".into(),
        )]),
        body,
    })
}

pub(crate) fn phase1_lx_send(
    window: &WebviewWindow,
    event_name: String,
    data: Value,
    _app: &AppHandle,
    _state: &LxPocState,
) -> Result<(), String> {
    match event_name.as_str() {
        "updateAlert" => Ok(()),
        "inited" => {
            let supports_wy = data
                .get("sources")
                .and_then(|sources| sources.get("wy"))
                .is_some();
            if !supports_wy {
                return Err("community script initialized without the mocked wy source".into());
            }
            let payload = json!({
                "source": "wy",
                "action": "musicUrl",
                "info": {
                    "type": "128k",
                    "musicInfo": { "hash": "phase1-track", "name": "Phase 1" }
                }
            });
            let payload = serde_json::to_string(&payload).map_err(|error| error.to_string())?;
            window
                .eval(format!(
                    "setTimeout(() => window.__gxDispatchRequest({payload}), 0)"
                ))
                .map_err(|error| error.to_string())
        }
        _ => Err(format!("unsupported lx.send event: {event_name}")),
    }
}

#[tauri::command]
fn lx_poc_result(
    window: WebviewWindow,
    app: AppHandle,
    state: tauri::State<LxPocState>,
    result: Value,
) -> Result<(), String> {
    require_window(&window, SANDBOX_LABEL)?;
    let url = result
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if url != "https://media.example/phase-1.mp3" {
        return Err(format!("unexpected community-script result: {result}"));
    }
    println!("GX_PHASE1_LX_MUSIC_URL_OK {url}");
    state.progress.lock().unwrap().music_url_passed = true;
    window
        .eval("window.__gxRunCryptoSelfTest(); window.__gxRunSecuritySelfTest();")
        .map_err(|error| error.to_string())?;
    maybe_finish(&app, &state);
    Ok(())
}

#[tauri::command]
fn lx_crypto_result(
    window: WebviewWindow,
    app: AppHandle,
    state: tauri::State<LxPocState>,
    passed: bool,
    details: Value,
) -> Result<(), String> {
    require_window(&window, SANDBOX_LABEL)?;
    if !passed {
        return Err(format!("synchronous crypto self-test failed: {details}"));
    }
    println!("GX_PHASE1_LX_SYNC_CRYPTO_OK {details}");
    state.progress.lock().unwrap().crypto_passed = true;
    maybe_finish(&app, &state);
    Ok(())
}

#[tauri::command]
fn lx_security_result(
    window: WebviewWindow,
    app: AppHandle,
    state: tauri::State<LxPocState>,
    results: SecurityResults,
) -> Result<(), String> {
    require_window(&window, SANDBOX_LABEL)?;
    if !(results.main_command_blocked
        && results.source_command_blocked
        && results.opener_blocked
        && results.new_window_blocked
        && results.file_blocked
        && results.shell_blocked
        && results.clipboard_blocked
        && results.ssrf_blocked)
    {
        return Err("sandbox security self-test did not block every forbidden action".into());
    }
    println!("GX_PHASE1_LX_SECURITY_OK");
    state.progress.lock().unwrap().security_passed = true;
    maybe_finish(&app, &state);
    Ok(())
}

#[tauri::command]
fn lx_poc_failure(
    window: WebviewWindow,
    app: AppHandle,
    stage: String,
    error: String,
) -> Result<(), String> {
    require_window(&window, SANDBOX_LABEL)?;
    eprintln!("GX_PHASE1_LX_FAILED stage={stage} error={error}");
    app.exit(2);
    Ok(())
}

fn maybe_finish(app: &AppHandle, state: &tauri::State<LxPocState>) {
    let progress = state.progress.lock().unwrap();
    if progress.music_url_passed && progress.crypto_passed && progress.security_passed {
        println!("GX_PHASE1_LX_SANDBOX_OK");
        if std::env::var_os("GX_PHASE1_AUTO_EXIT").is_some() {
            app.exit(0);
        }
    }
}

fn phase1_script_path() -> PathBuf {
    std::env::var_os("GX_LX_SCRIPT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(".phase1-cache/lx-script/dist/lx-source-script.js")
        })
}

/// Size and show the main window before first paint.
/// Restores saved geometry when present and on-screen; otherwise safe centered default.
fn place_and_show_main_window(
    window: &WebviewWindow,
    app_data: &std::path::Path,
) -> tauri::Result<()> {
    let saved = window_state::load(app_data);
    let has_saved =
        saved.width.is_some() || saved.x.is_some() || saved.mini_mode || saved.maximized;
    if has_saved {
        window_state::apply_to_window(window, &saved);
    } else {
        window_state::apply_default_placement(window);
    }

    let _ = window.unminimize();
    window.show()?;
    let _ = window.set_focus();
    Ok(())
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        // If a bad geometry made the window invisible, recover to center.
        if let Ok(app_data) = app.path().app_data_dir() {
            let state = window_state::load(&app_data);
            // Re-apply validation (no-op when already good).
            window_state::apply_to_window(&window, &state);
        }
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn save_main_window_state(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let Ok(app_data) = app.path().app_data_dir() else {
        return;
    };
    let mini_mode = app
        .try_state::<window_state::WindowModeState>()
        .is_some_and(|mode| mode.mini_mode());
    if mini_mode {
        let mut state = window_state::load(&app_data);
        state.mini_mode = true;
        let _ = window_state::save(&app_data, &state);
    } else if let Some(state) = window_state::capture_from_window(&window, false) {
        let _ = window_state::save(&app_data, &state);
    }
}

fn request_app_exit(app: &AppHandle) {
    save_main_window_state(app);
    app.exit(0);
}

fn create_system_tray(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "tray-show", "显示主界面", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "tray-quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;
    let icon = app
        .default_window_icon()
        .cloned()
        .ok_or_else(|| tauri::Error::AssetNotFound("default window icon".into()))?;

    TrayIconBuilder::new()
        .icon(icon)
        .tooltip("GXPlayer")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "tray-show" => show_main_window(app),
            "tray-quit" => request_app_exit(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

fn create_lx_sandbox(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    let sandbox =
        WebviewWindowBuilder::new(app, SANDBOX_LABEL, WebviewUrl::App("sandbox.html".into()))
            .title("GXPlayer LX Sandbox")
            .visible(false)
            .on_navigation(|url| {
                let internal_host = url.host_str().is_some_and(|host| {
                    host.eq_ignore_ascii_case("tauri.localhost")
                        || (cfg!(debug_assertions)
                            && host.eq_ignore_ascii_case("localhost")
                            && url.port_or_known_default() == Some(1420))
                });
                (url.scheme() == "tauri" || internal_host)
                    && url.path().trim_end_matches('/') == "/sandbox.html"
            })
            .on_new_window(|_, _| tauri::webview::NewWindowResponse::Deny)
            .build()?;
    let app_handle = app.clone();
    let ready_app = app.clone();
    let initial_generation = app.state::<SourceRuntime>().status().generation;
    tauri::async_runtime::spawn_blocking(move || {
        std::thread::sleep(std::time::Duration::from_secs(10));
        ready_app.state::<SourceRuntime>().fail_if_not_started(
            initial_generation,
            "LX sandbox runtime-ready timed out".into(),
        );
    });
    sandbox.on_window_event(move |event| {
        if matches!(event, tauri::WindowEvent::Destroyed) {
            app_handle
                .state::<SourceRuntime>()
                .fail_current("LX sandbox window was destroyed".into());
            if std::env::var_os("GX_PHASE1_LX_POC").is_none() {
                let app_for_thread = app_handle.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    let app_for_main = app_for_thread.clone();
                    let _ = app_for_thread.run_on_main_thread(move || {
                        if app_for_main.get_webview_window(SANDBOX_LABEL).is_none()
                            && let Err(error) = create_lx_sandbox(&app_for_main)
                        {
                            eprintln!("failed to rebuild LX sandbox: {error}");
                        }
                    });
                });
            }
        }
    });
    Ok(sandbox)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    windows_identity::initialize();
    let audio_engine = LocalAudioEngine::new().expect("failed to create local audio engine");
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(audio_engine)
        .manage(ResolveCancellationRegistry::default())
        .manage(media_session::MediaSessionState::default())
        .manage(transport::TransportState::default())
        .manage(AppCloseRequestState::default())
        .manage(LxPocState {
            script_path: phase1_script_path(),
            progress: Mutex::new(LxPocProgress::default()),
        })
        .on_window_event(|window, event| {
            if window.label() == "main"
                && let tauri::WindowEvent::CloseRequested { api, .. } = event
            {
                api.prevent_close();
                let app = window.app_handle();
                let preferences = app.state::<AppPreferencesState>();
                match preferences.close_action() {
                    CloseAction::Exit => request_app_exit(app),
                    CloseAction::Hide => {
                        save_main_window_state(app);
                        let _ = window.hide();
                    }
                    CloseAction::Explain => {
                        let close_request = app.state::<AppCloseRequestState>();
                        if close_request
                            .notice_pending
                            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                            .is_ok()
                            && window.emit("gx-close-to-tray-notice-requested", ()).is_err()
                        {
                            close_request.notice_pending.store(false, Ordering::Release);
                        }
                    }
                }
            }
        })
        .setup(|app| {
            let app_data = isolated_smoke_data_root().unwrap_or(app.path().app_data_dir()?);
            let preferences = AppPreferencesState::open(&app_data);
            let restored_preferences = preferences.get();
            let engine = app.state::<LocalAudioEngine>();
            engine
                .set_volume(restored_preferences.volume)
                .map_err(tauri::Error::Anyhow)?;
            if let Some(device) = restored_preferences.output_device.as_ref() {
                let available = engine.output_devices().unwrap_or_default();
                if available.iter().any(|candidate| candidate == device) {
                    engine
                        .set_output_device(Some(device.clone()))
                        .map_err(tauri::Error::Anyhow)?;
                } else if let Err(error) = preferences.clear_output_device_if_matches(device) {
                    eprintln!("failed to clear unavailable output device preference: {error}");
                }
            }
            app.manage(preferences);
            app.manage(artwork::ArtworkCache::new(
                app_data.join("artwork-cache"),
            ));
            app.manage(network_settings::NetworkSettingsState::open(&app_data));
            app.manage(DiagnosticLogState::open(&app_data));
            app.manage(window_state::WindowModeState::new(
                window_state::load(&app_data).mini_mode,
            ));
            app.manage(LibraryStore::open(app_data.join("library.sqlite3"))?);
            let cache_store = gx_cache::CacheStore::open(
                &app_data,
                std::env::current_exe().ok().as_deref(),
            )
            .inspect_err(|error| {
                diagnostic_log::record_diagnostic(
                    app.handle(),
                    "cache_open_failed",
                    Some("cache"),
                    error.to_string(),
                );
            })?;
            app.manage(cache_store);
            let source_root = app_data.join("sources");
            let drop_in_root = source_root.join("drop-in");
            let mut source_store = SourceStore::open(&source_root)?;
            match source_store.import_drop_in_dir(&drop_in_root) {
                Ok(report) => {
                    if report.discovered > 0 {
                        println!(
                            "LX drop-in scan completed: directory={} discovered={} imported={} already_present={} failed={} active_source_id={}",
                            drop_in_root.display(),
                            report.discovered,
                            report.imported.len(),
                            report.already_present.len(),
                            report.failures.len(),
                            report.active_source_id.as_deref().unwrap_or("none")
                        );
                    }
                    for failure in report.failures {
                        eprintln!(
                            "LX drop-in source skipped: path={} error={}",
                            failure.path.display(),
                            failure.error
                        );
                    }
                }
                Err(error) => eprintln!(
                    "LX drop-in directory scan failed: directory={} error={error}",
                    drop_in_root.display()
                ),
            }
            if let Some(path) = std::env::var_os("GX_PHASE2_LX_SCRIPT") {
                source_store.import_file(&PathBuf::from(path))?;
            }
            app.manage(SourceRuntime::new(source_store));
            create_lx_sandbox(app.handle())?;
            create_system_tray(app.handle())?;

            if let Err(error) = taskbar_toolbar::install(app.handle().clone()) {
                eprintln!("GX_TASKBAR unavailable: {error}");
            }

            if let Some(main) = app.get_webview_window("main") {
                // Fail soft: still show with tauri.conf fallback size if monitor probe fails.
                if let Err(error) = place_and_show_main_window(&main, &app_data) {
                    eprintln!("main window placement failed: {error}");
                    window_state::force_show_main(&main, &app_data);
                }
            }
            // SMTC after show so HWND is valid and visible.
            media_session::spawn_media_session(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            main_only_probe,
            ui_ready,
            network_proxy_status,
            network_set_proxy_mode,
            diagnostic_log_status,
            diagnostic_log_set_enabled,
            diagnostic_log_recent,
            diagnostic_log_clear,
            diagnostic_log_export,
            artwork_get,
            player_load_local,
            player_enqueue_local,
            player_load_resolved,
            player_play_online_track,
            player_cancel_resolve,
            player_play,
            player_pause,
            player_seek,
            player_set_volume,
            player_commit_volume,
            player_set_audio_mode,
            player_set_play_mode,
            player_set_dsp_settings,
            player_next,
            player_previous,
            player_jump,
            player_remove_queue_item,
            player_reorder_queue,
            player_clear_queue,
            player_snapshot,
            player_set_transport_capabilities,
            player_output_devices,
            player_refresh_output_devices,
            player_set_output_device,
            app_preferences_get,
            app_preferences_set_close_behavior,
            app_close_notice_confirm,
            app_close_notice_cancel,
            player_media_action,
            library_import_files,
            library_relink_track,
            library_tracks,
            library_favorites,
            library_set_favorite,
            library_playlists,
            library_create_playlist,
            library_delete_playlist,
            library_playlist_tracks,
            library_add_to_playlist,
            library_remove_from_playlist,
            library_export_backup,
            library_restore_backup,
            library_scan_missing,
            library_check_local_paths,
            library_history,
            library_clear_history,
            library_record_history,
            library_embedded_cover,
            window_get_state,
            window_save_state,
            window_force_show,
            window_set_always_on_top,
            window_set_mini_mode,
            backup_write_file,
            backup_read_file,
            backup_preview_restore,
            backup_restore_atomic,
            sandbox_ready,
            source_list,
            source_status,
            source_import_file,
            source_import_url,
            source_activate,
            source_set_order,
            source_set_enabled,
            source_reimport,
            source_remove,
            source_reload,
            source_set_updates_enabled,
            source_get_config,
            source_set_config,
            source_get_fallback_config,
            source_set_fallback_config,
            source_export_backup,
            source_restore_backup,
            source_resolve,
            metadata_search,
            metadata_chart,
            metadata_lyrics,
            metadata_find_replacements,
            metadata_play_preview,
            lx_http_request,
            lx_send,
            lx_runtime_result,
            lx_runtime_failure,
            lx_poc_result,
            lx_crypto_result,
            lx_security_result,
            lx_poc_failure,
            cache_status,
            cache_set_limit,
            cache_set_directory,
            cache_reset_directory,
            cache_clear,
            cache_list_entries,
            cache_remove_entry,
            cache_remove_entries,
            cache_remove_by_quality,
            cache_online_favorites,
            cache_set_online_favorite,
            player_play_cache_entry
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
