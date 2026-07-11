use std::fs;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::{AppHandle, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use gx_audio::engine::{EngineSnapshot, LocalAudioEngine};

const SANDBOX_LABEL: &str = "lx-sandbox";

struct LxPocState {
    script_path: PathBuf,
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
struct LxHttpResponse {
    status_code: u16,
    headers: std::collections::BTreeMap<String, String>,
    body: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SecurityResults {
    main_command_blocked: bool,
    opener_blocked: bool,
    ssrf_blocked: bool,
}

fn require_window(window: &WebviewWindow, expected: &str) -> Result<(), String> {
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
fn sandbox_ready(window: WebviewWindow, state: tauri::State<LxPocState>) -> Result<(), String> {
    require_window(&window, SANDBOX_LABEL)?;
    let script = fs::read_to_string(&state.script_path).map_err(|error| {
        format!(
            "failed to read community LX script {}: {error}",
            state.script_path.display()
        )
    })?;
    let encoded = serde_json::to_string(&script).map_err(|error| error.to_string())?;
    window
        .eval(format!("window.__gxRunCommunityScript({encoded})"))
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn lx_http_request(
    window: WebviewWindow,
    url: String,
    options: Value,
) -> Result<LxHttpResponse, String> {
    require_window(&window, SANDBOX_LABEL)?;
    let parsed = reqwest::Url::parse(&url).map_err(|error| format!("invalid URL: {error}"))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err("only HTTP(S) is allowed".into());
    }
    if parsed.username() != "" || parsed.password().is_some() {
        return Err("credentials in URLs are not allowed".into());
    }
    if is_private_destination(&parsed) {
        return Err("loopback, link-local, and private-network destinations are denied".into());
    }
    if options.to_string().len() > 64 * 1024 {
        return Err("HTTP options exceed the Phase-1 size limit".into());
    }
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
        json!({
            "code": 0,
            "msg": "ok",
            "data": "https://media.example/phase-1.mp3"
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

#[tauri::command]
fn lx_send(window: WebviewWindow, event_name: String, data: Value) -> Result<(), String> {
    require_window(&window, SANDBOX_LABEL)?;
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
    if !(results.main_command_blocked && results.opener_blocked && results.ssrf_blocked) {
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

fn is_private_destination(url: &reqwest::Url) -> bool {
    let Some(host) = url.host_str() else {
        return true;
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(address)) => private_ipv4(address),
        Ok(IpAddr::V6(address)) => private_ipv6(address),
        Err(_) => false,
    }
}

fn private_ipv4(address: Ipv4Addr) -> bool {
    address.is_private()
        || address.is_loopback()
        || address.is_link_local()
        || address.is_broadcast()
        || address.is_unspecified()
}

fn private_ipv6(address: Ipv6Addr) -> bool {
    address.is_loopback()
        || address.is_unspecified()
        || (address.segments()[0] & 0xfe00) == 0xfc00
        || (address.segments()[0] & 0xffc0) == 0xfe80
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
            if std::env::var_os("GX_PHASE1_LX_POC").is_some() {
                WebviewWindowBuilder::new(
                    app,
                    SANDBOX_LABEL,
                    WebviewUrl::App("sandbox.html".into()),
                )
                .title("GXPlayer LX Sandbox")
                .visible(false)
                .build()?;
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            main_only_probe,
            ui_ready,
            player_load_local,
            player_play,
            player_pause,
            player_seek,
            player_set_volume,
            player_next,
            player_previous,
            player_snapshot,
            sandbox_ready,
            lx_http_request,
            lx_send,
            lx_poc_result,
            lx_crypto_result,
            lx_security_result,
            lx_poc_failure
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
