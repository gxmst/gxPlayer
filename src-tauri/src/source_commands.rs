use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use gx_audio::engine::LocalAudioEngine;
use gx_contracts::ResolvedMediaRequest;
use gx_source::SourceBackup;
use gx_source::safe_http::{SafeHttpRequest, execute};
use reqwest::{Method, Url};
use serde::Serialize;
use serde_json::{Map, Value, json};
use tauri::{AppHandle, Manager, WebviewWindow};

use crate::source_runtime::{
    ListedSource, RuntimeStatus, ScriptLaunch, SourceRuntime, normalize_media_request,
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
    pub source: gx_source::ManagedSource,
    pub runtime: RuntimeStatus,
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
        source,
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
        source,
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
            Ok(())
        }
        _ => Err(format!("unsupported lx.send event: {event_name}")),
    }
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
            for _ in 0..600 {
                let snapshot = app_for_monitor.state::<LocalAudioEngine>().snapshot();
                if snapshot.status == gx_contracts::PlaybackStatus::Playing
                    && snapshot.position_seconds > 0.2
                {
                    println!(
                        "GX_PHASE2_NATIVE_STREAM_PLAYBACK_OK position={:.3} underruns={}",
                        snapshot.position_seconds, snapshot.underrun_callbacks
                    );
                    return Ok(());
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
    let script = serde_json::to_string(&launch.script).map_err(|error| error.to_string())?;
    let config = if std::env::var_os("GX_PHASE2_LX_MOCK").is_some() {
        json!({ "api": { "addr": "http://gx.invalid/", "pass": "" } })
    } else {
        json!({})
    };
    let context = json!({
        "generation": launch.generation,
        "poc": false,
        "scriptInfo": {
            "name": launch.source.metadata.name,
            "version": launch.source.metadata.version,
            "author": launch.source.metadata.author,
            "homepage": launch.source.metadata.homepage,
            "rawScript": launch.script
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
}
