use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use gx_audio::engine::{EngineSnapshot, LocalAudioEngine};
use gx_contracts::ResolvedMediaRequest;
use gx_dsp::DspSettings;
use gx_source::{SourceStore, safe_http};

mod metadata_commands;
mod source_commands;
mod source_runtime;

use metadata_commands::{
    maybe_start_phase3_smoke, metadata_chart, metadata_find_replacements, metadata_lyrics,
    metadata_play_preview, metadata_search,
};
use source_commands::{
    lx_http_request, lx_runtime_failure, lx_runtime_result, lx_send, source_activate,
    source_export_backup, source_import_file, source_import_url, source_list, source_reload,
    source_remove, source_resolve, source_restore_backup, source_set_updates_enabled,
    source_status,
};
use source_runtime::SourceRuntime;

pub(crate) const SANDBOX_LABEL: &str = "lx-sandbox";

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
fn player_load_local(
    window: WebviewWindow,
    engine: tauri::State<LocalAudioEngine>,
    paths: Vec<String>,
) -> Result<(), String> {
    require_window(&window, "main")?;
    let paths = paths.into_iter().map(PathBuf::from).collect();
    engine.load(paths).map_err(|error| error.to_string())
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

#[tauri::command]
fn player_set_output_device(
    window: WebviewWindow,
    engine: tauri::State<LocalAudioEngine>,
    name: Option<String>,
) -> Result<(), String> {
    require_window(&window, "main")?;
    engine
        .set_output_device(name)
        .map_err(|error| error.to_string())
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
    let audio_engine = LocalAudioEngine::new().expect("failed to create local audio engine");
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(audio_engine)
        .manage(LxPocState {
            script_path: phase1_script_path(),
            progress: Mutex::new(LxPocProgress::default()),
        })
        .setup(|app| {
            let source_root = app.path().app_data_dir()?.join("sources");
            let mut source_store = SourceStore::open(source_root)?;
            if let Some(path) = std::env::var_os("GX_PHASE2_LX_SCRIPT") {
                source_store.import_file(&PathBuf::from(path))?;
            }
            app.manage(SourceRuntime::new(source_store));
            create_lx_sandbox(app.handle())?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            main_only_probe,
            ui_ready,
            player_load_local,
            player_load_resolved,
            player_play,
            player_pause,
            player_seek,
            player_set_volume,
            player_set_dsp_settings,
            player_next,
            player_previous,
            player_snapshot,
            player_output_devices,
            player_set_output_device,
            sandbox_ready,
            source_list,
            source_status,
            source_import_file,
            source_import_url,
            source_activate,
            source_remove,
            source_reload,
            source_set_updates_enabled,
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
            lx_poc_failure
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
