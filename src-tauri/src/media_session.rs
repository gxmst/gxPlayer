//! Windows SMTC / media-key bridge via souvlaki.
//!
//! Shows the OS taskbar media flyout (title + play/pause/prev/next) while GXPlayer is playing.
//! Play/pause hit the engine; next/previous are emitted to the frontend so playlist authority
//! (including online lazy resolve) stays consistent.

use std::thread;
use std::time::Duration;

use gx_audio::engine::LocalAudioEngine;
use gx_contracts::PlaybackStatus;
use souvlaki::{MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, PlatformConfig};
use tauri::{AppHandle, Emitter, Manager};

pub fn spawn_media_session(app: AppHandle) {
    thread::Builder::new()
        .name("gx-media-session".into())
        .spawn(move || {
            // Retry a few times — HWND may not be ready at first paint.
            for attempt in 0..8 {
                thread::sleep(Duration::from_millis(if attempt == 0 { 600 } else { 500 }));
                match run_media_session(app.clone()) {
                    Ok(()) => return,
                    Err(error) => {
                        eprintln!(
                            "media session attempt {} failed: {error}",
                            attempt + 1
                        );
                    }
                }
            }
            eprintln!("media session unavailable after retries");
        })
        .ok();
}

fn run_media_session(app: AppHandle) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let hwnd = {
        let window = app
            .get_webview_window("main")
            .ok_or_else(|| "main window missing for SMTC".to_owned())?;
        let hwnd = window.hwnd().map_err(|e| e.to_string())?;
        Some(hwnd.0)
    };
    #[cfg(not(target_os = "windows"))]
    let hwnd = None;

    let config = PlatformConfig {
        dbus_name: "com.gxplayer.desktop",
        display_name: "GXPlayer",
        hwnd,
    };
    let mut controls = MediaControls::new(config).map_err(|e| format!("{e:?}"))?;
    let app_events = app.clone();

    controls
        .attach(move |event| {
            let Some(engine) = app_events.try_state::<LocalAudioEngine>() else {
                return;
            };
            let result = match event {
                MediaControlEvent::Toggle => {
                    let status = engine.snapshot().status;
                    if matches!(status, PlaybackStatus::Playing | PlaybackStatus::Loading) {
                        engine.pause()
                    } else {
                        engine.play()
                    }
                }
                MediaControlEvent::Play => engine.play(),
                MediaControlEvent::Pause | MediaControlEvent::Stop => engine.pause(),
                // Frontend owns playlist order (online lazy resolve + local jump).
                MediaControlEvent::Next => {
                    let _ = app_events.emit("gx-media", "next");
                    Ok(())
                }
                MediaControlEvent::Previous => {
                    let _ = app_events.emit("gx-media", "previous");
                    Ok(())
                }
                _ => Ok(()),
            };
            if let Err(error) = result {
                eprintln!("media session command failed: {error}");
            }
        })
        .map_err(|e| format!("{e:?}"))?;

    let mut last_title = String::new();
    let mut last_generation = u64::MAX;
    let mut last_playing: Option<bool> = None;
    loop {
        thread::sleep(Duration::from_millis(350));
        let Some(engine) = app.try_state::<LocalAudioEngine>() else {
            continue;
        };
        let snap = engine.snapshot();
        let item = snap.queue_index.and_then(|index| snap.queue.get(index));
        let title = item
            .map(|item| item.title.clone())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "GXPlayer".into());
        // QueueItem has no artist field — surface online/local location hint lightly.
        let artist = item
            .map(|item| {
                if item.online {
                    "在线播放".to_owned()
                } else {
                    "本地播放".to_owned()
                }
            })
            .unwrap_or_else(|| "GXPlayer".into());
        let playing = matches!(
            snap.status,
            PlaybackStatus::Playing | PlaybackStatus::Loading
        );
        let active = item.is_some()
            && !matches!(
                snap.status,
                PlaybackStatus::Idle | PlaybackStatus::Stopped
            );

        if title != last_title || snap.generation != last_generation {
            let _ = controls.set_metadata(MediaMetadata {
                title: Some(title.as_str()),
                artist: Some(artist.as_str()),
                album: Some("GXPlayer"),
                cover_url: None,
                duration: None,
            });
            last_title = title;
            last_generation = snap.generation;
        }

        let playback = if !active {
            MediaPlayback::Stopped
        } else if playing {
            MediaPlayback::Playing { progress: None }
        } else {
            MediaPlayback::Paused { progress: None }
        };
        let playing_flag = matches!(playback, MediaPlayback::Playing { .. });
        if last_playing != Some(playing_flag) || (!active && last_playing.is_some()) {
            let _ = controls.set_playback(playback);
            last_playing = if active { Some(playing_flag) } else { Some(false) };
        }
    }
}
