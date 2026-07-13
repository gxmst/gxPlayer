//! Windows System Media Transport Controls (SMTC) and playback snapshot events.
//!
//! Windows exposes this session in its media flyout/quick settings and on media keys. The
//! session remains attached to the main HWND while the WebView is hidden to the tray.

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use gx_audio::engine::{EngineSnapshot, LocalAudioEngine, QueueItem};
use gx_contracts::PlaybackStatus;
use gx_library::LibraryStore;
use gx_metadata::CatalogTrack;
use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
    SeekDirection,
};
use tauri::{AppHandle, Emitter, Manager};

use crate::transport::{TransportAction, dispatch};

#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage, WM_QUIT,
};

const SNAPSHOT_INTERVAL: Duration = Duration::from_millis(250);
const WINDOWS_MESSAGE_PUMP_INTERVAL: Duration = Duration::from_millis(25);
const DEFAULT_SEEK_SECONDS: f64 = 10.0;

#[derive(Debug, Clone, Default)]
struct PlaybackMetadata {
    revision: u64,
    minimum_generation: u64,
    location_hint: Option<String>,
    online_only: bool,
    title: String,
    artist: String,
    album: String,
    duration_seconds: Option<f64>,
    cover_url: Option<String>,
}

#[derive(Debug, Default)]
struct MediaSessionInner {
    next_revision: u64,
    override_metadata: Option<PlaybackMetadata>,
}

/// Metadata supplied by online/preview/cache commands. Local-library metadata is read directly
/// from `LibraryStore` using the engine queue path.
#[derive(Debug, Default)]
pub struct MediaSessionState {
    inner: Mutex<MediaSessionInner>,
}

impl MediaSessionState {
    fn set_override(&self, mut metadata: PlaybackMetadata) -> u64 {
        let mut inner = self.inner.lock().unwrap();
        inner.next_revision = inner.next_revision.wrapping_add(1).max(1);
        metadata.revision = inner.next_revision;
        let revision = metadata.revision;
        inner.override_metadata = Some(metadata);
        revision
    }

    fn set_cover_if_revision(&self, revision: u64, cover_url: String) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if inner
            .override_metadata
            .as_ref()
            .map(|metadata| metadata.revision)
            != Some(revision)
        {
            return false;
        }
        inner.next_revision = inner.next_revision.wrapping_add(1).max(1);
        let next_revision = inner.next_revision;
        let metadata = inner.override_metadata.as_mut().unwrap();
        metadata.revision = next_revision;
        metadata.cover_url = Some(cover_url);
        true
    }

    fn matching_override(
        &self,
        snapshot: &EngineSnapshot,
        item: &QueueItem,
    ) -> Option<PlaybackMetadata> {
        let metadata = self.inner.lock().unwrap().override_metadata.clone()?;
        if snapshot.generation < metadata.minimum_generation {
            return None;
        }
        let matches = metadata.location_hint.as_deref().map_or_else(
            || !metadata.online_only || item.online,
            |location| location == item.location,
        );
        matches.then_some(metadata)
    }
}

/// Capture this before submitting a load command, then pass it to `set_*_metadata`.
pub fn next_engine_generation(engine: &LocalAudioEngine) -> u64 {
    engine.snapshot().generation.wrapping_add(1)
}

pub fn set_online_metadata(
    app: &AppHandle,
    track: &CatalogTrack,
    minimum_generation: u64,
    location_hint: Option<String>,
) {
    let artwork_url = track.artwork_url.as_ref().map(ToString::to_string);
    let revision = app
        .state::<MediaSessionState>()
        .set_override(PlaybackMetadata {
            minimum_generation,
            location_hint,
            online_only: true,
            title: track.title.clone(),
            artist: track.artist.clone(),
            album: track.album.clone(),
            duration_seconds: track.duration_ms.map(|value| value as f64 / 1000.0),
            cover_url: None,
            ..PlaybackMetadata::default()
        });
    fetch_online_cover(app, artwork_url, revision);
}

pub fn set_cached_metadata(
    app: &AppHandle,
    title: String,
    artist: String,
    album: String,
    cover_url: Option<String>,
    minimum_generation: u64,
    location: String,
) {
    let revision = app
        .state::<MediaSessionState>()
        .set_override(PlaybackMetadata {
            minimum_generation,
            location_hint: Some(location),
            online_only: false,
            title,
            artist,
            album,
            duration_seconds: None,
            cover_url: None,
            ..PlaybackMetadata::default()
        });
    fetch_online_cover(app, cover_url, revision);
}

fn fetch_online_cover(app: &AppHandle, artwork_url: Option<String>, revision: u64) {
    let Some(artwork_url) = artwork_url.filter(|url| is_remote_url(url)) else {
        return;
    };
    let app = app.clone();
    thread::Builder::new()
        .name("gx-artwork".into())
        .spawn(move || {
            let result = app
                .state::<crate::artwork::ArtworkCache>()
                .ensure(&artwork_url);
            let Ok(asset) = result else {
                return;
            };
            let cover_url = file_url(&asset.path);
            app.state::<MediaSessionState>()
                .set_cover_if_revision(revision, cover_url);
        })
        .ok();
}

fn is_remote_url(url: &str) -> bool {
    reqwest::Url::parse(url).is_ok_and(|url| url.scheme() == "http" || url.scheme() == "https")
}

pub fn spawn_media_session(app: AppHandle) {
    thread::Builder::new()
        .name("gx-media-session".into())
        .spawn(move || {
            for attempt in 1..=12 {
                thread::sleep(Duration::from_millis(if attempt == 1 { 500 } else { 700 }));
                match run_media_session(app.clone()) {
                    Ok(()) => return,
                    Err(error) => eprintln!("GX_SMTC attempt {attempt} failed: {error}"),
                }
            }
            eprintln!("GX_SMTC unavailable after retries (Windows media controls disabled)");
        })
        .ok();
}

pub(crate) fn main_hwnd(app: &AppHandle) -> Result<*mut std::ffi::c_void, String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "main window missing".to_owned())?;
    #[cfg(windows)]
    {
        let hwnd = window.hwnd().map_err(|error| error.to_string())?;
        let raw = hwnd.0;
        if raw.is_null() {
            return Err("HWND is null".into());
        }
        Ok(raw)
    }
    #[cfg(not(windows))]
    {
        let _ = window;
        Err("SMTC is Windows-only".into())
    }
}

#[cfg(windows)]
struct WinRtApartment;

#[cfg(windows)]
impl WinRtApartment {
    fn initialize() -> Result<Self, String> {
        unsafe {
            windows::Win32::System::WinRT::RoInitialize(
                windows::Win32::System::WinRT::RO_INIT_MULTITHREADED,
            )
            .map_err(|error| format!("RoInitialize: {error}"))?;
        }
        Ok(Self)
    }
}

#[cfg(windows)]
impl Drop for WinRtApartment {
    fn drop(&mut self) {
        unsafe { windows::Win32::System::WinRT::RoUninitialize() };
    }
}

fn run_media_session(app: AppHandle) -> Result<(), String> {
    #[cfg(windows)]
    let _apartment = WinRtApartment::initialize()?;

    let hwnd = main_hwnd(&app)?;
    let config = PlatformConfig {
        dbus_name: "com.gxplayer.desktop",
        display_name: "GXPlayer",
        hwnd: Some(hwnd),
    };
    let mut controls =
        MediaControls::new(config).map_err(|error| format!("MediaControls::new: {error:?}"))?;
    let app_events = app.clone();
    controls
        .attach(move |event| handle_media_control_event(&app_events, event))
        .map_err(|error| format!("MediaControls::attach: {error:?}"))?;

    let default_cover = default_cover_url(&app);
    set_metadata_best_effort(
        &mut controls,
        MediaMetadata {
            title: Some("GXPlayer"),
            artist: Some("就绪"),
            album: Some("GXPlayer"),
            cover_url: default_cover.as_deref(),
            duration: None,
        },
        "initial SMTC metadata",
    )?;
    controls
        .set_playback(MediaPlayback::Stopped)
        .map_err(|error| format!("initial SMTC playback: {error:?}"))?;
    println!("GX_SMTC attached hwnd={hwnd:?}");

    let mut last_metadata_key = String::new();
    let mut last_status = None;
    let mut last_position = f64::NEG_INFINITY;
    let mut next_snapshot = Instant::now();
    loop {
        #[cfg(windows)]
        if pump_windows_messages() {
            eprintln!("GX_SMTC received WM_QUIT; stopping media session");
            return Ok(());
        }

        if Instant::now() >= next_snapshot {
            next_snapshot = Instant::now() + SNAPSHOT_INTERVAL;
            let Some(engine) = app.try_state::<LocalAudioEngine>() else {
                eprintln!("GX_SMTC snapshot skipped: audio engine unavailable");
                thread::sleep(WINDOWS_MESSAGE_PUMP_INTERVAL);
                continue;
            };
            let snapshot = engine.snapshot();
            if let Err(error) = app.emit("gx-player-snapshot", &snapshot) {
                eprintln!("GX_SMTC snapshot event failed: {error}");
            }

            let item = snapshot
                .queue_index
                .and_then(|index| snapshot.queue.get(index));
            let override_metadata = item.and_then(|item| {
                app.state::<MediaSessionState>()
                    .matching_override(&snapshot, item)
            });
            let metadata_key = metadata_key(&snapshot, item, override_metadata.as_ref());
            if metadata_key != last_metadata_key {
                let metadata =
                    resolve_metadata(&app, &snapshot, item, override_metadata, &default_cover);
                let duration = metadata
                    .duration_seconds
                    .or(snapshot.duration_seconds)
                    .filter(|value| value.is_finite() && *value > 0.0)
                    .map(Duration::from_secs_f64);
                if let Err(error) = set_metadata_best_effort(
                    &mut controls,
                    MediaMetadata {
                        title: Some(&metadata.title),
                        artist: Some(&metadata.artist),
                        album: Some(&metadata.album),
                        cover_url: metadata.cover_url.as_deref().or(default_cover.as_deref()),
                        duration,
                    },
                    "SMTC set_metadata",
                ) {
                    eprintln!("GX_SMTC set_metadata text failed: {error}");
                }
                if let Some(window) = app.get_webview_window("main") {
                    let title = window_title(&metadata.title, &metadata.artist);
                    if let Err(error) = window.set_title(&title) {
                        eprintln!("GX_SMTC window title update failed: {error}");
                    }
                }
                last_metadata_key = metadata_key;
            }

            let status = playback_label(snapshot.status);
            let position = if snapshot.position_seconds.is_finite() {
                snapshot.position_seconds.max(0.0)
            } else {
                0.0
            };
            if last_status != Some(status)
                || !last_position.is_finite()
                || (position - last_position).abs() >= 0.2
            {
                let progress = Some(MediaPosition(Duration::from_secs_f64(position)));
                let playback = match status {
                    "playing" => MediaPlayback::Playing { progress },
                    "paused" => MediaPlayback::Paused { progress },
                    _ => MediaPlayback::Stopped,
                };
                if let Err(error) = controls.set_playback(playback) {
                    eprintln!("GX_SMTC set_playback: {error:?}");
                }
                last_status = Some(status);
                last_position = position;
            }
        }
        thread::sleep(WINDOWS_MESSAGE_PUMP_INTERVAL);
    }
}

fn set_metadata_best_effort(
    controls: &mut MediaControls,
    metadata: MediaMetadata<'_>,
    context: &str,
) -> Result<(), String> {
    let text_metadata = MediaMetadata {
        cover_url: None,
        ..metadata.clone()
    };
    controls
        .set_metadata(text_metadata)
        .map_err(|error| format!("{context} text: {error:?}"))?;
    if metadata.cover_url.is_some_and(|url| !is_remote_url(url))
        && let Err(error) = controls.set_metadata(metadata)
    {
        eprintln!("GX_SMTC {context} cover skipped: {error:?}");
    }
    Ok(())
}

#[cfg(windows)]
fn pump_windows_messages() -> bool {
    unsafe {
        let mut message = MSG::default();
        while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
            if message.message == WM_QUIT {
                return true;
            }
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    false
}

fn handle_media_control_event(app: &AppHandle, event: MediaControlEvent) {
    eprintln!("GX_SMTC event received: {event:?}");
    let Some(engine) = app.try_state::<LocalAudioEngine>() else {
        eprintln!("GX_SMTC event dropped: audio engine unavailable");
        return;
    };
    let result: Result<(), String> = match event {
        MediaControlEvent::Toggle => dispatch(app, TransportAction::Toggle),
        MediaControlEvent::Play => dispatch(app, TransportAction::Play),
        MediaControlEvent::Pause | MediaControlEvent::Stop => dispatch(app, TransportAction::Pause),
        MediaControlEvent::Next => dispatch(app, TransportAction::Next),
        MediaControlEvent::Previous => dispatch(app, TransportAction::Previous),
        MediaControlEvent::SetPosition(MediaPosition(position)) => engine
            .seek(position.as_secs_f64())
            .map_err(|error| error.to_string()),
        MediaControlEvent::SeekBy(direction, amount) => {
            seek_relative(&engine, direction, amount.as_secs_f64())
        }
        MediaControlEvent::Seek(direction) => {
            seek_relative(&engine, direction, DEFAULT_SEEK_SECONDS)
        }
        MediaControlEvent::Raise => {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
            Ok(())
        }
        _ => Ok(()),
    };
    if let Err(error) = result {
        eprintln!("GX_SMTC command failed: {error}");
    }
}

fn seek_relative(
    engine: &LocalAudioEngine,
    direction: SeekDirection,
    amount_seconds: f64,
) -> Result<(), String> {
    let position = engine.snapshot().position_seconds;
    let target = match direction {
        SeekDirection::Forward => position + amount_seconds,
        SeekDirection::Backward => (position - amount_seconds).max(0.0),
    };
    engine.seek(target).map_err(|error| error.to_string())
}

fn playback_label(status: PlaybackStatus) -> &'static str {
    match status {
        PlaybackStatus::Playing | PlaybackStatus::Loading => "playing",
        PlaybackStatus::Paused | PlaybackStatus::Buffering => "paused",
        _ => "stopped",
    }
}

fn window_title(title: &str, artist: &str) -> String {
    let clean = |value: &str| value.replace(['\r', '\n'], " ").trim().to_owned();
    let title = clean(title);
    let artist = clean(artist);
    if title.is_empty() || title == "GXPlayer" {
        "GXPlayer".into()
    } else if artist.is_empty() {
        format!("{title} · GXPlayer")
    } else {
        format!("{title} — {artist} · GXPlayer")
    }
}

fn metadata_key(
    snapshot: &EngineSnapshot,
    item: Option<&QueueItem>,
    metadata: Option<&PlaybackMetadata>,
) -> String {
    format!(
        "{}:{}:{}:{}",
        snapshot.queue_index.map_or(usize::MAX, |value| value),
        item.map_or("", |item| item.location.as_str()),
        metadata.map_or(0, |metadata| metadata.revision),
        snapshot
            .duration_seconds
            .filter(|value| value.is_finite())
            .map_or(0, |value| (value * 10.0) as u64)
    )
}

fn resolve_metadata(
    app: &AppHandle,
    snapshot: &EngineSnapshot,
    item: Option<&QueueItem>,
    override_metadata: Option<PlaybackMetadata>,
    default_cover: &Option<String>,
) -> PlaybackMetadata {
    if let Some(mut metadata) = override_metadata {
        metadata.duration_seconds = metadata.duration_seconds.or(snapshot.duration_seconds);
        if metadata.cover_url.is_none() {
            metadata.cover_url = default_cover.clone();
        }
        return metadata;
    }
    let Some(item) = item else {
        return PlaybackMetadata {
            title: "GXPlayer".into(),
            artist: "就绪".into(),
            album: "GXPlayer".into(),
            cover_url: default_cover.clone(),
            ..PlaybackMetadata::default()
        };
    };
    if !item.online {
        let library_track = app
            .try_state::<LibraryStore>()
            .and_then(|library| library.track_by_path(&item.location).ok().flatten());
        let probed = if library_track.is_none() {
            gx_audio::probe_local_file(Path::new(&item.location)).ok()
        } else {
            None
        };
        let title = library_track
            .as_ref()
            .map(|track| track.title.clone())
            .or_else(|| probed.as_ref().and_then(|info| info.title.clone()))
            .unwrap_or_else(|| item.title.clone());
        let artist = library_track
            .as_ref()
            .map(|track| track.artist.clone())
            .or_else(|| probed.as_ref().and_then(|info| info.artist.clone()))
            .unwrap_or_default();
        let album = library_track
            .as_ref()
            .map(|track| track.album.clone())
            .or_else(|| probed.as_ref().and_then(|info| info.album.clone()))
            .unwrap_or_default();
        let duration_seconds = library_track
            .as_ref()
            .and_then(|track| track.duration_seconds)
            .or_else(|| probed.as_ref().and_then(|info| info.duration_seconds))
            .or(snapshot.duration_seconds);
        let cover_url =
            local_cover_url(app, Path::new(&item.location)).or_else(|| default_cover.clone());
        return PlaybackMetadata {
            title,
            artist,
            album,
            duration_seconds,
            cover_url,
            ..PlaybackMetadata::default()
        };
    }
    PlaybackMetadata {
        title: item.title.clone(),
        artist: String::new(),
        album: String::new(),
        duration_seconds: snapshot.duration_seconds,
        cover_url: default_cover.clone(),
        ..PlaybackMetadata::default()
    }
}

fn local_cover_url(app: &AppHandle, media_path: &Path) -> Option<String> {
    let cover = gx_audio::extract_embedded_cover(media_path)
        .ok()
        .flatten()?;
    let mut hasher = DefaultHasher::new();
    media_path.hash(&mut hasher);
    media_path
        .metadata()
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .hash(&mut hasher);
    let extension = if cover.mime.eq_ignore_ascii_case("image/png") {
        "png"
    } else {
        "jpg"
    };
    let directory = app.path().app_cache_dir().ok()?.join("media-session");
    fs::create_dir_all(&directory).ok()?;
    let path = directory.join(format!("local-{:016x}.{extension}", hasher.finish()));
    if !path.exists() {
        fs::write(&path, cover.data).ok()?;
    }
    Some(file_url(&path))
}

fn default_cover_url(app: &AppHandle) -> Option<String> {
    let directory = app.path().app_cache_dir().ok()?.join("media-session");
    fs::create_dir_all(&directory).ok()?;
    let path = directory.join("gxplayer-default.png");
    if !path.exists() {
        fs::write(&path, include_bytes!("../icons/128x128.png")).ok()?;
    }
    Some(file_url(&path))
}

fn file_url(path: &Path) -> String {
    let absolute = path.canonicalize().unwrap_or_else(|_| PathBuf::from(path));
    #[cfg(windows)]
    {
        // souvlaki 0.8 strips this prefix and passes the remainder to StorageFile,
        // which requires a native absolute Windows path with backslashes.
        let path = absolute.to_string_lossy();
        let native = if let Some(path) = path.strip_prefix(r"\\?\UNC\") {
            format!(r"\\{path}")
        } else if let Some(path) = path.strip_prefix(r"\\?\") {
            path.to_owned()
        } else {
            path.into_owned()
        };
        format!("file://{native}")
    }
    #[cfg(not(windows))]
    {
        url::Url::from_file_path(&absolute)
            .map(|url| url.to_string())
            .unwrap_or_else(|_| format!("file://{}", absolute.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playback_status_mapping_is_stable() {
        assert_eq!(playback_label(PlaybackStatus::Playing), "playing");
        assert_eq!(playback_label(PlaybackStatus::Buffering), "paused");
        assert_eq!(playback_label(PlaybackStatus::Failed), "stopped");
    }

    #[test]
    fn generation_guard_rejects_old_override() {
        let state = MediaSessionState::default();
        state.set_override(PlaybackMetadata {
            minimum_generation: 5,
            online_only: true,
            title: "new".into(),
            ..PlaybackMetadata::default()
        });
        let mut snapshot = EngineSnapshot {
            generation: 4,
            ..EngineSnapshot::default()
        };
        let item = QueueItem {
            location: "online".into(),
            title: "old".into(),
            duration_seconds: None,
            online: true,
        };
        assert!(state.matching_override(&snapshot, &item).is_none());
        snapshot.generation = 5;
        assert_eq!(
            state.matching_override(&snapshot, &item).unwrap().title,
            "new"
        );
    }

    #[test]
    fn stale_artwork_cannot_replace_newer_metadata() {
        let state = MediaSessionState::default();
        let older = state.set_override(PlaybackMetadata {
            title: "older".into(),
            ..PlaybackMetadata::default()
        });
        let latest = state.set_override(PlaybackMetadata {
            title: "latest".into(),
            ..PlaybackMetadata::default()
        });

        assert!(!state.set_cover_if_revision(older, "file://older.jpg".into()));
        assert!(state.set_cover_if_revision(latest, "file://latest.jpg".into()));
        let metadata = state
            .inner
            .lock()
            .unwrap()
            .override_metadata
            .clone()
            .unwrap();
        assert_eq!(metadata.title, "latest");
        assert_eq!(metadata.cover_url.as_deref(), Some("file://latest.jpg"));
    }

    #[test]
    fn taskbar_window_title_contains_real_track_metadata() {
        assert_eq!(window_title("Song", "Artist"), "Song — Artist · GXPlayer");
        assert_eq!(window_title("Song", ""), "Song · GXPlayer");
        assert_eq!(window_title("GXPlayer", "就绪"), "GXPlayer");
    }

    #[cfg(windows)]
    #[test]
    fn souvlaki_file_url_preserves_a_native_windows_path() {
        assert_eq!(
            file_url(Path::new(r"C:\GXPlayer\cover.png")),
            r"file://C:\GXPlayer\cover.png"
        );
        assert_eq!(
            file_url(Path::new(r"\\?\C:\GXPlayer\cover.png")),
            r"file://C:\GXPlayer\cover.png"
        );
        assert_eq!(
            file_url(Path::new(r"\\?\UNC\server\share\cover.png")),
            r"file://\\server\share\cover.png"
        );
    }
}
