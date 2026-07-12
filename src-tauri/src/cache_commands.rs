use gx_cache::{CacheStatus, CacheStore};
use gx_metadata::CatalogTrack;
use tauri::WebviewWindow;

use crate::require_window;
use crate::source_runtime::{MAX_RUNTIME_PAYLOAD_BYTES, ensure_json_size};

#[tauri::command]
pub fn cache_status(
    window: WebviewWindow,
    cache: tauri::State<'_, CacheStore>,
) -> Result<CacheStatus, String> {
    require_window(&window, "main")?;
    Ok(cache.status())
}

#[tauri::command]
pub async fn cache_set_limit(
    window: WebviewWindow,
    cache: tauri::State<'_, CacheStore>,
    limit_bytes: u64,
) -> Result<CacheStatus, String> {
    require_window(&window, "main")?;
    let cache = cache.inner().clone();
    tauri::async_runtime::spawn_blocking(move || cache.set_limit_bytes(limit_bytes))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn cache_set_directory(
    window: WebviewWindow,
    cache: tauri::State<'_, CacheStore>,
    path: String,
) -> Result<CacheStatus, String> {
    require_window(&window, "main")?;
    let cache = cache.inner().clone();
    tauri::async_runtime::spawn_blocking(move || cache.set_directory(path))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn cache_reset_directory(
    window: WebviewWindow,
    cache: tauri::State<'_, CacheStore>,
) -> Result<CacheStatus, String> {
    require_window(&window, "main")?;
    let cache = cache.inner().clone();
    tauri::async_runtime::spawn_blocking(move || cache.reset_directory())
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn cache_clear(
    window: WebviewWindow,
    cache: tauri::State<'_, CacheStore>,
    include_pinned: bool,
) -> Result<CacheStatus, String> {
    require_window(&window, "main")?;
    let cache = cache.inner().clone();
    tauri::async_runtime::spawn_blocking(move || cache.clear(include_pinned))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn cache_online_favorites(
    window: WebviewWindow,
    cache: tauri::State<CacheStore>,
) -> Result<Vec<CatalogTrack>, String> {
    require_window(&window, "main")?;
    Ok(cache
        .online_favorites()
        .into_iter()
        .filter_map(|value| serde_json::from_value(value).ok())
        .collect())
}

#[tauri::command]
pub fn cache_set_online_favorite(
    window: WebviewWindow,
    cache: tauri::State<CacheStore>,
    mut track: CatalogTrack,
    favorite: bool,
) -> Result<(), String> {
    require_window(&window, "main")?;
    // Preview URLs may be signed and short-lived; favorites only need stable catalog identity.
    track.preview = None;
    let value = serde_json::to_value(&track).map_err(|error| error.to_string())?;
    ensure_json_size(&value, MAX_RUNTIME_PAYLOAD_BYTES, "online favorite")?;
    cache
        .set_online_favorite(
            &track.provider_id,
            &track.provider_track_id,
            favorite.then_some(value),
            favorite,
        )
        .map_err(|error| error.to_string())
}
