//! Windows System Media Transport Controls (taskbar media flyout).
//!
//! Uses souvlaki + the main window HWND. Next/previous are emitted to the
//! frontend so playlist ownership stays consistent.

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
            // Wait for the main window to be shown; HWND is required on Windows.
            for attempt in 1..=12 {
                thread::sleep(Duration::from_millis(if attempt == 1 { 900 } else { 700 }));
                match run_media_session(app.clone()) {
                    Ok(()) => return,
                    Err(error) => {
                        eprintln!("GX_SMTC attempt {attempt} failed: {error}");
                    }
                }
            }
            eprintln!("GX_SMTC unavailable after retries (taskbar media controls disabled)");
        })
        .ok();
}

fn main_hwnd(app: &AppHandle) -> Result<*mut std::ffi::c_void, String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "main window missing".to_owned())?;
    if !window.is_visible().unwrap_or(false) {
        return Err("main window not visible yet".into());
    }
    #[cfg(windows)]
    {
        let hwnd = window.hwnd().map_err(|e| e.to_string())?;
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

fn run_media_session(app: AppHandle) -> Result<(), String> {
    let hwnd = main_hwnd(&app)?;
    println!("GX_SMTC hwnd={hwnd:?}");

    let config = PlatformConfig {
        dbus_name: "com.gxplayer.desktop",
        display_name: "GXPlayer",
        hwnd: Some(hwnd),
    };
    let mut controls = MediaControls::new(config).map_err(|e| format!("MediaControls::new: {e:?}"))?;
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
                eprintln!("GX_SMTC command failed: {error}");
            }
        })
        .map_err(|e| format!("MediaControls::attach: {e:?}"))?;

    // Seed metadata immediately so Windows registers the session.
    let _ = controls.set_metadata(MediaMetadata {
        title: Some("GXPlayer"),
        artist: Some("就绪"),
        album: Some("GXPlayer"),
        cover_url: None,
        duration: None,
    });
    let _ = controls.set_playback(MediaPlayback::Stopped);
    println!("GX_SMTC attached");

    let mut last_title = String::new();
    let mut last_generation = u64::MAX;
    let mut last_status: Option<&'static str> = None;

    loop {
        thread::sleep(Duration::from_millis(300));
        let Some(engine) = app.try_state::<LocalAudioEngine>() else {
            continue;
        };
        let snap = engine.snapshot();
        let item = snap.queue_index.and_then(|index| snap.queue.get(index));
        let title = item
            .map(|item| item.title.clone())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "GXPlayer".into());
        let artist = item
            .map(|item| {
                if item.online {
                    "在线".to_owned()
                } else {
                    "本地".to_owned()
                }
            })
            .unwrap_or_else(|| "GXPlayer".into());

        if title != last_title || snap.generation != last_generation {
            if let Err(error) = controls.set_metadata(MediaMetadata {
                title: Some(title.as_str()),
                artist: Some(artist.as_str()),
                album: Some("GXPlayer"),
                cover_url: None,
                duration: snap
                    .duration_seconds
                    .filter(|v| v.is_finite() && *v > 0.0)
                    .map(Duration::from_secs_f64),
            }) {
                eprintln!("GX_SMTC set_metadata: {error:?}");
            }
            last_title = title;
            last_generation = snap.generation;
        }

        let status_label: &'static str = match snap.status {
            PlaybackStatus::Playing | PlaybackStatus::Loading => "playing",
            PlaybackStatus::Paused | PlaybackStatus::Buffering => "paused",
            _ => "stopped",
        };
        if last_status != Some(status_label) {
            let playback = match status_label {
                "playing" => MediaPlayback::Playing {
                    progress: Some(souvlaki::MediaPosition(Duration::from_secs_f64(
                        snap.position_seconds.max(0.0),
                    ))),
                },
                "paused" => MediaPlayback::Paused {
                    progress: Some(souvlaki::MediaPosition(Duration::from_secs_f64(
                        snap.position_seconds.max(0.0),
                    ))),
                },
                _ => MediaPlayback::Stopped,
            };
            if let Err(error) = controls.set_playback(playback) {
                eprintln!("GX_SMTC set_playback: {error:?}");
            } else {
                println!("GX_SMTC playback={status_label}");
            }
            last_status = Some(status_label);
        }
    }
}
