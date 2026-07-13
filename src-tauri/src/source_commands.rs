use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use gx_audio::engine::LocalAudioEngine;
use gx_cache::{CacheKey, CacheStore};
use gx_contracts::ResolvedMediaRequest;
use gx_metadata::{
    CatalogTrack, find_replacements, search_all, search_kugou, search_kuwo, search_netease,
};
use gx_source::safe_http::{SafeHttpError, SafeHttpRequest, execute};
use gx_source::{SourceBackup, SourceFallbackConfig};
use reqwest::{Method, Url};
use serde::Serialize;
use serde_json::{Map, Value, json};
use tauri::{AppHandle, Emitter, Manager, WebviewWindow};

use crate::source_runtime::{
    ListedSource, PublicSource, RuntimeStatus, ScriptLaunch, SourceRuntime, ensure_json_size,
    normalize_media_request,
};
use crate::{LxHttpResponse, LxPocState, SANDBOX_LABEL, require_window};

const MAX_HTTP_OPTIONS_BYTES: usize = 64 * 1024;
const MAX_HTTP_BODY_BYTES: usize = 1024 * 1024;
const MAX_HTTP_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_SOURCE_DOWNLOAD_BYTES: usize = 5 * 1024 * 1024;
const RUNTIME_REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
const RUNTIME_INIT_TIMEOUT: Duration = Duration::from_secs(8);
const MEDIA_PROBE_TIMEOUT: Duration = Duration::from_secs(6);

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    pub source: PublicSource,
    pub runtime: RuntimeStatus,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OnlinePlaybackResult {
    pub outcome: ResolveOutcome,
    pub track: CatalogTrack,
    pub source_id: Option<String>,
    pub source_name: Option<String>,
    pub quality: Option<String>,
    pub cache_hit: bool,
    pub attempts: Vec<ResolveAttemptDiagnostic>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResolveOutcome {
    Started,
    Failed,
    Cancelled,
    Stale,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ResolveAttemptDiagnostic {
    pub source_id: Option<String>,
    pub source_name: Option<String>,
    pub provider_id: String,
    pub provider_track_id: String,
    pub quality: Option<String>,
    pub stage: String,
    pub success: bool,
    pub error: Option<String>,
}

const RESOLVE_ACTIVE: u8 = 0;
const RESOLVE_CANCELLED: u8 = 1;
const RESOLVE_STALE: u8 = 2;

#[derive(Clone)]
pub(crate) struct ResolveToken {
    request_id: String,
    state: Arc<AtomicU8>,
}

impl ResolveToken {
    pub(crate) fn outcome(&self) -> Option<ResolveOutcome> {
        match self.state.load(Ordering::Acquire) {
            RESOLVE_CANCELLED => Some(ResolveOutcome::Cancelled),
            RESOLVE_STALE => Some(ResolveOutcome::Stale),
            _ => None,
        }
    }
}

#[derive(Default)]
struct ResolveRegistryInner {
    current_request_id: Option<String>,
    requests: HashMap<String, Arc<AtomicU8>>,
}

#[derive(Default)]
pub struct ResolveCancellationRegistry {
    inner: Mutex<ResolveRegistryInner>,
}

impl ResolveCancellationRegistry {
    pub(crate) fn begin(&self, request_id: String) -> Result<ResolveToken, String> {
        let request_id = request_id.trim().to_owned();
        if request_id.is_empty() || request_id.len() > 160 {
            return Err("resolve requestId must contain 1 to 160 characters".into());
        }
        let mut inner = self.inner.lock().unwrap();
        if let Some(previous_id) = inner.current_request_id.take()
            && previous_id != request_id
            && let Some(previous) = inner.requests.get(&previous_id)
        {
            previous.store(RESOLVE_STALE, Ordering::Release);
        }
        if let Some(previous) = inner.requests.remove(&request_id) {
            previous.store(RESOLVE_STALE, Ordering::Release);
        }
        let state = Arc::new(AtomicU8::new(RESOLVE_ACTIVE));
        inner.requests.insert(request_id.clone(), state.clone());
        inner.current_request_id = Some(request_id.clone());
        Ok(ResolveToken { request_id, state })
    }

    fn cancel(&self, request_id: &str) -> bool {
        let inner = self.inner.lock().unwrap();
        let Some(state) = inner.requests.get(request_id) else {
            return false;
        };
        state.store(RESOLVE_CANCELLED, Ordering::Release);
        true
    }

    pub(crate) fn run_if_active<T>(
        &self,
        token: &ResolveToken,
        operation: impl FnOnce() -> T,
    ) -> Result<T, ResolveOutcome> {
        let inner = self.inner.lock().unwrap();
        let owns_current = inner.current_request_id.as_deref() == Some(token.request_id.as_str())
            && inner
                .requests
                .get(&token.request_id)
                .is_some_and(|state| Arc::ptr_eq(state, &token.state));
        if !owns_current {
            return Err(token.outcome().unwrap_or(ResolveOutcome::Stale));
        }
        if let Some(outcome) = token.outcome() {
            return Err(outcome);
        }
        Ok(operation())
    }

    pub(crate) fn finish(&self, token: &ResolveToken) {
        let mut inner = self.inner.lock().unwrap();
        let owns_request = inner
            .requests
            .get(&token.request_id)
            .is_some_and(|state| Arc::ptr_eq(state, &token.state));
        if owns_request {
            inner.requests.remove(&token.request_id);
            if inner.current_request_id.as_deref() == Some(token.request_id.as_str()) {
                inner.current_request_id = None;
            }
        }
    }
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
pub fn source_get_fallback_config(
    window: WebviewWindow,
    runtime: tauri::State<SourceRuntime>,
) -> Result<SourceFallbackConfig, String> {
    require_window(&window, "main")?;
    Ok(runtime.fallback_config())
}

#[tauri::command]
pub fn source_set_fallback_config(
    window: WebviewWindow,
    runtime: tauri::State<SourceRuntime>,
    enabled: bool,
    source_ids: Vec<String>,
) -> Result<SourceFallbackConfig, String> {
    require_window(&window, "main")?;
    runtime.serialized(|| {
        runtime
            .set_fallback_config(enabled, source_ids)
            .map_err(|error| error.to_string())
    })?;
    Ok(runtime.fallback_config())
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
        resolve_with_fallback(&app, payload, quality.as_deref(), source_id.as_deref())
    })
    .await
    .map_err(|error| format!("LX resolver task failed: {error}"))?
}

fn resolve_with_fallback(
    app: &AppHandle,
    payload: Value,
    quality: Option<&str>,
    requested_source_id: Option<&str>,
) -> Result<ResolvedMediaRequest, String> {
    let source_ids = app
        .state::<SourceRuntime>()
        .resolution_source_ids(requested_source_id)
        .map_err(|error| error.to_string())?;
    if source_ids.is_empty() {
        return Err("no LX source is available".into());
    }
    let mut errors = Vec::new();
    for source_id in source_ids {
        match resolve_serialized(app, payload.clone(), quality, Some(&source_id)) {
            Ok(request) => return Ok(request),
            Err(error) => errors.push(format!("{source_id}: {error}")),
        }
    }
    Err(format!(
        "all LX source fallbacks failed ({})",
        errors.join("; ")
    ))
}

#[tauri::command]
pub async fn player_play_online_track(
    window: WebviewWindow,
    track: CatalogTrack,
    quality: Option<String>,
    source_id: Option<String>,
    request_id: Option<String>,
) -> Result<OnlinePlaybackResult, String> {
    require_window(&window, "main")?;
    let app = window.app_handle().clone();
    let token = request_id
        .map(|request_id| app.state::<ResolveCancellationRegistry>().begin(request_id))
        .transpose()?;
    let token_for_worker = token.clone();
    let app_for_worker = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        play_online_track(
            &app_for_worker,
            track,
            quality,
            source_id,
            token_for_worker.as_ref(),
        )
    })
    .await
    .map_err(|error| format!("online playback task failed: {error}"))?;
    if let Some(token) = token.as_ref() {
        app.state::<ResolveCancellationRegistry>().finish(token);
    }
    result
}

#[tauri::command]
pub fn player_cancel_resolve(
    window: WebviewWindow,
    registry: tauri::State<ResolveCancellationRegistry>,
    request_id: String,
) -> Result<bool, String> {
    require_window(&window, "main")?;
    Ok(registry.cancel(request_id.trim()))
}

fn play_online_track(
    app: &AppHandle,
    track: CatalogTrack,
    quality: Option<String>,
    source_id: Option<String>,
    cancellation: Option<&ResolveToken>,
) -> Result<OnlinePlaybackResult, String> {
    // Constraint 2 audit trail: each call is one on-demand resolve (never batch at enqueue).
    println!(
        "GX_ONLINE_RESOLVE provider={} id={} title={} quality={}",
        track.provider_id,
        track.provider_track_id,
        track.title,
        quality.as_deref().unwrap_or("auto")
    );
    let original_track = track.clone();
    let mut diagnostics = Vec::new();
    if let Some(outcome) = cancellation.and_then(ResolveToken::outcome) {
        return Ok(terminal_playback_result(
            original_track,
            outcome,
            diagnostics,
            None,
        ));
    }

    let runtime = app.state::<SourceRuntime>();
    let source_ids = runtime
        .resolution_source_ids(source_id.as_deref())
        .map_err(|error| error.to_string())?;

    // A direct catalog identity lets us reuse an already verified cache without doing metadata
    // replacement searches or starting a JavaScript runtime.
    if let Some((provider, _)) = lx_identity(&track) {
        let capabilities = runtime.status().capabilities;
        for attempt in quality_attempts(&capabilities, provider, quality.as_deref()) {
            if let Some(outcome) = cancellation.and_then(ResolveToken::outcome) {
                return Ok(terminal_playback_result(
                    original_track,
                    outcome,
                    diagnostics,
                    None,
                ));
            }
            if let Some(result) =
                play_cache_hit(app, &track, &attempt, cancellation, &mut diagnostics)?
            {
                return Ok(result);
            }
        }
    }

    let candidates = match select_lx_candidates(track) {
        Ok(candidates) => candidates,
        Err(error) => {
            if let Some(outcome) = cancellation.and_then(ResolveToken::outcome) {
                return Ok(terminal_playback_result(
                    original_track,
                    outcome,
                    diagnostics,
                    None,
                ));
            }
            return Ok(terminal_playback_result(
                original_track,
                ResolveOutcome::Failed,
                diagnostics,
                Some(error),
            ));
        }
    };

    // Cache entries are source-independent after verification, so check every candidate once
    // before paying the cost of switching community runtimes.
    for candidate in &candidates {
        let provider = lx_identity(candidate)
            .map(|(provider, _)| provider)
            .ok_or_else(|| "candidate lost its LX source identity".to_owned())?;
        for attempt in quality_attempts(&Value::Null, provider, quality.as_deref()) {
            if let Some(outcome) = cancellation.and_then(ResolveToken::outcome) {
                return Ok(terminal_playback_result(
                    original_track,
                    outcome,
                    diagnostics,
                    None,
                ));
            }
            if let Some(result) =
                play_cache_hit(app, candidate, &attempt, cancellation, &mut diagnostics)?
            {
                return Ok(result);
            }
        }
    }

    // A cache miss is the point at which a live LX source becomes necessary.
    // Keep the cache-only path usable even when all imported sources are disabled.
    if source_ids.is_empty() {
        return Ok(terminal_playback_result(
            original_track,
            ResolveOutcome::Failed,
            diagnostics,
            Some("没有已导入且可用的 LX 音源".into()),
        ));
    }

    for runtime_source_id in &source_ids {
        if let Some(outcome) = cancellation.and_then(ResolveToken::outcome) {
            return Ok(terminal_playback_result(
                original_track,
                outcome,
                diagnostics,
                None,
            ));
        }
        if let Some(resolved) = resolve_candidates_with_source(
            app,
            runtime_source_id,
            &candidates,
            quality.as_deref(),
            cancellation,
            &mut diagnostics,
        )? {
            if let Some(outcome) = cancellation.and_then(ResolveToken::outcome) {
                return Ok(terminal_playback_result(
                    original_track,
                    outcome,
                    diagnostics,
                    None,
                ));
            }
            let cache_plan = app.state::<CacheStore>().prepare_with_meta(
                CacheKey {
                    provider_id: resolved.track.provider_id.clone(),
                    provider_track_id: resolved.track.provider_track_id.clone(),
                    quality: resolved.quality.clone(),
                },
                resolved.request.media_type.clone(),
                resolved.track.title.clone(),
                resolved.track.artist.clone(),
            );
            if let Some(outcome) = cancellation.and_then(ResolveToken::outcome) {
                return Ok(terminal_playback_result(
                    original_track,
                    outcome,
                    diagnostics,
                    None,
                ));
            }
            let engine = app.state::<LocalAudioEngine>();
            let minimum_generation = crate::media_session::next_engine_generation(&engine);
            let location = resolved.request.redacted_for_log();
            let commit = run_playback_commit(app, cancellation, || {
                engine
                    .load_resolved_cached(
                        resolved.request,
                        resolved.track.title.clone(),
                        Some(cache_plan),
                    )
                    .map_err(|error| format!("Rust streaming engine rejected LX media: {error}"))?;
                crate::media_session::set_online_metadata(
                    app,
                    &resolved.track,
                    minimum_generation,
                    Some(location),
                );
                Ok::<_, String>(())
            });
            match commit {
                Ok(result) => result?,
                Err(outcome) => {
                    return Ok(terminal_playback_result(
                        original_track,
                        outcome,
                        diagnostics,
                        None,
                    ));
                }
            }
            return Ok(OnlinePlaybackResult {
                outcome: ResolveOutcome::Started,
                track: resolved.track,
                source_id: Some(resolved.source_id),
                source_name: resolved.source_name,
                quality: Some(resolved.quality),
                cache_hit: false,
                attempts: diagnostics,
                error: None,
            });
        }
    }

    if let Some(outcome) = cancellation.and_then(ResolveToken::outcome) {
        return Ok(terminal_playback_result(
            original_track,
            outcome,
            diagnostics,
            None,
        ));
    }
    let error = format_attempt_failure(&diagnostics);
    Ok(terminal_playback_result(
        original_track,
        ResolveOutcome::Failed,
        diagnostics,
        Some(error),
    ))
}

fn play_cache_hit(
    app: &AppHandle,
    track: &CatalogTrack,
    quality: &str,
    cancellation: Option<&ResolveToken>,
    diagnostics: &mut Vec<ResolveAttemptDiagnostic>,
) -> Result<Option<OnlinePlaybackResult>, String> {
    let key = CacheKey {
        provider_id: track.provider_id.clone(),
        provider_track_id: track.provider_track_id.clone(),
        quality: quality.to_owned(),
    };
    let Some(hit) = app.state::<CacheStore>().lookup(&key) else {
        return Ok(None);
    };
    let engine = app.state::<LocalAudioEngine>();
    let minimum_generation = crate::media_session::next_engine_generation(&engine);
    let location = hit.audio_path.display().to_string();
    let commit = run_playback_commit(app, cancellation, || {
        engine
            .load_cached_online(hit.audio_path, track.title.clone())
            .map_err(|error| format!("Rust audio engine rejected cached media: {error}"))?;
        crate::media_session::set_online_metadata(app, track, minimum_generation, Some(location));
        Ok::<_, String>(())
    });
    match commit {
        Ok(result) => result?,
        Err(outcome) => {
            return Ok(Some(terminal_playback_result(
                track.clone(),
                outcome,
                diagnostics.clone(),
                None,
            )));
        }
    }
    diagnostics.push(ResolveAttemptDiagnostic {
        source_id: None,
        source_name: None,
        provider_id: track.provider_id.clone(),
        provider_track_id: track.provider_track_id.clone(),
        quality: Some(quality.to_owned()),
        stage: "cache".into(),
        success: true,
        error: None,
    });
    Ok(Some(OnlinePlaybackResult {
        outcome: ResolveOutcome::Started,
        track: track.clone(),
        source_id: None,
        source_name: None,
        quality: Some(quality.to_owned()),
        cache_hit: true,
        attempts: diagnostics.clone(),
        error: None,
    }))
}

fn source_identity(app: &AppHandle, source_id: Option<&str>) -> (Option<String>, Option<String>) {
    let selected_source_id = source_id
        .map(str::to_owned)
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

fn terminal_playback_result(
    track: CatalogTrack,
    outcome: ResolveOutcome,
    attempts: Vec<ResolveAttemptDiagnostic>,
    error: Option<String>,
) -> OnlinePlaybackResult {
    OnlinePlaybackResult {
        outcome,
        track,
        source_id: None,
        source_name: None,
        quality: None,
        cache_hit: false,
        attempts,
        error,
    }
}

fn run_playback_commit<T>(
    app: &AppHandle,
    cancellation: Option<&ResolveToken>,
    operation: impl FnOnce() -> T,
) -> Result<T, ResolveOutcome> {
    match cancellation {
        Some(token) => app
            .state::<ResolveCancellationRegistry>()
            .run_if_active(token, operation),
        None => Ok(operation()),
    }
}

fn format_attempt_failure(attempts: &[ResolveAttemptDiagnostic]) -> String {
    let details = attempts
        .iter()
        .filter(|attempt| !attempt.success)
        .filter_map(|attempt| {
            attempt.error.as_ref().map(|error| {
                format!(
                    "{} {}:{} {} {}: {error}",
                    attempt.source_name.as_deref().unwrap_or("LX"),
                    attempt.provider_id,
                    attempt.provider_track_id,
                    attempt.quality.as_deref().unwrap_or("auto"),
                    attempt.stage
                )
            })
        })
        .collect::<Vec<_>>();
    if details.is_empty() {
        "所有音源均无法返回结果".into()
    } else {
        format!("所有音源均无法返回结果（{}）", details.join("; "))
    }
}

fn public_attempt_error(stage: &str, error: &str) -> String {
    let error = error.to_ascii_lowercase();
    if error.contains("timed out") || error.contains("timeout") {
        return "timeout".into();
    }
    if error.contains("private destination")
        || error.contains("loopback")
        || error.contains("link-local")
    {
        return "blocked_destination".into();
    }
    if error.contains("http 401") || error.contains("http 403") {
        return "upstream_auth_rejected".into();
    }
    if error.contains("http 404") {
        return "upstream_not_found".into();
    }
    if error.contains("http 429") {
        return "upstream_rate_limited".into();
    }
    if error.contains("preview-sized") || error.contains("minimum full-track") {
        return "preview_or_truncated_media".into();
    }
    if error.contains("content-range") || error.contains("range probe") {
        return "range_verification_failed".into();
    }
    match stage {
        "initialize" => "source_initialization_failed",
        "payload" => "invalid_candidate_payload",
        "verify" => "media_verification_failed",
        "restore" => "active_source_restore_failed",
        _ => "source_resolution_failed",
    }
    .into()
}

fn should_skip_source(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    error.contains("timed out")
        || error.contains("timeout")
        || error.contains("http 401")
        || error.contains("http 403")
        || error.contains("http 429")
        || error.contains("runtime failed")
        || error.contains("sandbox window is unavailable")
}

struct ResolvedCandidate {
    track: CatalogTrack,
    request: ResolvedMediaRequest,
    quality: String,
    source_id: String,
    source_name: Option<String>,
}

fn resolve_candidates_with_source(
    app: &AppHandle,
    source_id: &str,
    candidates: &[CatalogTrack],
    quality_preference: Option<&str>,
    cancellation: Option<&ResolveToken>,
    diagnostics: &mut Vec<ResolveAttemptDiagnostic>,
) -> Result<Option<ResolvedCandidate>, String> {
    let runtime = app.state::<SourceRuntime>();
    let (_, source_name) = source_identity(app, Some(source_id));
    runtime.serialized(|| {
        let persistent_active = runtime
            .list()
            .into_iter()
            .find(|source| source.active)
            .map(|source| source.source.id);
        let temporary = persistent_active.as_deref() != Some(source_id);
        let status = runtime.status();
        let needs_launch = status.active_source_id.as_deref() != Some(source_id)
            || status.state != crate::source_runtime::RuntimeState::Ready;
        if needs_launch {
            let switched = (|| {
                let launch = runtime
                    .prepare_reload_for(source_id)
                    .map_err(|error| error.to_string())?;
                let sandbox = app
                    .get_webview_window(SANDBOX_LABEL)
                    .ok_or_else(|| "LX sandbox window is unavailable".to_owned())?;
                evaluate_launch(&sandbox, &launch)?;
                wait_until_ready(
                    &runtime,
                    launch.generation,
                    RUNTIME_INIT_TIMEOUT,
                    cancellation,
                )
            })();
            if let Err(error) = switched {
                if cancellation.and_then(ResolveToken::outcome).is_none() {
                    let first = candidates.first();
                    diagnostics.push(ResolveAttemptDiagnostic {
                        source_id: Some(source_id.to_owned()),
                        source_name: source_name.clone(),
                        provider_id: first
                            .map_or_else(String::new, |track| track.provider_id.clone()),
                        provider_track_id: first
                            .map_or_else(String::new, |track| track.provider_track_id.clone()),
                        quality: quality_preference.map(str::to_owned),
                        stage: "initialize".into(),
                        success: false,
                        error: Some(public_attempt_error("initialize", &error)),
                    });
                }
                if temporary {
                    let _ = restore_persistent_runtime_background(app, &runtime);
                }
                return Ok(None);
            }
        }

        let capabilities = runtime.status().capabilities;
        let mut resolved = None;
        'candidate: for candidate in candidates {
            if cancellation.and_then(ResolveToken::outcome).is_some() {
                break;
            }
            let Some(provider) = lx_identity(candidate).map(|(provider, _)| provider) else {
                diagnostics.push(ResolveAttemptDiagnostic {
                    source_id: Some(source_id.to_owned()),
                    source_name: source_name.clone(),
                    provider_id: candidate.provider_id.clone(),
                    provider_track_id: candidate.provider_track_id.clone(),
                    quality: quality_preference.map(str::to_owned),
                    stage: "payload".into(),
                    success: false,
                    error: Some("invalid_candidate_identity".into()),
                });
                continue;
            };
            for quality in quality_attempts(&capabilities, provider, quality_preference) {
                if cancellation.and_then(ResolveToken::outcome).is_some() {
                    break 'candidate;
                }
                let payload = match lx_music_url_payload(candidate, &quality) {
                    Ok(payload) => payload,
                    Err(error) => {
                        diagnostics.push(ResolveAttemptDiagnostic {
                            source_id: Some(source_id.to_owned()),
                            source_name: source_name.clone(),
                            provider_id: candidate.provider_id.clone(),
                            provider_track_id: candidate.provider_track_id.clone(),
                            quality: Some(quality),
                            stage: "payload".into(),
                            success: false,
                            error: Some(public_attempt_error("payload", &error)),
                        });
                        continue;
                    }
                };
                let request = match dispatch_and_wait(
                    app,
                    &runtime,
                    &payload,
                    Some(&quality),
                    cancellation,
                ) {
                    Ok(request) => request,
                    Err(error) => {
                        if cancellation.and_then(ResolveToken::outcome).is_none() {
                            diagnostics.push(ResolveAttemptDiagnostic {
                                source_id: Some(source_id.to_owned()),
                                source_name: source_name.clone(),
                                provider_id: candidate.provider_id.clone(),
                                provider_track_id: candidate.provider_track_id.clone(),
                                quality: Some(quality),
                                stage: "resolve".into(),
                                success: false,
                                error: Some(public_attempt_error("resolve", &error)),
                            });
                        }
                        if should_skip_source(&error) {
                            break 'candidate;
                        }
                        continue;
                    }
                };
                if cancellation.and_then(ResolveToken::outcome).is_some() {
                    break 'candidate;
                }
                if let Err(error) =
                    validate_full_track_request(&request, candidate.duration_ms, Some(&quality))
                {
                    if cancellation.and_then(ResolveToken::outcome).is_none() {
                        diagnostics.push(ResolveAttemptDiagnostic {
                            source_id: Some(source_id.to_owned()),
                            source_name: source_name.clone(),
                            provider_id: candidate.provider_id.clone(),
                            provider_track_id: candidate.provider_track_id.clone(),
                            quality: Some(quality),
                            stage: "verify".into(),
                            success: false,
                            error: Some(public_attempt_error("verify", &error)),
                        });
                    }
                    if should_skip_source(&error) {
                        break 'candidate;
                    }
                    continue;
                }
                if cancellation.and_then(ResolveToken::outcome).is_some() {
                    break 'candidate;
                }
                let resolved_quality = request.quality.clone().unwrap_or(quality);
                diagnostics.push(ResolveAttemptDiagnostic {
                    source_id: Some(source_id.to_owned()),
                    source_name: source_name.clone(),
                    provider_id: candidate.provider_id.clone(),
                    provider_track_id: candidate.provider_track_id.clone(),
                    quality: Some(resolved_quality.clone()),
                    stage: "verify".into(),
                    success: true,
                    error: None,
                });
                resolved = Some(ResolvedCandidate {
                    track: candidate.clone(),
                    request,
                    quality: resolved_quality,
                    source_id: source_id.to_owned(),
                    source_name: source_name.clone(),
                });
                break 'candidate;
            }
        }

        if temporary && let Err(error) = restore_persistent_runtime_background(app, &runtime) {
            let first = candidates.first();
            diagnostics.push(ResolveAttemptDiagnostic {
                source_id: persistent_active.clone(),
                source_name: persistent_active
                    .as_deref()
                    .and_then(|id| source_identity(app, Some(id)).1),
                provider_id: first.map_or_else(String::new, |track| track.provider_id.clone()),
                provider_track_id: first
                    .map_or_else(String::new, |track| track.provider_track_id.clone()),
                quality: quality_preference.map(str::to_owned),
                stage: "restore".into(),
                success: false,
                error: Some(public_attempt_error("restore", &error)),
            });
        }
        Ok(resolved)
    })
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
    requested_quality: Option<&str>,
) -> Result<(), String> {
    let minimum_full_track_bytes =
        minimum_full_track_bytes(expected_duration_ms, requested_quality);
    let mut base_headers = request
        .headers
        .iter()
        .map(|header| (header.name.clone(), header.value.clone()))
        .collect::<Vec<_>>();
    base_headers.retain(|(name, _)| !name.eq_ignore_ascii_case("range"));

    let head_length = execute(SafeHttpRequest {
        url: request.url.clone(),
        method: Method::HEAD,
        headers: base_headers.clone(),
        body: None,
        timeout: MEDIA_PROBE_TIMEOUT,
        max_response_bytes: 0,
    })
    .ok()
    .filter(|response| (200..300).contains(&response.status))
    .and_then(|response| header_u64(&response.headers, "content-length"));
    if head_length.is_some_and(|length| length >= minimum_full_track_bytes) {
        return Ok(());
    }

    let mut headers = base_headers;
    headers.push(("range".into(), "bytes=0-0".into()));
    let response = execute(SafeHttpRequest {
        url: request.url.clone(),
        method: Method::GET,
        headers,
        body: None,
        timeout: MEDIA_PROBE_TIMEOUT,
        max_response_bytes: minimum_full_track_bytes as usize,
    });
    let total_length = match response {
        Ok(response) if response.status == 206 => response
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("content-range"))
            .and_then(|(_, value)| parse_content_range_total(value))
            .ok_or_else(|| {
                "resolved media Range probe omitted total Content-Range length".to_owned()
            })?,
        Ok(response) if response.status == 200 => {
            header_u64(&response.headers, "content-length").unwrap_or(response.body.len() as u64)
        }
        Ok(response) => {
            return Err(format!(
                "resolved media probe returned unsupported HTTP {}",
                response.status
            ));
        }
        Err(SafeHttpError::ResponseTooLarge { limit, status: 200 })
            if limit as u64 >= minimum_full_track_bytes =>
        {
            return Ok(());
        }
        Err(error) => {
            if let Some(length) = head_length {
                length
            } else {
                return Err(format!("resolved media probe failed: {error}"));
            }
        }
    };
    if total_length < minimum_full_track_bytes {
        return Err(format!(
            "resolved media is only {total_length} bytes (minimum full-track {minimum_full_track_bytes}); refusing preview-sized audio"
        ));
    }
    Ok(())
}

fn header_u64(headers: &[(String, String)], name: &str) -> Option<u64> {
    headers
        .iter()
        .find(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
        .and_then(|(_, value)| value.trim().parse().ok())
}

fn parse_content_range_total(value: &str) -> Option<u64> {
    let (_, total) = value.rsplit_once('/')?;
    (total != "*").then(|| total.parse().ok()).flatten()
}

fn minimum_full_track_bytes(
    expected_duration_ms: Option<u64>,
    requested_quality: Option<&str>,
) -> u64 {
    let bytes_per_second = match requested_quality {
        Some("flac24bit") => 32_000,
        Some("flac") => 24_000,
        Some("320k") => 20_000,
        Some("128k") => 8_000,
        _ => 4_000,
    };
    expected_duration_ms
        .map(|duration| duration.div_ceil(1000) * bytes_per_second)
        .unwrap_or(0)
        .clamp(512 * 1024, 8 * 1024 * 1024)
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
            let source_id = runtime.mark_ready(generation, data)?;
            if let Some(main_window) = app.get_webview_window("main")
                && let Err(error) = main_window.emit("gx-source-capabilities-updated", source_id)
            {
                eprintln!("source capabilities event failed: {error}");
            }
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
            let result = play_online_track(&app_for_play, track, Some("320k".into()), None, None)?;
            if result.outcome != ResolveOutcome::Started {
                return Err(result
                    .error
                    .clone()
                    .unwrap_or_else(|| format!("online E2E ended as {:?}", result.outcome)));
            }
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
                None,
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
                wait_until_ready(&runtime, launch.generation, RUNTIME_INIT_TIMEOUT, None)
            })();
            if let Err(error) = switched {
                let _ = restore_persistent_runtime(app, &runtime);
                return Err(error);
            }
        }
        let result = dispatch_and_wait(app, &runtime, &payload, quality, None);
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
        wait_until_ready(runtime, launch.generation, RUNTIME_INIT_TIMEOUT, None)?;
    }
    Ok(())
}

fn restore_persistent_runtime_background(
    app: &AppHandle,
    runtime: &SourceRuntime,
) -> Result<(), String> {
    let restore = runtime
        .prepare_reload()
        .map_err(|error| error.to_string())?;
    if let Some(launch) = restore {
        let sandbox = app
            .get_webview_window(SANDBOX_LABEL)
            .ok_or_else(|| "LX sandbox window is unavailable".to_owned())?;
        evaluate_launch(&sandbox, &launch)?;
        schedule_runtime_timeout(app.clone(), launch.generation);
    }
    Ok(())
}

fn dispatch_and_wait(
    app: &AppHandle,
    runtime: &SourceRuntime,
    payload: &Value,
    quality: Option<&str>,
    cancellation: Option<&ResolveToken>,
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
    let deadline = Instant::now() + RUNTIME_REQUEST_TIMEOUT;
    let raw = loop {
        if let Some(outcome) = cancellation.and_then(ResolveToken::outcome) {
            let reason = match outcome {
                ResolveOutcome::Cancelled => "LX resolver request cancelled",
                ResolveOutcome::Stale => "LX resolver request superseded",
                _ => "LX resolver request stopped",
            };
            runtime.cancel_request(&request_id, reason);
            return Err(reason.into());
        }
        let now = Instant::now();
        if now >= deadline {
            runtime.cancel_request(&request_id, "LX resolver request timed out");
            return Err("LX resolver request timed out".into());
        }
        let wait = (deadline - now).min(Duration::from_millis(75));
        match pending.receiver.recv_timeout(wait) {
            Ok(result) => break result?,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err("LX resolver response channel disconnected".into());
            }
        }
    };
    normalize_media_request(raw, quality)
}

fn wait_until_ready(
    runtime: &SourceRuntime,
    generation: u64,
    timeout: Duration,
    cancellation: Option<&ResolveToken>,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(outcome) = cancellation.and_then(ResolveToken::outcome) {
            return Err(match outcome {
                ResolveOutcome::Cancelled => "LX runtime initialization cancelled".into(),
                ResolveOutcome::Stale => "LX runtime initialization superseded".into(),
                _ => "LX runtime initialization stopped".into(),
            });
        }
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
    schedule_runtime_timeout(sandbox.app_handle().clone(), launch.generation);
    Ok(())
}

fn schedule_runtime_timeout(app: AppHandle, generation: u64) {
    tauri::async_runtime::spawn_blocking(move || {
        std::thread::sleep(RUNTIME_INIT_TIMEOUT);
        app.state::<SourceRuntime>()
            .fail_if_initializing(generation, "LX runtime initialization timed out".into());
    });
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
    fn replaced_request_finish_cannot_detach_the_new_owner() {
        let registry = ResolveCancellationRegistry::default();
        let old = registry.begin("same".into()).unwrap();
        let replacement = registry.begin("same".into()).unwrap();
        assert_eq!(old.outcome(), Some(ResolveOutcome::Stale));

        registry.finish(&old);
        let next = registry.begin("next".into()).unwrap();

        assert_eq!(replacement.outcome(), Some(ResolveOutcome::Stale));
        assert_eq!(next.outcome(), None);
    }

    #[test]
    fn cancelled_request_cannot_commit_playback() {
        let registry = ResolveCancellationRegistry::default();
        let token = registry.begin("cancelled".into()).unwrap();
        assert!(registry.cancel("cancelled"));
        let mut committed = false;

        let result = registry.run_if_active(&token, || committed = true);

        assert_eq!(result, Err(ResolveOutcome::Cancelled));
        assert!(!committed);
    }

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
    fn source_scoped_failures_skip_to_the_next_imported_source() {
        assert!(should_skip_source("LX resolver request timed out"));
        assert!(should_skip_source("upstream HTTP 429"));
        assert!(should_skip_source("HTTP 403 from source"));
        assert!(!should_skip_source(
            "resolved media failed content-range verification"
        ));
    }

    #[test]
    fn exhausted_fallbacks_have_a_clear_user_facing_error() {
        let message = format_attempt_failure(&[ResolveAttemptDiagnostic {
            source_id: Some("source-a".into()),
            source_name: Some("测试音源".into()),
            provider_id: "wy".into(),
            provider_track_id: "1".into(),
            quality: Some("320k".into()),
            stage: "resolve".into(),
            success: false,
            error: Some("timeout".into()),
        }]);
        assert!(message.starts_with("所有音源均无法返回结果"));
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
        assert_eq!(minimum_full_track_bytes(None, None), 512 * 1024);
        assert_eq!(
            minimum_full_track_bytes(Some(269_000), Some("128k")),
            2_152_000
        );
        assert_eq!(
            minimum_full_track_bytes(Some(269_000), Some("320k")),
            5_380_000
        );
        assert!(1_200_000 < minimum_full_track_bytes(Some(269_000), Some("320k")));
        assert!(10_792_943 > minimum_full_track_bytes(Some(269_000), Some("320k")));
        assert_eq!(
            parse_content_range_total("bytes 0-0/10792943"),
            Some(10_792_943)
        );
        assert_eq!(parse_content_range_total("bytes */*"), None);
        assert_eq!(
            header_u64(
                &[("Content-Length".into(), "10792943".into())],
                "content-length"
            ),
            Some(10_792_943)
        );
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
