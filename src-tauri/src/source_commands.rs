use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use gx_audio::engine::LocalAudioEngine;
use gx_cache::{CacheKey, CacheStore};
use gx_contracts::ResolvedMediaRequest;
use gx_metadata::{
    CatalogTrack, find_replacements, search_all, search_kugou, search_kuwo, search_netease,
};
use gx_source::SourceBackup;
use gx_source::safe_http::{SafeHttpRequest, execute};
use reqwest::{Method, Url};
use serde::Serialize;
use serde_json::{Map, Value, json};
use tauri::{AppHandle, Manager, WebviewWindow};

use crate::source_runtime::{
    ListedSource, PublicSource, RuntimeStatus, ScriptLaunch, SourceRuntime, ensure_json_size,
    normalize_media_request,
};
use crate::{LxHttpResponse, LxPocState, SANDBOX_LABEL, require_window};

const MAX_HTTP_OPTIONS_BYTES: usize = 64 * 1024;
const MAX_HTTP_BODY_BYTES: usize = 1024 * 1024;
const MAX_HTTP_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_SOURCE_DOWNLOAD_BYTES: usize = 5 * 1024 * 1024;
const RUNTIME_REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    pub source: PublicSource,
    pub runtime: RuntimeStatus,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OnlinePlaybackResult {
    pub track: CatalogTrack,
    pub source_id: Option<String>,
    pub source_name: Option<String>,
    pub quality: Option<String>,
    pub cache_hit: bool,
}

#[tauri::command]
pub fn source_list(
    window: WebviewWindow,
    runtime: tauri::State<SourceRuntime>,
) -> Result<Vec<ListedSource>, String> {
    require_window(&window, "main")?;
    Ok(runtime.list())
}

#[tauri::command]
pub fn source_status(
    window: WebviewWindow,
    runtime: tauri::State<SourceRuntime>,
) -> Result<RuntimeStatus, String> {
    require_window(&window, "main")?;
    Ok(runtime.status())
}

#[tauri::command]
pub fn source_import_file(
    window: WebviewWindow,
    runtime: tauri::State<SourceRuntime>,
    path: String,
) -> Result<ImportResult, String> {
    require_window(&window, "main")?;
    let source = runtime.serialized(|| {
        let source = runtime
            .import_file(Path::new(&path))
            .map_err(|error| error.to_string())?;
        reload_runtime(
            &window.app_handle().get_webview_window(SANDBOX_LABEL),
            &runtime,
        )?;
        Ok::<_, String>(source)
    })?;
    Ok(ImportResult {
        source: PublicSource::from(&source),
        runtime: runtime.status(),
    })
}

#[tauri::command]
pub async fn source_import_url(
    window: WebviewWindow,
    runtime: tauri::State<'_, SourceRuntime>,
    url: String,
) -> Result<ImportResult, String> {
    require_window(&window, "main")?;
    let parsed = Url::parse(&url).map_err(|error| format!("invalid source URL: {error}"))?;
    let request = SafeHttpRequest {
        url: parsed.clone(),
        method: Method::GET,
        headers: Vec::new(),
        body: None,
        timeout: Duration::from_secs(20),
        max_response_bytes: MAX_SOURCE_DOWNLOAD_BYTES,
    };
    let response = tauri::async_runtime::spawn_blocking(move || execute(request))
        .await
        .map_err(|error| format!("source download task failed: {error}"))?
        .map_err(|error| error.to_string())?;
    if !(200..300).contains(&response.status) {
        return Err(format!("source download returned HTTP {}", response.status));
    }
    let script = String::from_utf8(response.body)
        .map_err(|_| "source script is not valid UTF-8".to_owned())?;
    let fallback = parsed
        .path_segments()
        .and_then(Iterator::last)
        .filter(|name| !name.is_empty())
        .unwrap_or("LX Source");
    let source = runtime.serialized(|| {
        let source = runtime
            .import_script(&script, url, fallback)
            .map_err(|error| error.to_string())?;
        reload_runtime(
            &window.app_handle().get_webview_window(SANDBOX_LABEL),
            &runtime,
        )?;
        Ok::<_, String>(source)
    })?;
    Ok(ImportResult {
        source: PublicSource::from(&source),
        runtime: runtime.status(),
    })
}

#[tauri::command]
pub fn source_activate(
    window: WebviewWindow,
    runtime: tauri::State<SourceRuntime>,
    id: String,
) -> Result<RuntimeStatus, String> {
    require_window(&window, "main")?;
    runtime.serialized(|| {
        runtime.activate(&id).map_err(|error| error.to_string())?;
        reload_runtime(
            &window.app_handle().get_webview_window(SANDBOX_LABEL),
            &runtime,
        )
    })?;
    Ok(runtime.status())
}

#[tauri::command]
pub fn source_remove(
    window: WebviewWindow,
    runtime: tauri::State<SourceRuntime>,
    id: String,
) -> Result<RuntimeStatus, String> {
    require_window(&window, "main")?;
    runtime.serialized(|| {
        runtime.remove(&id).map_err(|error| error.to_string())?;
        reload_runtime(
            &window.app_handle().get_webview_window(SANDBOX_LABEL),
            &runtime,
        )
    })?;
    Ok(runtime.status())
}

#[tauri::command]
pub fn source_reload(
    window: WebviewWindow,
    runtime: tauri::State<SourceRuntime>,
) -> Result<RuntimeStatus, String> {
    require_window(&window, "main")?;
    runtime.serialized(|| {
        reload_runtime(
            &window.app_handle().get_webview_window(SANDBOX_LABEL),
            &runtime,
        )
    })?;
    Ok(runtime.status())
}

#[tauri::command]
pub fn source_set_updates_enabled(
    window: WebviewWindow,
    runtime: tauri::State<SourceRuntime>,
    id: String,
    enabled: bool,
) -> Result<(), String> {
    require_window(&window, "main")?;
    runtime.serialized(|| {
        runtime
            .set_updates_enabled(&id, enabled)
            .map_err(|error| error.to_string())
    })
}

#[tauri::command]
pub fn source_get_config(
    window: WebviewWindow,
    runtime: tauri::State<SourceRuntime>,
    id: String,
) -> Result<Value, String> {
    require_window(&window, "main")?;
    runtime.config(&id).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn source_set_config(
    window: WebviewWindow,
    runtime: tauri::State<SourceRuntime>,
    id: String,
    config: Value,
) -> Result<RuntimeStatus, String> {
    require_window(&window, "main")?;
    ensure_json_size(
        &config,
        crate::source_runtime::MAX_RUNTIME_PAYLOAD_BYTES,
        "source config",
    )?;
    if !config.is_object() {
        return Err("source config must be a JSON object".into());
    }
    runtime.serialized(|| {
        let is_active = runtime
            .list()
            .into_iter()
            .any(|source| source.active && source.source.id == id);
        runtime
            .set_config(&id, config)
            .map_err(|error| error.to_string())?;
        if is_active {
            reload_runtime(
                &window.app_handle().get_webview_window(SANDBOX_LABEL),
                &runtime,
            )?;
        }
        Ok::<_, String>(())
    })?;
    Ok(runtime.status())
}

#[tauri::command]
pub fn source_export_backup(
    window: WebviewWindow,
    runtime: tauri::State<SourceRuntime>,
) -> Result<SourceBackup, String> {
    require_window(&window, "main")?;
    runtime.export_backup().map_err(|error| error.to_string())
}

#[tauri::command]
pub fn source_restore_backup(
    window: WebviewWindow,
    runtime: tauri::State<SourceRuntime>,
    backup: SourceBackup,
) -> Result<RuntimeStatus, String> {
    require_window(&window, "main")?;
    runtime.serialized(|| {
        runtime
            .restore_backup(backup)
            .map_err(|error| error.to_string())?;
        reload_runtime(
            &window.app_handle().get_webview_window(SANDBOX_LABEL),
            &runtime,
        )
    })?;
    Ok(runtime.status())
}

#[tauri::command]
pub async fn source_resolve(
    window: WebviewWindow,
    payload: Value,
    quality: Option<String>,
    source_id: Option<String>,
) -> Result<ResolvedMediaRequest, String> {
    require_window(&window, "main")?;
    let app = window.app_handle().clone();
    tauri::async_runtime::spawn_blocking(move || {
        resolve_serialized(&app, payload, quality.as_deref(), source_id.as_deref())
    })
    .await
    .map_err(|error| format!("LX resolver task failed: {error}"))?
}

#[tauri::command]
pub async fn player_play_online_track(
    window: WebviewWindow,
    track: CatalogTrack,
    quality: Option<String>,
    source_id: Option<String>,
) -> Result<OnlinePlaybackResult, String> {
    require_window(&window, "main")?;
    let app = window.app_handle().clone();
    tauri::async_runtime::spawn_blocking(move || play_online_track(&app, track, quality, source_id))
        .await
        .map_err(|error| format!("online playback task failed: {error}"))?
}

fn play_online_track(
    app: &AppHandle,
    track: CatalogTrack,
    quality: Option<String>,
    source_id: Option<String>,
) -> Result<OnlinePlaybackResult, String> {
    if let Some((source, _)) = lx_identity(&track) {
        let capabilities = app.state::<SourceRuntime>().status().capabilities;
        for attempt in quality_attempts(&capabilities, source, quality.as_deref()) {
            if let Some(result) = play_cache_hit(app, &track, &attempt, &source_id)? {
                return Ok(result);
            }
        }
    }
    let candidates = select_lx_candidates(track)?;
    let mut errors = Vec::new();
    for candidate in candidates {
        let source = lx_identity(&candidate)
            .map(|(source, _)| source)
            .ok_or_else(|| "candidate lost its LX source identity".to_owned())?;
        let capabilities = app.state::<SourceRuntime>().status().capabilities;
        let attempts = quality_attempts(&capabilities, source, quality.as_deref());
        for attempt in &attempts {
            if let Some(result) = play_cache_hit(app, &candidate, attempt, &source_id)? {
                return Ok(result);
            }
            let payload = lx_music_url_payload(&candidate, attempt)?;
            match resolve_serialized(app, payload, Some(attempt), source_id.as_deref()) {
                Ok(request) => {
                    if let Err(error) = validate_full_track_request(&request, candidate.duration_ms)
                    {
                        errors.push(format!(
                            "{}:{} {attempt}: {error}",
                            candidate.provider_id, candidate.provider_track_id
                        ));
                        continue;
                    }
                    let resolved_quality =
                        request.quality.clone().unwrap_or_else(|| attempt.clone());
                    let cache_plan = app.state::<CacheStore>().prepare(
                        CacheKey {
                            provider_id: candidate.provider_id.clone(),
                            provider_track_id: candidate.provider_track_id.clone(),
                            quality: resolved_quality.clone(),
                        },
                        request.media_type.clone(),
                    );
                    app.state::<LocalAudioEngine>()
                        .load_resolved_cached(request, candidate.title.clone(), Some(cache_plan))
                        .map_err(|error| {
                            format!("Rust streaming engine rejected LX media: {error}")
                        })?;

                    let (selected_source_id, selected_source_name) =
                        selected_source(app, &source_id);
                    return Ok(OnlinePlaybackResult {
                        track: candidate,
                        source_id: selected_source_id,
                        source_name: selected_source_name,
                        quality: Some(resolved_quality),
                        cache_hit: false,
                    });
                }
                Err(error) => errors.push(format!(
                    "{}:{} {attempt}: {error}",
                    candidate.provider_id, candidate.provider_track_id
                )),
            }
        }
    }
    Err(format!(
        "LX source could not resolve a verified full-track URL ({})",
        errors.join("; ")
    ))
}

fn play_cache_hit(
    app: &AppHandle,
    track: &CatalogTrack,
    quality: &str,
    source_id: &Option<String>,
) -> Result<Option<OnlinePlaybackResult>, String> {
    let key = CacheKey {
        provider_id: track.provider_id.clone(),
        provider_track_id: track.provider_track_id.clone(),
        quality: quality.to_owned(),
    };
    let Some(hit) = app.state::<CacheStore>().lookup(&key) else {
        return Ok(None);
    };
    app.state::<LocalAudioEngine>()
        .load_cached_online(hit.audio_path, track.title.clone())
        .map_err(|error| format!("Rust audio engine rejected cached media: {error}"))?;
    let (selected_source_id, selected_source_name) = selected_source(app, source_id);
    Ok(Some(OnlinePlaybackResult {
        track: track.clone(),
        source_id: selected_source_id,
        source_name: selected_source_name,
        quality: Some(quality.to_owned()),
        cache_hit: true,
    }))
}

fn selected_source(
    app: &AppHandle,
    requested_source_id: &Option<String>,
) -> (Option<String>, Option<String>) {
    let selected_source_id = requested_source_id
        .clone()
        .or_else(|| app.state::<SourceRuntime>().status().active_source_id);
    let selected_source_name = selected_source_id.as_deref().and_then(|id| {
        app.state::<SourceRuntime>()
            .list()
            .into_iter()
            .find(|source| source.source.id == id)
            .map(|source| source.source.metadata.name)
    });
    (selected_source_id, selected_source_name)
}

const QUALITY_ORDER: [&str; 4] = ["flac24bit", "flac", "320k", "128k"];

fn quality_attempts(capabilities: &Value, source: &str, preference: Option<&str>) -> Vec<String> {
    let supported = advertised_qualities(capabilities, source);
    let preference = preference
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "auto")
        .filter(|value| QUALITY_ORDER.contains(value));
    let start = preference
        .and_then(|value| QUALITY_ORDER.iter().position(|quality| *quality == value))
        .unwrap_or(0);
    let mut attempts = QUALITY_ORDER[start..]
        .iter()
        .filter(|quality| {
            supported
                .as_ref()
                .is_none_or(|supported| supported.iter().any(|value| value == **quality))
        })
        .map(|quality| (*quality).to_owned())
        .collect::<Vec<_>>();
    if attempts.is_empty() {
        attempts = if preference == Some("128k") {
            vec!["128k".into()]
        } else {
            vec!["320k".into(), "128k".into()]
        };
    }
    attempts
}

fn advertised_qualities(capabilities: &Value, source: &str) -> Option<Vec<String>> {
    let source = capabilities.get("sources")?.get(source)?;
    let values = source
        .get("qualitys")
        .or_else(|| source.get("qualities"))
        .unwrap_or(source)
        .as_array()?;
    let qualities = values
        .iter()
        .filter_map(Value::as_str)
        .filter(|quality| QUALITY_ORDER.contains(quality))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    (!qualities.is_empty()).then_some(qualities)
}

fn select_lx_candidates(track: CatalogTrack) -> Result<Vec<CatalogTrack>, String> {
    let direct = lx_identity(&track).is_some().then(|| track.clone());
    let query = format!("{} {}", track.title, track.artist);
    let (kugou, kuwo, netease) = std::thread::scope(|scope| {
        let kugou = scope.spawn(|| search_kugou(&query, 12));
        let kuwo = scope.spawn(|| search_kuwo(&query, 12));
        let netease = scope.spawn(|| search_netease(&query, 12));
        (
            kugou.join().unwrap(),
            kuwo.join().unwrap(),
            netease.join().unwrap(),
        )
    });
    let mut candidates = Vec::new();
    let mut errors = Vec::new();
    match kugou {
        Ok(mut found) => candidates.append(&mut found),
        Err(error) => errors.push(format!("Kugou metadata: {error}")),
    }
    match kuwo {
        Ok(mut found) => candidates.append(&mut found),
        Err(error) => errors.push(format!("Kuwo metadata: {error}")),
    }
    match netease {
        Ok(mut found) => candidates.append(&mut found),
        Err(error) => errors.push(format!("NetEase metadata: {error}")),
    }
    let matches = find_replacements(&track, candidates);
    let selected = direct
        .into_iter()
        .chain(matches.into_iter().map(|candidate| candidate.track))
        .collect::<Vec<_>>();
    if selected.is_empty() {
        Err({
            if errors.is_empty() {
                "no matching LX-platform song was found for this catalog result".into()
            } else {
                format!(
                    "LX-platform metadata lookup failed or found no safe match ({})",
                    errors.join("; ")
                )
            }
        })
    } else {
        Ok(selected)
    }
}

fn validate_full_track_request(
    request: &ResolvedMediaRequest,
    expected_duration_ms: Option<u64>,
) -> Result<(), String> {
    let mut headers = request
        .headers
        .iter()
        .map(|header| (header.name.clone(), header.value.clone()))
        .collect::<Vec<_>>();
    headers.retain(|(name, _)| !name.eq_ignore_ascii_case("range"));
    headers.push(("range".into(), "bytes=0-0".into()));
    let response = execute(SafeHttpRequest {
        url: request.url.clone(),
        method: Method::GET,
        headers,
        body: None,
        timeout: Duration::from_secs(10),
        max_response_bytes: 4096,
    })
    .map_err(|error| format!("resolved media probe failed: {error}"))?;
    if response.status != 206 {
        return Err(format!(
            "resolved media Range probe returned HTTP {} instead of 206",
            response.status
        ));
    }
    let total_length = response
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-range"))
        .and_then(|(_, value)| parse_content_range_total(value))
        .ok_or_else(|| {
            "resolved media Range probe omitted total Content-Range length".to_owned()
        })?;
    let minimum_full_track_bytes = minimum_full_track_bytes(expected_duration_ms);
    if total_length < minimum_full_track_bytes {
        return Err(format!(
            "resolved media is only {total_length} bytes (minimum {minimum_full_track_bytes}); refusing preview-sized audio"
        ));
    }
    Ok(())
}

fn parse_content_range_total(value: &str) -> Option<u64> {
    let (_, total) = value.rsplit_once('/')?;
    (total != "*").then(|| total.parse().ok()).flatten()
}

fn minimum_full_track_bytes(expected_duration_ms: Option<u64>) -> u64 {
    expected_duration_ms
        .map(|duration| duration / 1000 * 3000)
        .unwrap_or(0)
        .max(512 * 1024)
}

fn lx_music_url_payload(track: &CatalogTrack, quality: &str) -> Result<Value, String> {
    let (source, music_info) = lx_identity(track)
        .ok_or_else(|| "catalog result does not contain an LX musicInfo payload".to_owned())?;
    Ok(json!({
        "source": source,
        "action": "musicUrl",
        "info": {
            "type": quality,
            "musicInfo": music_info,
        }
    }))
}

fn lx_identity(track: &CatalogTrack) -> Option<(&str, &Value)> {
    let source = track.resolver_payload.get("source")?.as_str()?;
    if !matches!(source, "kw" | "wy" | "tx" | "kg" | "mg") {
        return None;
    }
    let music_info = track.resolver_payload.get("musicInfo")?;
    music_info.is_object().then_some((source, music_info))
}

#[tauri::command]
pub fn lx_runtime_result(
    window: WebviewWindow,
    runtime: tauri::State<SourceRuntime>,
    request_id: String,
    generation: u64,
    result: Option<Value>,
    error: Option<String>,
) -> Result<(), String> {
    require_window(&window, SANDBOX_LABEL)?;
    let outcome = match error {
        Some(error) => Err(error),
        None => result.ok_or_else(|| "LX runtime returned no result".to_owned()),
    };
    runtime.complete_request(&request_id, generation, outcome)
}

#[tauri::command]
pub fn lx_runtime_failure(
    window: WebviewWindow,
    runtime: tauri::State<SourceRuntime>,
    generation: u64,
    stage: String,
    error: String,
) -> Result<(), String> {
    require_window(&window, SANDBOX_LABEL)?;
    runtime.mark_failed(generation, format!("{stage}: {error}"));
    Ok(())
}

#[tauri::command]
pub async fn lx_http_request(
    window: WebviewWindow,
    url: String,
    options: Value,
) -> Result<LxHttpResponse, String> {
    require_window(&window, SANDBOX_LABEL)?;
    if url.len() > 16 * 1024 {
        return Err("HTTP URL exceeds the 16 KiB limit".into());
    }
    if std::env::var_os("GX_PHASE1_LX_POC").is_some()
        || std::env::var_os("GX_PHASE2_LX_MOCK").is_some()
    {
        return crate::phase1_http_mock(&url, &options);
    }
    let request = parse_http_request(&url, options)?;
    let response = tauri::async_runtime::spawn_blocking(move || execute(request))
        .await
        .map_err(|error| format!("HTTP proxy task failed: {error}"))?
        .map_err(|error| error.to_string())?;
    let headers = response.headers.into_iter().collect::<BTreeMap<_, _>>();
    let content_type = headers
        .get("content-type")
        .map(String::as_str)
        .unwrap_or_default();
    let body = if content_type.to_ascii_lowercase().contains("json") {
        serde_json::from_slice(&response.body)
            .map_err(|error| format!("HTTP response declared JSON but was invalid: {error}"))?
    } else {
        Value::String(String::from_utf8_lossy(&response.body).into_owned())
    };
    Ok(LxHttpResponse {
        status_code: response.status,
        headers,
        body,
    })
}

#[tauri::command]
pub fn lx_send(
    window: WebviewWindow,
    app: AppHandle,
    runtime: tauri::State<SourceRuntime>,
    poc: tauri::State<LxPocState>,
    event_name: String,
    data: Value,
    generation: u64,
) -> Result<(), String> {
    require_window(&window, SANDBOX_LABEL)?;
    if event_name.len() > 64 {
        return Err("LX event name exceeds the size limit".into());
    }
    if std::env::var_os("GX_PHASE1_LX_POC").is_some() {
        return crate::phase1_lx_send(&window, event_name, data, &app, &poc);
    }
    match event_name.as_str() {
        "updateAlert" => runtime.record_update_alert(generation, data),
        "inited" => {
            runtime.mark_ready(generation, data)?;
            if std::env::var_os("GX_PHASE2_AUTO_RESOLVE").is_some() {
                start_phase2_auto_resolve(&app)?;
            }
            if std::env::var_os("GX_ONLINE_E2E_QUERY").is_some() {
                start_online_e2e(&app)?;
            }
            Ok(())
        }
        _ => Err(format!("unsupported lx.send event: {event_name}")),
    }
}

fn start_online_e2e(app: &AppHandle) -> Result<(), String> {
    let query = std::env::var("GX_ONLINE_E2E_QUERY").map_err(|error| error.to_string())?;
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let app_for_play = app.clone();
        let result = tauri::async_runtime::spawn_blocking(move || {
            let track = search_all(&query, 8)
                .map_err(|error| error.to_string())?
                .into_iter()
                .find(|track| lx_identity(track).is_some())
                .ok_or_else(|| "online E2E search returned no LX-compatible track".to_owned())?;
            let result = play_online_track(&app_for_play, track, Some("320k".into()), None)?;
            println!(
                "GX_ONLINE_SEARCH_RESOLVE_OK provider={} id={} quality={} cache_hit={}",
                result.track.provider_id,
                result.track.provider_track_id,
                result.quality.as_deref().unwrap_or("unknown"),
                result.cache_hit
            );
            if std::env::var_os("GX_CACHE_COMPLETE_E2E").is_some() {
                monitor_cache_completion(&app_for_play, result)
            } else {
                monitor_full_track_controls(&app_for_play)?;
                verify_interrupted_cache_was_discarded(&app_for_play, &result)
            }
        })
        .await
        .map_err(|error| format!("online E2E task failed: {error}"))
        .and_then(|result| result);
        match result {
            Ok(()) => {
                println!("GX_ONLINE_SEARCH_TO_NATIVE_STREAM_OK");
                if std::env::var_os("GX_PHASE2_AUTO_EXIT").is_some() {
                    app.exit(0);
                }
            }
            Err(error) => {
                eprintln!("GX_ONLINE_E2E_FAILED {error}");
                app.exit(2);
            }
        }
    });
    Ok(())
}

fn cache_key_for_result(result: &OnlinePlaybackResult) -> Result<CacheKey, String> {
    Ok(CacheKey {
        provider_id: result.track.provider_id.clone(),
        provider_track_id: result.track.provider_track_id.clone(),
        quality: result
            .quality
            .clone()
            .ok_or_else(|| "online result has no quality for cache verification".to_owned())?,
    })
}

fn verify_interrupted_cache_was_discarded(
    app: &AppHandle,
    result: &OnlinePlaybackResult,
) -> Result<(), String> {
    if result.cache_hit {
        return Ok(());
    }
    std::thread::sleep(Duration::from_millis(500));
    let cache = app.state::<CacheStore>();
    let key = cache_key_for_result(result)?;
    if cache.lookup(&key).is_some() {
        return Err("seek-interrupted stream incorrectly became a complete cache entry".into());
    }
    let has_part = std::fs::read_dir(cache.status().directory)
        .map_err(|error| error.to_string())?
        .flatten()
        .any(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|value| value == "part")
        });
    if has_part {
        return Err("seek-interrupted stream left a .part file behind".into());
    }
    println!("GX_CACHE_SEEK_ABORT_OK no_entry=true no_part=true");
    Ok(())
}

fn monitor_cache_completion(app: &AppHandle, first: OnlinePlaybackResult) -> Result<(), String> {
    if first.cache_hit {
        return Err("cache completion smoke requires an initial cache miss".into());
    }
    let key = cache_key_for_result(&first)?;
    for _ in 0..7_200 {
        let snapshot = app.state::<LocalAudioEngine>().snapshot();
        if snapshot.status == gx_contracts::PlaybackStatus::Failed {
            return Err(snapshot.error.unwrap_or_else(|| "playback failed".into()));
        }
        if snapshot.status == gx_contracts::PlaybackStatus::Stopped {
            let cache = app.state::<CacheStore>();
            let entry = (0..50)
                .find_map(|_| {
                    let hit = cache.lookup(&key);
                    if hit.is_none() {
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    hit
                })
                .ok_or_else(|| "completed playback did not produce a cache entry".to_owned())?;
            if entry.source_sample_rate.is_none() || entry.source_channels.is_none() {
                return Err("cache sidecar is missing measured source specifications".into());
            }
            println!(
                "GX_CACHE_COMPLETE_OK bytes={} sample_rate={} bit_depth={} channels={}",
                entry.byte_len,
                entry.source_sample_rate.unwrap(),
                entry
                    .source_bit_depth
                    .map_or_else(|| "unknown".into(), |value| value.to_string()),
                entry.source_channels.unwrap()
            );
            let replay = play_online_track(
                app,
                first.track.clone(),
                first.quality.clone(),
                first.source_id.clone(),
            )?;
            if !replay.cache_hit {
                return Err("second playback did not hit the completed cache".into());
            }
            for _ in 0..300 {
                let replay_snapshot = app.state::<LocalAudioEngine>().snapshot();
                if replay_snapshot.status == gx_contracts::PlaybackStatus::Playing {
                    app.state::<LocalAudioEngine>()
                        .seek(30.0)
                        .map_err(|error| error.to_string())?;
                    println!("GX_CACHE_REPLAY_HIT_OK local=true seek_submitted=true");
                    return Ok(());
                }
                if replay_snapshot.status == gx_contracts::PlaybackStatus::Failed {
                    return Err(replay_snapshot
                        .error
                        .unwrap_or_else(|| "cached replay failed".into()));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            return Err("cached replay did not start within 30 seconds".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err("cache completion playback did not finish within 12 minutes".into())
}

fn start_phase2_auto_resolve(app: &AppHandle) -> Result<(), String> {
    let payload = std::env::var("GX_PHASE2_RESOLVER_PAYLOAD")
        .ok()
        .map(|text| serde_json::from_str(&text).map_err(|error| error.to_string()))
        .transpose()?
        .unwrap_or_else(|| {
            json!({
                "source": "wy",
                "action": "musicUrl",
                "info": {
                    "type": "128k",
                    "musicInfo": { "hash": "phase2-track", "name": "Phase 2" }
                }
            })
        });
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let app_for_resolve = app.clone();
        let result = tauri::async_runtime::spawn_blocking(move || {
            resolve_serialized(&app_for_resolve, payload, Some("128k"), None)
        })
        .await
        .map_err(|error| format!("LX resolver task failed: {error}"))
        .and_then(|result| result);
        let request = match result {
            Ok(request) => request,
            Err(error) => {
                eprintln!("GX_PHASE2_LX_FAILED {error}");
                app.exit(2);
                return;
            }
        };
        println!("GX_PHASE2_LX_RESOLVED_OK {}", request.redacted_for_log());
        let engine = app.state::<LocalAudioEngine>();
        if let Err(error) = engine.load_resolved(request, "Phase 2 online smoke".into()) {
            eprintln!("GX_PHASE2_LX_FAILED {error}");
            app.exit(2);
            return;
        }
        let app_for_monitor = app.clone();
        let monitor = tauri::async_runtime::spawn_blocking(move || {
            let full_e2e = std::env::var_os("GX_PHASE2_FULL_E2E").is_some();
            for _ in 0..600 {
                let snapshot = app_for_monitor.state::<LocalAudioEngine>().snapshot();
                if snapshot.status == gx_contracts::PlaybackStatus::Playing
                    && snapshot.position_seconds > 0.2
                    && !full_e2e
                {
                    println!(
                        "GX_PHASE2_NATIVE_STREAM_PLAYBACK_OK position={:.3} underruns={}",
                        snapshot.position_seconds, snapshot.underrun_callbacks
                    );
                    return Ok(());
                }
                if snapshot.status == gx_contracts::PlaybackStatus::Playing
                    && snapshot.position_seconds > 1.0
                    && full_e2e
                {
                    let duration = snapshot.duration_seconds.ok_or_else(|| {
                        "full-track smoke did not expose a media duration".to_owned()
                    })?;
                    if duration <= 60.0 {
                        return Err(format!(
                            "full-track smoke resolved only {duration:.1}s; refusing to accept a preview"
                        ));
                    }
                    if snapshot.underrun_callbacks != 0 {
                        return Err(format!(
                            "full-track smoke underrun before controls: {}",
                            snapshot.underrun_callbacks
                        ));
                    }
                    let engine = app_for_monitor.state::<LocalAudioEngine>();
                    engine.pause().map_err(|error| error.to_string())?;
                    std::thread::sleep(Duration::from_millis(300));
                    let paused = engine.snapshot();
                    if paused.status != gx_contracts::PlaybackStatus::Paused {
                        return Err(format!(
                            "pause smoke expected Paused, got {:?}",
                            paused.status
                        ));
                    }
                    engine.seek(30.0).map_err(|error| error.to_string())?;
                    engine.play().map_err(|error| error.to_string())?;
                    for _ in 0..300 {
                        let after_seek = app_for_monitor.state::<LocalAudioEngine>().snapshot();
                        if after_seek.status == gx_contracts::PlaybackStatus::Failed {
                            return Err(after_seek
                                .error
                                .unwrap_or_else(|| "playback failed after Range seek".into()));
                        }
                        if after_seek.position_seconds > 35.0 {
                            if after_seek.underrun_callbacks != 0 {
                                return Err(format!(
                                    "full-track smoke underruns after seek: {}",
                                    after_seek.underrun_callbacks
                                ));
                            }
                            println!(
                                "GX_PHASE2_FULL_TRACK_CONTROLS_OK duration={duration:.3} position={:.3} underruns=0",
                                after_seek.position_seconds
                            );
                            return Ok(());
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    return Err("Range seek did not resume within 30 seconds".into());
                }
                if snapshot.status == gx_contracts::PlaybackStatus::Failed {
                    return Err(snapshot
                        .error
                        .unwrap_or_else(|| "online playback failed".into()));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err("online playback did not start within 60 seconds".to_owned())
        })
        .await;
        match monitor {
            Ok(Ok(())) => {
                println!("GX_PHASE2_LX_E2E_OK");
                if std::env::var_os("GX_PHASE2_AUTO_EXIT").is_some() {
                    app.exit(0);
                }
            }
            Ok(Err(error)) => {
                eprintln!("GX_PHASE2_LX_FAILED {error}");
                app.exit(2);
            }
            Err(error) => {
                eprintln!("GX_PHASE2_LX_FAILED monitor task: {error}");
                app.exit(2);
            }
        }
    });
    Ok(())
}

fn monitor_full_track_controls(app: &AppHandle) -> Result<(), String> {
    for _ in 0..600 {
        let snapshot = app.state::<LocalAudioEngine>().snapshot();
        if snapshot.status == gx_contracts::PlaybackStatus::Failed {
            return Err(snapshot
                .error
                .unwrap_or_else(|| "online playback failed".into()));
        }
        if snapshot.status == gx_contracts::PlaybackStatus::Playing
            && snapshot.position_seconds > 1.0
        {
            let duration = snapshot
                .duration_seconds
                .ok_or_else(|| "full-track smoke did not expose a media duration".to_owned())?;
            if duration <= 60.0 {
                return Err(format!(
                    "full-track smoke resolved only {duration:.1}s; refusing to accept a preview"
                ));
            }
            if snapshot.underrun_callbacks != 0 {
                return Err(format!(
                    "full-track smoke underrun before controls: {}",
                    snapshot.underrun_callbacks
                ));
            }
            let engine = app.state::<LocalAudioEngine>();
            let generation_before_volume = snapshot.generation;
            engine.set_volume(0.35).map_err(|error| error.to_string())?;
            std::thread::sleep(Duration::from_millis(300));
            let after_volume = engine.snapshot();
            if after_volume.generation != generation_before_volume
                || after_volume.status != gx_contracts::PlaybackStatus::Playing
                || after_volume.underrun_callbacks != 0
            {
                return Err(format!(
                    "volume hot-update disrupted playback: generation {}->{}, status={:?}, underruns={}",
                    generation_before_volume,
                    after_volume.generation,
                    after_volume.status,
                    after_volume.underrun_callbacks
                ));
            }
            println!("GX_VOLUME_HOT_UPDATE_OK generation={generation_before_volume} underruns=0");
            engine.pause().map_err(|error| error.to_string())?;
            std::thread::sleep(Duration::from_millis(300));
            let paused = engine.snapshot();
            if paused.status != gx_contracts::PlaybackStatus::Paused {
                return Err(format!(
                    "pause smoke expected Paused, got {:?}",
                    paused.status
                ));
            }
            engine.seek(30.0).map_err(|error| error.to_string())?;
            engine.play().map_err(|error| error.to_string())?;
            for _ in 0..300 {
                let after_seek = app.state::<LocalAudioEngine>().snapshot();
                if after_seek.status == gx_contracts::PlaybackStatus::Failed {
                    return Err(after_seek
                        .error
                        .unwrap_or_else(|| "playback failed after Range seek".into()));
                }
                if after_seek.position_seconds > 35.0 {
                    if after_seek.underrun_callbacks != 0 {
                        return Err(format!(
                            "full-track smoke underruns after seek: {}",
                            after_seek.underrun_callbacks
                        ));
                    }
                    println!(
                        "GX_FULL_TRACK_CONTROLS_OK duration={duration:.3} position={:.3} underruns=0",
                        after_seek.position_seconds
                    );
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            return Err("Range seek did not resume within 30 seconds".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err("online playback did not start within 60 seconds".into())
}

fn resolve_serialized(
    app: &AppHandle,
    payload: Value,
    quality: Option<&str>,
    source_id: Option<&str>,
) -> Result<ResolvedMediaRequest, String> {
    let runtime = app.state::<SourceRuntime>();
    runtime.serialized(|| {
        let persistent_active = runtime
            .list()
            .into_iter()
            .find(|source| source.active)
            .map(|source| source.source.id);
        let temporary = source_id.filter(|id| persistent_active.as_deref() != Some(*id));
        if let Some(id) = temporary {
            let switched = (|| {
                let launch = runtime
                    .prepare_reload_for(id)
                    .map_err(|error| error.to_string())?;
                let sandbox = app
                    .get_webview_window(SANDBOX_LABEL)
                    .ok_or_else(|| "LX sandbox window is unavailable".to_owned())?;
                evaluate_launch(&sandbox, &launch)?;
                wait_until_ready(&runtime, launch.generation, Duration::from_secs(15))
            })();
            if let Err(error) = switched {
                let _ = restore_persistent_runtime(app, &runtime);
                return Err(error);
            }
        }
        let result = dispatch_and_wait(app, &runtime, &payload, quality);
        if temporary.is_some()
            && let Err(restore_error) = restore_persistent_runtime(app, &runtime)
        {
            return match result {
                Ok(_) => Err(restore_error),
                Err(resolve_error) => Err(format!(
                    "{resolve_error}; additionally failed to restore active source: {restore_error}"
                )),
            };
        }
        result
    })
}

fn restore_persistent_runtime(app: &AppHandle, runtime: &SourceRuntime) -> Result<(), String> {
    let restore = runtime
        .prepare_reload()
        .map_err(|error| error.to_string())?;
    if let Some(launch) = restore {
        let sandbox = app
            .get_webview_window(SANDBOX_LABEL)
            .ok_or_else(|| "LX sandbox window is unavailable".to_owned())?;
        evaluate_launch(&sandbox, &launch)?;
        wait_until_ready(runtime, launch.generation, Duration::from_secs(15))?;
    }
    Ok(())
}

fn dispatch_and_wait(
    app: &AppHandle,
    runtime: &SourceRuntime,
    payload: &Value,
    quality: Option<&str>,
) -> Result<ResolvedMediaRequest, String> {
    let pending = runtime.begin_request(payload)?;
    let request_id = pending.request_id.clone();
    let generation = pending.generation;
    let encoded_id = serde_json::to_string(&request_id).map_err(|error| error.to_string())?;
    let encoded_payload = serde_json::to_string(payload).map_err(|error| error.to_string())?;
    let sandbox = app
        .get_webview_window(SANDBOX_LABEL)
        .ok_or_else(|| "LX sandbox window is unavailable".to_owned())?;
    if let Err(error) = sandbox.eval(format!(
        "window.__gxDispatchRequest({encoded_id}, {encoded_payload}, {generation})"
    )) {
        runtime.cancel_request(&request_id, "failed to dispatch LX runtime request");
        return Err(error.to_string());
    }
    let raw = match pending.receiver.recv_timeout(RUNTIME_REQUEST_TIMEOUT) {
        Ok(result) => result?,
        Err(_) => {
            runtime.cancel_request(&request_id, "LX resolver request timed out");
            return Err("LX resolver request timed out".into());
        }
    };
    normalize_media_request(raw, quality)
}

fn wait_until_ready(
    runtime: &SourceRuntime,
    generation: u64,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        let status = runtime.status();
        if status.generation != generation {
            return Err("LX runtime generation changed while waiting for initialization".into());
        }
        match status.state {
            crate::source_runtime::RuntimeState::Ready => return Ok(()),
            crate::source_runtime::RuntimeState::Failed => {
                return Err(status.error.unwrap_or_else(|| "LX runtime failed".into()));
            }
            _ if Instant::now() >= deadline => {
                runtime
                    .fail_if_initializing(generation, "LX runtime initialization timed out".into());
                return Err("LX runtime initialization timed out".into());
            }
            _ => std::thread::sleep(Duration::from_millis(25)),
        }
    }
}

pub fn sandbox_became_ready(
    window: &WebviewWindow,
    runtime: &SourceRuntime,
    poc: &LxPocState,
) -> Result<(), String> {
    if std::env::var_os("GX_PHASE1_LX_POC").is_some() {
        let script = std::fs::read_to_string(&poc.script_path).map_err(|error| {
            format!(
                "failed to read community LX script {}: {error}",
                poc.script_path.display()
            )
        })?;
        let encoded = serde_json::to_string(&script).map_err(|error| error.to_string())?;
        return window
            .eval(format!(
                "window.__gxRunCommunityScript({encoded}, {{ poc: true }})"
            ))
            .map_err(|error| error.to_string());
    }
    reload_runtime(&Some(window.clone()), runtime)
}

fn reload_runtime(sandbox: &Option<WebviewWindow>, runtime: &SourceRuntime) -> Result<(), String> {
    let launch = runtime
        .prepare_reload()
        .map_err(|error| error.to_string())?;
    let Some(launch) = launch else {
        return Ok(());
    };
    let sandbox = sandbox
        .as_ref()
        .ok_or_else(|| "LX sandbox window is unavailable".to_owned())?;
    evaluate_launch(sandbox, &launch)?;
    let app = sandbox.app_handle().clone();
    let generation = launch.generation;
    tauri::async_runtime::spawn_blocking(move || {
        std::thread::sleep(Duration::from_secs(15));
        app.state::<SourceRuntime>()
            .fail_if_initializing(generation, "LX runtime initialization timed out".into());
    });
    Ok(())
}

fn evaluate_launch(window: &WebviewWindow, launch: &ScriptLaunch) -> Result<(), String> {
    let (ls_config, key_overrides) = split_source_config(&launch.source.config);
    let executable_script = apply_key_overrides(&launch.script, key_overrides)?;
    let script = serde_json::to_string(&executable_script).map_err(|error| error.to_string())?;
    let config = if std::env::var_os("GX_PHASE2_LX_MOCK").is_some() {
        json!({ "api": { "addr": "http://gx.invalid/", "pass": "" } })
    } else {
        ls_config
    };
    let context = json!({
        "generation": launch.generation,
        "poc": false,
        "scriptInfo": {
            "name": launch.source.metadata.name,
            "version": launch.source.metadata.version,
            "author": launch.source.metadata.author,
            "homepage": launch.source.metadata.homepage,
            "rawScript": executable_script
        },
        "config": config
    });
    let context = serde_json::to_string(&context).map_err(|error| error.to_string())?;
    window
        .eval(format!(
            "window.__gxRunCommunityScript({script}, {context})"
        ))
        .map_err(|error| error.to_string())
}

fn split_source_config(config: &Value) -> (Value, &[Value]) {
    let Some(object) = config.as_object() else {
        return (json!({}), &[]);
    };
    let is_structured = object.contains_key("lsConfig") || object.contains_key("keyOverrides");
    let ls_config = if is_structured {
        object.get("lsConfig").cloned().unwrap_or_else(|| json!({}))
    } else {
        config.clone()
    };
    let key_overrides = object
        .get("keyOverrides")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    (ls_config, key_overrides)
}

fn apply_key_overrides(script: &str, overrides: &[Value]) -> Result<String, String> {
    let mut output = script.to_owned();
    for item in overrides {
        let Some(const_name) = item.get("constName").and_then(Value::as_str) else {
            continue;
        };
        let Some(value) = item.get("value").and_then(Value::as_str) else {
            continue;
        };
        if !is_safe_const_name(const_name) {
            continue;
        }
        let name = regex::escape(const_name);
        let pattern = format!(
            r#"(?m)^([\t ]*const[\t ]+{name}[\t ]*=[\t ]*)(?:'(?:\\.|[^'\\\r\n])*'|\"(?:\\.|[^\"\\\r\n])*\")"#
        );
        let regex = regex::Regex::new(&pattern).map_err(|error| error.to_string())?;
        let literal = serde_json::to_string(value).map_err(|error| error.to_string())?;
        output = regex
            .replacen(&output, 1, |captures: &regex::Captures<'_>| {
                format!("{}{}", &captures[1], literal)
            })
            .into_owned();
    }
    Ok(output)
}

fn is_safe_const_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_' || first == '$')
        && chars.all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '$'
        })
}

fn parse_http_request(url: &str, options: Value) -> Result<SafeHttpRequest, String> {
    if serde_json::to_vec(&options)
        .map_err(|error| error.to_string())?
        .len()
        > MAX_HTTP_OPTIONS_BYTES
    {
        return Err("HTTP options exceed the 64 KiB limit".into());
    }
    let parsed = Url::parse(url).map_err(|error| format!("invalid URL: {error}"))?;
    let object = options
        .as_object()
        .ok_or_else(|| "HTTP options must be an object".to_owned())?;
    let method = parse_method(object.get("method"))?;
    let mut headers = parse_headers(object.get("headers"))?;
    let body = parse_body(object)?;
    if object.contains_key("json")
        && !headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("content-type"))
    {
        headers.push(("content-type".into(), "application/json".into()));
    } else if object.contains_key("form")
        && !headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("content-type"))
    {
        headers.push((
            "content-type".into(),
            "application/x-www-form-urlencoded".into(),
        ));
    }
    let timeout_ms = object
        .get("timeout")
        .and_then(Value::as_u64)
        .unwrap_or(15_000)
        .clamp(1_000, 30_000);
    Ok(SafeHttpRequest {
        url: parsed,
        method,
        headers,
        body,
        timeout: Duration::from_millis(timeout_ms),
        max_response_bytes: MAX_HTTP_RESPONSE_BYTES,
    })
}

fn parse_method(value: Option<&Value>) -> Result<Method, String> {
    let text = value
        .and_then(Value::as_str)
        .unwrap_or("GET")
        .to_ascii_uppercase();
    match text.as_str() {
        "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD" | "OPTIONS" => {
            Method::from_bytes(text.as_bytes()).map_err(|error| error.to_string())
        }
        _ => Err(format!("HTTP method {text} is not allowed")),
    }
}

fn parse_headers(value: Option<&Value>) -> Result<Vec<(String, String)>, String> {
    let Some(Value::Object(headers)) = value else {
        return Ok(Vec::new());
    };
    if headers.len() > 64 {
        return Err("HTTP request has too many headers".into());
    }
    headers
        .iter()
        .map(|(name, value)| {
            let value = value
                .as_str()
                .ok_or_else(|| "HTTP header values must be strings".to_owned())?;
            if name.len() > 256 || value.len() > 8192 {
                return Err("HTTP header exceeds the size limit".into());
            }
            Ok((name.clone(), value.to_owned()))
        })
        .collect()
}

fn parse_body(options: &Map<String, Value>) -> Result<Option<Vec<u8>>, String> {
    let body = if let Some(value) = options.get("json") {
        Some(serde_json::to_vec(value).map_err(|error| error.to_string())?)
    } else if let Some(Value::Object(form)) = options.get("form") {
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        for (name, value) in form {
            let value = value
                .as_str()
                .ok_or_else(|| "HTTP form values must be strings".to_owned())?;
            serializer.append_pair(name, value);
        }
        Some(serializer.finish().into_bytes())
    } else {
        options
            .get("body")
            .map(|value| match value {
                Value::String(text) => Ok(text.as_bytes().to_vec()),
                _ => serde_json::to_vec(value).map_err(|error| error.to_string()),
            })
            .transpose()?
    };
    if body
        .as_ref()
        .is_some_and(|body| body.len() > MAX_HTTP_BODY_BYTES)
    {
        return Err("HTTP request body exceeds the 1 MiB limit".into());
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bounded_http_options() {
        let request = parse_http_request(
            "https://example.com/api",
            json!({
                "method":"post",
                "headers":{"x-test":"yes"},
                "form":{"q":"hello world"},
                "timeout":999_999
            }),
        )
        .unwrap();
        assert_eq!(request.method, Method::POST);
        assert_eq!(request.timeout, Duration::from_secs(30));
        assert_eq!(request.body.unwrap(), b"q=hello+world");
    }

    #[test]
    fn rejects_dangerous_methods_and_oversized_bodies() {
        assert!(parse_http_request("https://example.com", json!({"method":"CONNECT"})).is_err());
        assert!(
            parse_http_request(
                "https://example.com",
                json!({"body":"x".repeat(MAX_HTTP_BODY_BYTES + 1)})
            )
            .is_err()
        );
    }

    #[test]
    fn builds_only_music_url_requests_from_lx_catalog_payloads() {
        let track = CatalogTrack {
            provider_id: "kw".into(),
            provider_track_id: "228908".into(),
            title: "晴天".into(),
            artist: "周杰伦".into(),
            album: "叶惠美".into(),
            duration_ms: Some(269_000),
            artwork_url: None,
            resolver_payload: json!({
                "source": "kw",
                "musicInfo": {
                    "name": "晴天",
                    "singer": "周杰伦",
                    "source": "kw",
                    "songmid": "228908"
                }
            }),
            preview: None,
        };
        let payload = lx_music_url_payload(&track, "320k").unwrap();
        assert_eq!(payload["action"], "musicUrl");
        assert_eq!(payload["source"], "kw");
        assert_eq!(payload["info"]["type"], "320k");
        assert_eq!(payload["info"]["musicInfo"]["songmid"], "228908");
    }

    #[test]
    fn rejects_non_lx_catalog_payloads() {
        let track = CatalogTrack {
            provider_id: "itunes".into(),
            provider_track_id: "1".into(),
            title: "Song".into(),
            artist: "Artist".into(),
            album: String::new(),
            duration_ms: None,
            artwork_url: None,
            resolver_payload: json!({ "provider": "itunes", "trackId": 1 }),
            preview: None,
        };
        assert!(lx_music_url_payload(&track, "128k").is_err());
    }

    #[test]
    fn preview_guard_scales_with_catalog_duration() {
        assert_eq!(minimum_full_track_bytes(None), 512 * 1024);
        assert_eq!(minimum_full_track_bytes(Some(269_000)), 807_000);
        assert!(185_336 < minimum_full_track_bytes(Some(269_000)));
        assert!(10_792_943 > minimum_full_track_bytes(Some(269_000)));
        assert_eq!(
            parse_content_range_total("bytes 0-0/10792943"),
            Some(10_792_943)
        );
        assert_eq!(parse_content_range_total("bytes */*"), None);
    }

    #[test]
    fn quality_attempts_follow_per_platform_capabilities() {
        let capabilities = json!({
            "sources": {
                "wy": { "qualitys": ["128k", "320k", "flac", "hires"] },
                "kg": { "qualitys": ["128k", "320k", "flac", "flac24bit"] },
                "legacy": ["128k", "320k"]
            }
        });
        assert_eq!(
            quality_attempts(&capabilities, "wy", None),
            ["flac", "320k", "128k"]
        );
        assert_eq!(
            quality_attempts(&capabilities, "kg", None),
            ["flac24bit", "flac", "320k", "128k"]
        );
        assert_eq!(
            quality_attempts(&capabilities, "wy", Some("flac24bit")),
            ["flac", "320k", "128k"]
        );
        assert_eq!(
            quality_attempts(&capabilities, "legacy", Some("flac")),
            ["320k", "128k"]
        );
    }

    #[test]
    fn splits_structured_and_legacy_source_config() {
        let structured = json!({
            "lsConfig": { "api": { "addr": "https://api.example" } },
            "keyOverrides": [{ "constName": "YuNingXi", "value": "secret" }]
        });
        let (ls_config, overrides) = split_source_config(&structured);
        assert_eq!(ls_config["api"]["addr"], "https://api.example");
        assert_eq!(overrides[0]["constName"], "YuNingXi");

        let legacy = json!({ "api": { "pass": "old-secret" } });
        let (ls_config, overrides) = split_source_config(&legacy);
        assert_eq!(ls_config, legacy);
        assert!(overrides.is_empty());
    }

    #[test]
    fn key_override_only_replaces_first_anchored_const_declaration() {
        let script = "const YuNingXi = ''; // key\nconst YuNingXi = 'second';\nlet YuNingXi = 'third';\nconst Other = 'YuNingXi';";
        let output = apply_key_overrides(
            script,
            &[json!({ "constName": "YuNingXi", "value": "a'\\\"b\\nc" })],
        )
        .unwrap();
        assert!(output.starts_with("const YuNingXi = \"a'\\\\\\\"b\\\\nc\"; // key"));
        assert!(output.contains("const YuNingXi = 'second';"));
        assert!(output.contains("let YuNingXi = 'third';"));
        assert!(output.contains("const Other = 'YuNingXi';"));
    }

    #[test]
    fn key_override_skips_missing_or_unsafe_constant_names() {
        let script = "const Safe = 'original';";
        let output = apply_key_overrides(
            script,
            &[
                json!({ "constName": "Missing", "value": "x" }),
                json!({ "constName": "Safe; globalThis.pwned", "value": "x" }),
            ],
        )
        .unwrap();
        assert_eq!(output, script);
    }
}
