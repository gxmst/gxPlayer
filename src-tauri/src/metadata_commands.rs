use gx_audio::engine::LocalAudioEngine;
use gx_metadata::{
    CatalogTrack, LyricDocument, MetadataClient, ReplacementMatch, SearchBatch, apple_chart,
    fetch_lyrics, fetch_preview_bytes, find_replacements, parse_lrc, preview_is_available, search_all,
    select_playable_with,
};
use gx_source::safe_http::RequestCancellation;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};

use crate::require_window;
use crate::source_commands::{ResolveCancellationRegistry, ResolveToken};

static PHASE3_SMOKE_STARTED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, Deserialize, Hash, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum MetadataLane {
    SearchSuggestions,
    SearchResults,
    SearchImport,
    Lyrics,
}

impl MetadataLane {
    fn accepts_search(self) -> bool {
        matches!(
            self,
            Self::SearchSuggestions | Self::SearchResults | Self::SearchImport
        )
    }
}

#[derive(Clone)]
struct MetadataToken {
    lane: MetadataLane,
    request_id: String,
    cancellation: Arc<RequestCancellation>,
}

impl MetadataToken {
    fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }
}

struct ActiveMetadataRequest {
    request_id: String,
    cancellation: Arc<RequestCancellation>,
}

#[derive(Default)]
struct MetadataCancellationState {
    active: HashMap<MetadataLane, ActiveMetadataRequest>,
    cancelled_before_begin: HashMap<MetadataLane, VecDeque<String>>,
}

#[derive(Default)]
pub struct MetadataCancellationRegistry {
    state: Mutex<MetadataCancellationState>,
}

impl MetadataCancellationRegistry {
    fn begin(&self, lane: MetadataLane, request_id: String) -> Result<MetadataToken, String> {
        let request_id = request_id.trim().to_owned();
        if !valid_request_id(&request_id) {
            return Err("metadata requestId must contain 1 to 160 characters".into());
        }
        let cancellation = Arc::new(RequestCancellation::new());
        let active = ActiveMetadataRequest {
            request_id: request_id.clone(),
            cancellation: Arc::clone(&cancellation),
        };
        let mut state = self.state.lock().unwrap();
        let cancelled_before_begin = state
            .cancelled_before_begin
            .get_mut(&lane)
            .and_then(|request_ids| {
                let position = request_ids
                    .iter()
                    .position(|pending| pending == &request_id)?;
                request_ids.remove(position)
            })
            .is_some();
        if state
            .cancelled_before_begin
            .get(&lane)
            .is_some_and(VecDeque::is_empty)
        {
            state.cancelled_before_begin.remove(&lane);
        }
        if let Some(previous) = state.active.insert(lane, active) {
            previous.cancellation.cancel();
        }
        if cancelled_before_begin {
            cancellation.cancel();
        }
        drop(state);
        Ok(MetadataToken {
            lane,
            request_id,
            cancellation,
        })
    }

    fn cancel(&self, lane: MetadataLane, request_id: &str) -> bool {
        const MAX_EARLY_CANCELLATIONS_PER_LANE: usize = 32;

        let request_id = request_id.trim();
        if !valid_request_id(request_id) {
            return false;
        }
        let mut state = self.state.lock().unwrap();
        let matches = state
            .active
            .get(&lane)
            .is_some_and(|request| request.request_id == request_id);
        if !matches {
            let pending = state.cancelled_before_begin.entry(lane).or_default();
            if !pending.iter().any(|candidate| candidate == request_id) {
                if pending.len() == MAX_EARLY_CANCELLATIONS_PER_LANE {
                    pending.pop_front();
                }
                pending.push_back(request_id.to_owned());
            }
            return false;
        }
        if let Some(request) = state.active.remove(&lane) {
            request.cancellation.cancel();
        }
        true
    }

    fn finish(&self, token: &MetadataToken) {
        let mut state = self.state.lock().unwrap();
        let owns_request = state.active.get(&token.lane).is_some_and(|request| {
            request.request_id == token.request_id
                && Arc::ptr_eq(&request.cancellation, &token.cancellation)
        });
        if owns_request {
            state.active.remove(&token.lane);
        }
    }
}

fn valid_request_id(request_id: &str) -> bool {
    !request_id.is_empty() && request_id.len() <= 160
}

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
    client: State<'_, MetadataClient>,
    registry: State<'_, MetadataCancellationRegistry>,
    query: String,
    limit: Option<usize>,
    request_id: String,
    lane: MetadataLane,
) -> Result<Vec<CatalogTrack>, String> {
    require_window(&window, "main")?;
    if !lane.accepts_search() {
        return Err("lyrics lane cannot run a metadata search".into());
    }
    let token = registry.begin(lane, request_id.clone())?;
    let event_window = window.clone();
    let event_token = token.clone();
    let result = client
        .search_all_progressive(
            &query,
            limit.unwrap_or(15),
            token.cancellation.as_ref(),
            move |batch: SearchBatch| {
                if event_token.is_cancelled() {
                    return;
                }
                let payload = CatalogSearchBatchEvent {
                    request_id: request_id.clone(),
                    provider_id: batch.provider_id,
                    tracks: batch.tracks,
                    error: batch.error,
                };
                if let Err(error) = event_window.emit("gx-catalog-search-batch", payload) {
                    eprintln!("catalog search batch emit failed: {error}");
                }
            },
        )
        .await;
    registry.finish(&token);
    result.map_err(|error| error.to_string())
}

#[tauri::command]
pub fn metadata_cancel_request(
    window: WebviewWindow,
    registry: State<'_, MetadataCancellationRegistry>,
    lane: MetadataLane,
    request_id: String,
) -> Result<bool, String> {
    require_window(&window, "main")?;
    Ok(registry.cancel(lane, &request_id))
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
    client: State<'_, MetadataClient>,
    registry: State<'_, MetadataCancellationRegistry>,
    title: String,
    artist: String,
    duration_ms: Option<u64>,
    request_id: String,
) -> Result<Option<LyricDocument>, String> {
    require_window(&window, "main")?;
    let token = registry.begin(MetadataLane::Lyrics, request_id)?;
    let result = client
        .fetch_lyrics(&title, &artist, duration_ms, token.cancellation.as_ref())
        .await;
    registry.finish(&token);
    result.map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn metadata_read_local_lyrics(
    window: WebviewWindow,
    path: String,
) -> Result<LyricDocument, String> {
    require_window(&window, "main")?;
    let path = PathBuf::from(path);
    if !path.extension().and_then(|value| value.to_str()).is_some_and(|value| value.eq_ignore_ascii_case("lrc")) {
        return Err("请选择 .lrc 歌词文件".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let metadata = std::fs::metadata(&path).map_err(|error| error.to_string())?;
        if metadata.len() > 2 * 1024 * 1024 {
            return Err("LRC 文件不能超过 2 MiB".into());
        }
        let text = std::fs::read_to_string(&path).map_err(|error| error.to_string())?;
        Ok(parse_lrc(&text))
    })
    .await
    .map_err(|error| format!("读取本地歌词任务失败: {error}"))?
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
    intent_generation: Option<u64>,
) -> Result<SelectedPlayback, String> {
    require_window(&window, "main")?;
    let app = window.app_handle().clone();
    let token = request_id
        .map(|request_id| {
            app.state::<ResolveCancellationRegistry>()
                .begin(request_id, intent_generation)
        })
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
    if let Some(cached) = app
        .state::<crate::preview_cache::PreviewCacheStore>()
        .lookup(&track.provider_id, &track.provider_track_id)?
    {
        return Ok(cached);
    }
    let app_for_task = app.clone();
    let provider_id = track.provider_id.clone();
    let provider_track_id = track.provider_track_id.clone();
    let destination = tauri::async_runtime::spawn_blocking(move || {
        let bytes = fetch_preview_bytes(&request).map_err(|error| error.to_string())?;
        app_for_task
            .state::<crate::preview_cache::PreviewCacheStore>()
            .insert(&provider_id, &provider_track_id, extension, &bytes)
    })
    .await
    .map_err(|error| format!("preview cache task failed: {error}"))??;
    let _ = app.emit(
        "gx-preview-cache-changed",
        app.state::<crate::preview_cache::PreviewCacheStore>()
            .status(),
    );
    Ok(destination)
}

#[cfg(test)]
mod tests {
    use super::{MetadataCancellationRegistry, MetadataLane};

    #[test]
    fn newer_request_cancels_the_previous_request_in_the_same_lane() {
        let registry = MetadataCancellationRegistry::default();
        let first = registry
            .begin(MetadataLane::SearchResults, "first".into())
            .unwrap();
        let second = registry
            .begin(MetadataLane::SearchResults, "second".into())
            .unwrap();

        assert!(first.is_cancelled());
        assert!(!second.is_cancelled());
    }

    #[test]
    fn requests_in_different_lanes_do_not_cancel_each_other() {
        let registry = MetadataCancellationRegistry::default();
        let suggestions = registry
            .begin(MetadataLane::SearchSuggestions, "suggestions".into())
            .unwrap();
        let lyrics = registry
            .begin(MetadataLane::Lyrics, "lyrics".into())
            .unwrap();

        assert!(!suggestions.is_cancelled());
        assert!(!lyrics.is_cancelled());
    }

    #[test]
    fn finishing_an_old_token_does_not_remove_the_new_owner() {
        let registry = MetadataCancellationRegistry::default();
        let old = registry
            .begin(MetadataLane::SearchImport, "shared-id".into())
            .unwrap();
        let current = registry
            .begin(MetadataLane::SearchImport, "shared-id".into())
            .unwrap();

        registry.finish(&old);

        assert!(registry.cancel(MetadataLane::SearchImport, "shared-id"));
        assert!(current.is_cancelled());
    }

    #[test]
    fn a_mismatched_request_id_cannot_cancel_the_current_owner() {
        let registry = MetadataCancellationRegistry::default();
        let current = registry
            .begin(MetadataLane::SearchResults, "current".into())
            .unwrap();

        assert!(!registry.cancel(MetadataLane::SearchResults, "stale"));
        assert!(!current.is_cancelled());
        assert!(registry.cancel(MetadataLane::SearchResults, "current"));
    }

    #[test]
    fn cancellation_that_arrives_before_begin_is_not_lost() {
        let registry = MetadataCancellationRegistry::default();

        assert!(!registry.cancel(MetadataLane::SearchSuggestions, "late-begin"));
        let request = registry
            .begin(MetadataLane::SearchSuggestions, "late-begin".into())
            .unwrap();

        assert!(request.is_cancelled());
    }

    #[test]
    fn invalid_request_ids_are_rejected_without_replacing_the_current_owner() {
        let registry = MetadataCancellationRegistry::default();
        let current = registry
            .begin(MetadataLane::Lyrics, "current".into())
            .unwrap();

        assert!(registry.begin(MetadataLane::Lyrics, "   ".into()).is_err());
        assert!(
            registry
                .begin(MetadataLane::Lyrics, "x".repeat(161))
                .is_err()
        );
        assert!(!current.is_cancelled());
        assert!(registry.cancel(MetadataLane::Lyrics, "current"));
    }
}
