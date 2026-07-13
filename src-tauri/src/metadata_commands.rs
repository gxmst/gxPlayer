use gx_audio::engine::LocalAudioEngine;
use gx_metadata::{
    CatalogTrack, LyricDocument, ReplacementMatch, SearchBatch, apple_chart, fetch_lyrics,
    fetch_preview_bytes, find_replacements, preview_is_available, search_all,
    search_all_progressive, select_playable_with,
};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, WebviewWindow};

use crate::require_window;
use crate::source_commands::{ResolveCancellationRegistry, ResolveToken};

static PHASE3_SMOKE_STARTED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectedPlayback {
    pub track: CatalogTrack,
    pub replaced_provider_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CatalogSearchBatchEvent {
    request_id: String,
    provider_id: String,
    tracks: Vec<CatalogTrack>,
    error: Option<String>,
}

#[tauri::command]
pub async fn metadata_search(
    window: WebviewWindow,
    query: String,
    limit: Option<usize>,
    request_id: Option<String>,
) -> Result<Vec<CatalogTrack>, String> {
    require_window(&window, "main")?;
    let event_window = window.clone();
    tauri::async_runtime::spawn_blocking(move || {
        search_all_progressive(&query, limit.unwrap_or(15), |batch: SearchBatch| {
            let Some(request_id) = request_id.as_ref() else {
                return;
            };
            let payload = CatalogSearchBatchEvent {
                request_id: request_id.clone(),
                provider_id: batch.provider_id,
                tracks: batch.tracks,
                error: batch.error,
            };
            if let Err(error) = event_window.emit("gx-catalog-search-batch", payload) {
                eprintln!("catalog search batch emit failed: {error}");
            }
        })
    })
    .await
    .map_err(|error| format!("metadata search task failed: {error}"))?
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn metadata_chart(
    window: WebviewWindow,
    limit: Option<usize>,
) -> Result<Vec<CatalogTrack>, String> {
    require_window(&window, "main")?;
    tauri::async_runtime::spawn_blocking(move || apple_chart(limit.unwrap_or(25)))
        .await
        .map_err(|error| format!("metadata chart task failed: {error}"))?
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn metadata_lyrics(
    window: WebviewWindow,
    title: String,
    artist: String,
    duration_ms: Option<u64>,
) -> Result<Option<LyricDocument>, String> {
    require_window(&window, "main")?;
    tauri::async_runtime::spawn_blocking(move || fetch_lyrics(&title, &artist, duration_ms))
        .await
        .map_err(|error| format!("lyrics task failed: {error}"))?
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn metadata_find_replacements(
    window: WebviewWindow,
    wanted: CatalogTrack,
    candidates: Vec<CatalogTrack>,
) -> Result<Vec<ReplacementMatch>, String> {
    require_window(&window, "main")?;
    Ok(find_replacements(&wanted, candidates))
}

#[tauri::command]
pub async fn metadata_play_preview(
    window: WebviewWindow,
    wanted: CatalogTrack,
    candidates: Vec<CatalogTrack>,
    request_id: Option<String>,
) -> Result<SelectedPlayback, String> {
    require_window(&window, "main")?;
    let app = window.app_handle().clone();
    let token = request_id
        .map(|request_id| app.state::<ResolveCancellationRegistry>().begin(request_id))
        .transpose()?;
    let result = async {
        let (selected, replaced_provider_id) = tauri::async_runtime::spawn_blocking(move || {
            select_playable_with(&wanted, candidates, preview_is_available)
        })
        .await
        .map_err(|error| format!("preview availability task failed: {error}"))?
        .ok_or_else(|| {
            "no playable preview exists on the selected or replacement platform".to_owned()
        })?;
        ensure_preview_active(token.as_ref())?;
        let cached = cache_preview(&app, &selected).await?;
        ensure_preview_active(token.as_ref())?;
        let engine = app.state::<LocalAudioEngine>();
        let minimum_generation = crate::media_session::next_engine_generation(&engine);
        let location = cached.display().to_string();
        let load = || {
            engine
                .load(vec![cached])
                .map_err(|error| error.to_string())?;
            crate::media_session::set_online_metadata(
                &app,
                &selected,
                minimum_generation,
                Some(location),
            );
            Ok::<_, String>(())
        };
        match token.as_ref() {
            Some(token) => app
                .state::<ResolveCancellationRegistry>()
                .run_if_active(token, load)
                .map_err(|outcome| format!("preview request ended as {outcome:?}"))??,
            None => load()?,
        }
        Ok(SelectedPlayback {
            track: selected,
            replaced_provider_id,
        })
    }
    .await;
    if let Some(token) = token.as_ref() {
        app.state::<ResolveCancellationRegistry>().finish(token);
    }
    result
}

fn ensure_preview_active(token: Option<&ResolveToken>) -> Result<(), String> {
    match token.and_then(ResolveToken::outcome) {
        Some(outcome) => Err(format!("preview request ended as {outcome:?}")),
        None => Ok(()),
    }
}

pub fn maybe_start_phase3_smoke(app: &AppHandle) {
    if std::env::var_os("GX_PHASE3_AUTO_SMOKE").is_none()
        || PHASE3_SMOKE_STARTED.swap(true, Ordering::AcqRel)
    {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let result = tauri::async_runtime::spawn_blocking(|| {
            let results = search_all("Hello Adele", 15).map_err(|error| error.to_string())?;
            let wanted = results
                .iter()
                .find(|track| {
                    track.provider_id == "deezer"
                        && track.title.eq_ignore_ascii_case("Hello")
                        && track.artist.to_ascii_lowercase().contains("adele")
                })
                .cloned()
                .ok_or_else(|| "Deezer search result was unavailable".to_owned())?;
            let selected = select_playable_with(&wanted, results, preview_is_available)
                .ok_or_else(|| "cross-platform replacement was unavailable".to_owned())?;
            let lyrics = fetch_lyrics(&wanted.title, &wanted.artist, wanted.duration_ms)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "lyrics service returned no document".to_owned())?;
            Ok::<_, String>((selected, lyrics))
        })
        .await
        .map_err(|error| format!("Phase 3 smoke task failed: {error}"))
        .and_then(|result| result);
        let ((selected, replaced_provider), lyrics) = match result {
            Ok(result) => result,
            Err(error) => {
                eprintln!("GX_PHASE3_FAILED {error}");
                app.exit(2);
                return;
            }
        };
        println!(
            "GX_PHASE3_REPLACEMENT_OK from={} to={}",
            replaced_provider.as_deref().unwrap_or("none"),
            selected.provider_id,
        );
        println!("GX_PHASE3_LYRICS_OK lines={}", lyrics.lines.len());
        let cached = match cache_preview(&app, &selected).await {
            Ok(path) => path,
            Err(error) => {
                eprintln!("GX_PHASE3_FAILED {error}");
                app.exit(2);
                return;
            }
        };
        let engine = app.state::<LocalAudioEngine>();
        let minimum_generation = crate::media_session::next_engine_generation(&engine);
        let location = cached.display().to_string();
        if let Err(error) = engine.load(vec![cached]) {
            eprintln!("GX_PHASE3_FAILED {error}");
            app.exit(2);
            return;
        }
        crate::media_session::set_online_metadata(
            &app,
            &selected,
            minimum_generation,
            Some(location),
        );
        let monitor_app = app.clone();
        let monitored = tauri::async_runtime::spawn_blocking(move || {
            for _ in 0..600 {
                let snapshot = monitor_app.state::<LocalAudioEngine>().snapshot();
                if snapshot.status == gx_contracts::PlaybackStatus::Playing
                    && snapshot.position_seconds > 0.2
                {
                    return Ok(snapshot);
                }
                if snapshot.status == gx_contracts::PlaybackStatus::Failed {
                    return Err(snapshot
                        .error
                        .unwrap_or_else(|| "preview playback failed".into()));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err("preview playback did not start within 60 seconds".to_owned())
        })
        .await;
        match monitored {
            Ok(Ok(snapshot)) => {
                println!(
                    "GX_PHASE3_SEARCH_PLAY_LYRICS_OK position={:.3} underruns={}",
                    snapshot.position_seconds, snapshot.underrun_callbacks
                );
                if std::env::var_os("GX_PHASE3_AUTO_EXIT").is_some() {
                    app.exit(0);
                }
            }
            Ok(Err(error)) => {
                eprintln!("GX_PHASE3_FAILED {error}");
                app.exit(2);
            }
            Err(error) => {
                eprintln!("GX_PHASE3_FAILED monitor task: {error}");
                app.exit(2);
            }
        }
    });
}

async fn cache_preview(
    app: &AppHandle,
    track: &CatalogTrack,
) -> Result<std::path::PathBuf, String> {
    let request = track
        .preview
        .clone()
        .ok_or_else(|| "selected track has no preview request".to_owned())?;
    let extension = match request.media_type {
        gx_contracts::MediaType::Mp3 => "mp3",
        gx_contracts::MediaType::Aac => "m4a",
        gx_contracts::MediaType::Flac => "flac",
        gx_contracts::MediaType::Ogg => "ogg",
        gx_contracts::MediaType::Wav => "wav",
        _ => "media",
    };
    let safe_id = track
        .provider_track_id
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(80)
        .collect::<String>();
    let root = crate::isolated_smoke_data_root()
        .map(|root| root.join("preview-cache"))
        .unwrap_or(
            app.path()
                .app_cache_dir()
                .map_err(|error| error.to_string())?
                .join("preview-cache"),
        );
    let destination = root.join(format!("{}-{safe_id}.{extension}", track.provider_id));
    if destination
        .metadata()
        .is_ok_and(|metadata| metadata.len() > 0)
    {
        return Ok(destination);
    }
    let destination_for_task = destination.clone();
    tauri::async_runtime::spawn_blocking(move || {
        std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        let bytes = fetch_preview_bytes(&request).map_err(|error| error.to_string())?;
        let temporary = destination_for_task.with_extension(format!("{extension}.tmp"));
        std::fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
        std::fs::rename(&temporary, &destination_for_task).map_err(|error| error.to_string())?;
        Ok::<_, String>(())
    })
    .await
    .map_err(|error| format!("preview cache task failed: {error}"))??;
    Ok(destination)
}
