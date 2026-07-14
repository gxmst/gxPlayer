use gx_audio::engine::LocalAudioEngine;
use gx_cache::{CacheEntryView, CacheKey, CacheStatus, CacheStore};
use gx_metadata::CatalogTrack;
use tauri::{AppHandle, Manager, WebviewWindow};

use crate::diagnostic_log::record_diagnostic;
use crate::require_window;
use crate::source_runtime::{MAX_RUNTIME_PAYLOAD_BYTES, ensure_json_size};

fn cache_error_code(error: &str) -> &'static str {
    let error = error.to_ascii_lowercase();
    if error.contains("permission denied") || error.contains("access is denied") {
        "permission_denied"
    } else if error.contains("not found") || error.contains("cannot find") {
        "not_found"
    } else if error.contains("no space") || error.contains("storage full") {
        "storage_full"
    } else if error.contains("timed out") || error.contains("timeout") {
        "timeout"
    } else if error.contains("invalid") || error.contains("outside") {
        "invalid_path"
    } else if error.contains("channel") || error.contains("disconnected") {
        "channel_disconnected"
    } else {
        "io_failed"
    }
}

fn record_cache_operation_failure(app: &AppHandle, operation: &str, error: &str) {
    record_diagnostic(
        app,
        "cache_operation_failed",
        Some("cache"),
        format!("operation={operation} code={}", cache_error_code(error)),
    );
}

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
    let app = window.app_handle().clone();
    let cache = cache.inner().clone();
    let result = match tauri::async_runtime::spawn_blocking(move || {
        cache.set_limit_bytes(limit_bytes)
    })
    .await
    {
        Ok(result) => result.map_err(|error| error.to_string()),
        Err(error) => Err(error.to_string()),
    };
    if let Err(error) = &result {
        record_cache_operation_failure(&app, "set_limit", error);
    }
    result
}

#[tauri::command]
pub async fn cache_set_directory(
    window: WebviewWindow,
    cache: tauri::State<'_, CacheStore>,
    path: String,
) -> Result<CacheStatus, String> {
    require_window(&window, "main")?;
    let app = window.app_handle().clone();
    let cache = cache.inner().clone();
    let cache_for_validation = cache.clone();
    let result = match tauri::async_runtime::spawn_blocking(move || cache.set_directory(path)).await
    {
        Ok(result) => result.map_err(|error| error.to_string()),
        Err(error) => Err(error.to_string()),
    };
    if let Err(error) = &result {
        record_cache_operation_failure(&app, "set_directory", error);
    } else {
        tauri::async_runtime::spawn_blocking(move || {
            let _ = cache_for_validation.deep_validate();
        });
    }
    result
}

#[tauri::command]
pub async fn cache_reset_directory(
    window: WebviewWindow,
    cache: tauri::State<'_, CacheStore>,
) -> Result<CacheStatus, String> {
    require_window(&window, "main")?;
    let app = window.app_handle().clone();
    let cache = cache.inner().clone();
    let cache_for_validation = cache.clone();
    let result = match tauri::async_runtime::spawn_blocking(move || cache.reset_directory()).await {
        Ok(result) => result.map_err(|error| error.to_string()),
        Err(error) => Err(error.to_string()),
    };
    if let Err(error) = &result {
        record_cache_operation_failure(&app, "reset_directory", error);
    } else {
        tauri::async_runtime::spawn_blocking(move || {
            let _ = cache_for_validation.deep_validate();
        });
    }
    result
}

#[tauri::command]
pub async fn cache_clear(
    window: WebviewWindow,
    cache: tauri::State<'_, CacheStore>,
    include_pinned: bool,
) -> Result<CacheStatus, String> {
    require_window(&window, "main")?;
    let app = window.app_handle().clone();
    let cache = cache.inner().clone();
    let result =
        match tauri::async_runtime::spawn_blocking(move || cache.clear(include_pinned)).await {
            Ok(result) => result.map_err(|error| error.to_string()),
            Err(error) => Err(error.to_string()),
        };
    if let Err(error) = &result {
        record_cache_operation_failure(&app, "clear", error);
    }
    result
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
    let result = cache
        .set_online_favorite(
            &track.provider_id,
            &track.provider_track_id,
            favorite.then_some(value),
            favorite,
        )
        .map_err(|error| error.to_string());
    if let Err(error) = &result {
        record_cache_operation_failure(window.app_handle(), "set_favorite", error);
    }
    result
}

#[tauri::command]
pub fn cache_list_entries(
    window: WebviewWindow,
    cache: tauri::State<'_, CacheStore>,
) -> Result<Vec<CacheEntryView>, String> {
    require_window(&window, "main")?;
    Ok(cache.list_entries())
}

#[tauri::command]
pub fn cache_remove_entry(
    window: WebviewWindow,
    cache: tauri::State<'_, CacheStore>,
    provider_id: String,
    provider_track_id: String,
    quality: String,
) -> Result<CacheStatus, String> {
    require_window(&window, "main")?;
    let result = cache
        .remove_entry(&CacheKey {
            provider_id,
            provider_track_id,
            quality,
        })
        .map_err(|error| error.to_string());
    if let Err(error) = &result {
        record_cache_operation_failure(window.app_handle(), "remove_entry", error);
    }
    result
}

#[tauri::command]
pub fn cache_remove_entries(
    window: WebviewWindow,
    cache: tauri::State<'_, CacheStore>,
    keys: Vec<CacheKey>,
) -> Result<CacheStatus, String> {
    require_window(&window, "main")?;
    if keys.len() > 5_000 {
        return Err("一次最多删除 5000 条缓存".into());
    }
    let result = cache
        .remove_entries(&keys)
        .map_err(|error| error.to_string());
    if let Err(error) = &result {
        record_cache_operation_failure(window.app_handle(), "remove_entries", error);
    }
    result
}

#[tauri::command]
pub fn cache_remove_by_quality(
    window: WebviewWindow,
    cache: tauri::State<'_, CacheStore>,
    quality: String,
    include_pinned: Option<bool>,
) -> Result<CacheStatus, String> {
    require_window(&window, "main")?;
    let result = cache
        .remove_by_quality(&quality, include_pinned.unwrap_or(false))
        .map_err(|error| error.to_string());
    if let Err(error) = &result {
        record_cache_operation_failure(window.app_handle(), "remove_by_quality", error);
    }
    result
}

/// Play a completed cache entry via the local path (no LX resolve).
#[tauri::command]
pub fn player_play_cache_entry(
    window: WebviewWindow,
    cache: tauri::State<'_, CacheStore>,
    engine: tauri::State<'_, LocalAudioEngine>,
    provider_id: String,
    provider_track_id: String,
    quality: String,
    title: Option<String>,
) -> Result<(), String> {
    require_window(&window, "main")?;
    let key = CacheKey {
        provider_id,
        provider_track_id,
        quality,
    };
    let listed = cache.list_entries().into_iter().find(|entry| {
        entry.provider_id == key.provider_id
            && entry.provider_track_id == key.provider_track_id
            && entry.quality == key.quality
    });
    let favorite = cache
        .online_favorites()
        .into_iter()
        .filter_map(|value| serde_json::from_value::<CatalogTrack>(value).ok())
        .find(|track| {
            track.provider_id == key.provider_id && track.provider_track_id == key.provider_track_id
        });
    let Some(hit) = cache.lookup(&key) else {
        record_diagnostic(
            window.app_handle(),
            "cache_read_failed",
            Some("cache"),
            "stage=play_cache_entry code=not_found",
        );
        return Err("该缓存条目不存在或文件已丢失".to_owned());
    };
    let play_title = title
        .filter(|value| !value.trim().is_empty())
        .or_else(|| listed.as_ref().map(|entry| entry.title.clone()))
        .unwrap_or_else(|| hit.key.provider_track_id.clone());
    let artist = listed
        .as_ref()
        .map(|entry| entry.artist.clone())
        .or_else(|| favorite.as_ref().map(|track| track.artist.clone()))
        .unwrap_or_default();
    let album = listed
        .as_ref()
        .map(|entry| entry.album.clone())
        .or_else(|| favorite.as_ref().map(|track| track.album.clone()))
        .unwrap_or_default();
    let cover_url = favorite
        .as_ref()
        .and_then(|track| track.artwork_url.as_ref())
        .map(ToString::to_string);
    let minimum_generation = crate::media_session::next_engine_generation(&engine);
    let location = hit.audio_path.display().to_string();
    let metadata_title = play_title.clone();
    if let Err(error) = engine.load_cached_online(hit.audio_path, play_title) {
        record_diagnostic(
            window.app_handle(),
            "playback_submit_failed",
            Some("cache"),
            format!(
                "stage=enqueue code={}",
                cache_error_code(&error.to_string())
            ),
        );
        return Err(error.to_string());
    }
    crate::media_session::set_cached_metadata(
        window.app_handle(),
        metadata_title,
        artist,
        album,
        cover_url,
        minimum_generation,
        location,
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::cache_error_code;

    #[test]
    fn cache_error_codes_are_bounded_and_path_free() {
        assert_eq!(
            cache_error_code("failed to write C:\\Users\\Private Name\\cache: Access is denied"),
            "permission_denied"
        );
        assert_eq!(
            cache_error_code("worker channel disconnected"),
            "channel_disconnected"
        );
        assert_eq!(
            cache_error_code("opaque failure with a secret path"),
            "io_failed"
        );
    }
}
