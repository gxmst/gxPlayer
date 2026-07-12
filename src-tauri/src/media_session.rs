//! Windows SMTC / media-key bridge via souvlaki.
//!
//! Polls the engine snapshot and pushes metadata + play state to the OS media session.
//! Media key events call into LocalAudioEngine (play/pause/next/previous).

use std::thread;
use std::time::Duration;

use gx_audio::engine::LocalAudioEngine;
use gx_contracts::PlaybackStatus;
use souvlaki::{MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, PlatformConfig};
use tauri::{AppHandle, Manager};

pub fn spawn_media_session(app: AppHandle) {
    thread::Builder::new()
        .name("gx-media-session".into())
        .spawn(move || {
            if let Err(error) = run_media_session(app) {
                eprintln!("media session unavailable: {error}");
            }
        })
        .ok();
}

fn run_media_session(app: AppHandle) -> Result<(), String> {
    // Give the main window a moment to appear so HWND is valid.
    thread::sleep(Duration::from_millis(800));

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
                MediaControlEvent::Pause => engine.pause(),
                MediaControlEvent::Next => engine.next(),
                MediaControlEvent::Previous => engine.previous(),
                MediaControlEvent::Stop => engine.pause(),
                _ => Ok(()),
            };
            if let Err(error) = result {
                eprintln!("media session command failed: {error}");
            }
        })
        .map_err(|e| format!("{e:?}"))?;

    let mut last_title = String::new();
    let mut last_playing: Option<bool> = None;
    loop {
        thread::sleep(Duration::from_millis(400));
        let Some(engine) = app.try_state::<LocalAudioEngine>() else {
            continue;
        };
        let snap = engine.snapshot();
        let title = snap
            .queue_index
            .and_then(|index| snap.queue.get(index))
            .map(|item| item.title.clone())
            .unwrap_or_else(|| "GXPlayer".into());
        let playing = matches!(
            snap.status,
            PlaybackStatus::Playing | PlaybackStatus::Loading
        );
        if title != last_title {
            let _ = controls.set_metadata(MediaMetadata {
                title: Some(title.as_str()),
                artist: Some("GXPlayer"),
                album: None,
                cover_url: None,
                duration: None,
            });
            last_title = title;
        }
        if last_playing != Some(playing) {
            let _ = controls.set_playback(if playing {
                MediaPlayback::Playing { progress: None }
            } else {
                MediaPlayback::Paused { progress: None }
            });
            last_playing = Some(playing);
        }
    }
}
