use std::env;
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use gx_audio::engine::{AudioMode, EngineSnapshot, LocalAudioEngine};
use gx_contracts::PlaybackStatus;
use gx_dsp::{DspSettings, EqBand};

fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    let path = args
        .next()
        .map(PathBuf::from)
        .context("usage: engine-smoke <audio-file>")?;
    if args.next().as_deref() == Some("--stability") {
        let seconds = args
            .next()
            .map(|value| value.parse::<u64>())
            .transpose()
            .context("stability duration must be an integer number of seconds")?
            .unwrap_or(25);
        return run_stability(path, seconds);
    }
    let engine = LocalAudioEngine::new()?;
    let output_devices = engine.output_devices()?;
    if output_devices.is_empty() {
        bail!("no output devices were enumerated");
    }
    println!("output devices: {output_devices:?}");
    engine.load(vec![path.clone(), path])?;

    let playing = wait_for(&engine, "initial playback", |state| {
        state.status == PlaybackStatus::Playing && state.queue_index == Some(0)
    })?;
    println!("initial playback: {playing:#?}");
    thread::sleep(Duration::from_millis(600));

    engine.pause()?;
    let paused = wait_for(&engine, "pause", |state| {
        state.status == PlaybackStatus::Paused
    })?;
    thread::sleep(Duration::from_millis(400));
    let paused_later = engine.snapshot();
    if (paused_later.position_seconds - paused.position_seconds).abs() > 0.05 {
        bail!(
            "position advanced while paused: {:.3} -> {:.3}",
            paused.position_seconds,
            paused_later.position_seconds
        );
    }
    println!("pause stable at {:.3}s", paused.position_seconds);

    engine.seek(30.0)?;
    let seeked = wait_for(&engine, "seek while paused", |state| {
        state.status == PlaybackStatus::Paused && (state.position_seconds - 30.0).abs() < 0.2
    })?;
    println!("seeked: position={:.3}s", seeked.position_seconds);

    engine.set_volume(0.25)?;
    engine.play()?;
    let resumed = wait_for(&engine, "resume", |state| {
        state.status == PlaybackStatus::Playing
            && (state.volume - 0.25).abs() < f32::EPSILON
            && state.position_seconds >= 30.0
    })?;
    println!(
        "resumed at {:.3}s, volume={:.2}",
        resumed.position_seconds, resumed.volume
    );

    engine.next()?;
    let next = wait_for(&engine, "next track", |state| {
        state.status == PlaybackStatus::Playing && state.queue_index == Some(1)
    })?;
    println!("next track generation={}", next.generation);

    engine.previous()?;
    let previous = wait_for(&engine, "previous track", |state| {
        state.status == PlaybackStatus::Playing && state.queue_index == Some(0)
    })?;
    println!(
        "engine smoke passed: queue={}, underruns={}",
        previous.queue.len(),
        previous.underrun_callbacks
    );

    engine.set_dsp_settings(DspSettings {
        enabled: true,
        eq_enabled: true,
        eq_bands: vec![EqBand::peak(1_000.0, 9.0, 1.0)],
        ..DspSettings::default()
    })?;
    let eq_on = wait_for(&engine, "EQ enable", |state| {
        state.status == PlaybackStatus::Playing
            && state.dsp_settings.enabled
            && state.dsp_settings.eq_enabled
            && state.dsp_settings.eq_bands[0].gain_db == 9.0
    })?;
    println!("EQ enabled at {:.3}s", eq_on.position_seconds);

    engine.seek(40.0)?;
    let eq_seeked = wait_for(&engine, "seek with EQ enabled", |state| {
        state.status == PlaybackStatus::Playing
            && state.dsp_settings.enabled
            && state.position_seconds >= 40.0
    })?;
    println!("EQ seek passed at {:.3}s", eq_seeked.position_seconds);

    engine.set_audio_mode(AudioMode::CinemaGame)?;
    let spatial_on = wait_for(&engine, "spatial DSP enable", |state| {
        state.status == PlaybackStatus::Playing
            && state.audio_mode == AudioMode::CinemaGame
            && state.dsp_settings.crossfeed.enabled
            && state.dsp_settings.hrtf.enabled
            && state.dsp_settings.limiter.enabled
    })?;
    println!("spatial DSP enabled at {:.3}s", spatial_on.position_seconds);
    engine.seek(50.0)?;
    let spatial_seeked = wait_for(&engine, "seek with spatial DSP enabled", |state| {
        state.status == PlaybackStatus::Playing
            && state.dsp_settings.hrtf.enabled
            && state.position_seconds >= 50.0
    })?;
    thread::sleep(Duration::from_secs(2));
    let spatial_stable = engine.snapshot();
    if spatial_stable.underrun_callbacks != 0 {
        bail!(
            "spatial playback reported {} underrun callbacks",
            spatial_stable.underrun_callbacks
        );
    }
    println!(
        "spatial seek/stability passed at {:.3}s, underruns={}",
        spatial_seeked.position_seconds, spatial_stable.underrun_callbacks
    );

    let original_device = spatial_stable.output_device.clone();
    let selected_device = output_devices
        .iter()
        .find(|name| Some(name.as_str()) != original_device.as_deref())
        .cloned()
        .unwrap_or_else(|| output_devices[0].clone());
    let before_device_generation = spatial_stable.generation;
    engine.set_output_device(Some(selected_device.clone()))?;
    let switched = wait_for(&engine, "output device switch", |state| {
        state.status == PlaybackStatus::Playing
            && state.output_device.as_deref() == Some(selected_device.as_str())
            && state.generation > before_device_generation
    })?;
    println!(
        "output device switch passed: device={selected_device:?}, position={:.3}s",
        switched.position_seconds
    );

    engine.set_output_device(Some("GXPlayer definitely missing output device".into()))?;
    let invalid = wait_for_status(&engine, "invalid output device", PlaybackStatus::Failed)?;
    let invalid_error = invalid.error.as_deref().unwrap_or_default();
    if !invalid_error.contains("unavailable") {
        bail!("invalid output device returned an unclear error: {invalid_error:?}");
    }
    println!("invalid output device rejected clearly: {invalid_error}");

    engine.set_output_device(original_device.clone())?;
    let recovered = wait_for_after_generation(
        &engine,
        "output device recovery",
        invalid.generation,
        |state| state.status == PlaybackStatus::Playing && state.output_device == original_device,
    )?;
    println!(
        "output device recovery passed at {:.3}s",
        recovered.position_seconds
    );

    engine.set_audio_mode(AudioMode::Music)?;
    let bypassed = wait_for(&engine, "DSP bypass", |state| {
        state.status == PlaybackStatus::Playing
            && state.audio_mode == AudioMode::Music
            && !state.dsp_settings.enabled
    })?;
    println!("DSP bypass restored at {:.3}s", bypassed.position_seconds);

    let short_path = write_short_wav()?;
    engine.load(vec![short_path.clone(), short_path.clone()])?;
    let automatic_next = wait_for(&engine, "automatic next track", |state| {
        state.status == PlaybackStatus::Playing && state.queue_index == Some(1)
    })?;
    println!(
        "automatic next passed: generation={}, position={:.3}s",
        automatic_next.generation, automatic_next.position_seconds
    );
    drop(engine);
    fs::remove_file(short_path)?;
    Ok(())
}

fn run_stability(path: PathBuf, seconds: u64) -> Result<()> {
    let engine = LocalAudioEngine::new()?;
    engine.load(vec![path])?;
    let started = wait_for(&engine, "stability playback", |state| {
        state.status == PlaybackStatus::Playing
    })?;
    println!(
        "GX_ENGINE_STABILITY_STARTED output_rate={:?} position={:.3}",
        started.output_sample_rate, started.position_seconds
    );
    for _ in 0..seconds {
        thread::sleep(Duration::from_secs(1));
        let snapshot = engine.snapshot();
        if snapshot.status == PlaybackStatus::Failed {
            bail!("stability playback failed: {:?}", snapshot.error);
        }
        if snapshot.status == PlaybackStatus::Stopped {
            break;
        }
    }
    let final_state = engine.snapshot();
    println!(
        "GX_ENGINE_STABILITY_OK position={:.3} underruns={} output_rate={:?}",
        final_state.position_seconds,
        final_state.underrun_callbacks,
        final_state.output_sample_rate
    );
    if final_state.underrun_callbacks != 0 {
        bail!(
            "stability playback reported {} underrun callbacks",
            final_state.underrun_callbacks
        );
    }
    Ok(())
}

fn wait_for_after_generation(
    engine: &LocalAudioEngine,
    label: &str,
    previous_generation: u64,
    predicate: impl Fn(&EngineSnapshot) -> bool,
) -> Result<EngineSnapshot> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let snapshot = engine.snapshot();
        if snapshot.generation > previous_generation && predicate(&snapshot) {
            return Ok(snapshot);
        }
        if snapshot.generation > previous_generation && snapshot.status == PlaybackStatus::Failed {
            bail!("{label} failed: {:?}", snapshot.error);
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for {label}; last state: {snapshot:#?}");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_status(
    engine: &LocalAudioEngine,
    label: &str,
    status: PlaybackStatus,
) -> Result<EngineSnapshot> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let snapshot = engine.snapshot();
        if snapshot.status == status {
            return Ok(snapshot);
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for {label}; last state: {snapshot:#?}");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for(
    engine: &LocalAudioEngine,
    label: &str,
    predicate: impl Fn(&EngineSnapshot) -> bool,
) -> Result<EngineSnapshot> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let snapshot = engine.snapshot();
        if predicate(&snapshot) {
            return Ok(snapshot);
        }
        if snapshot.status == PlaybackStatus::Failed {
            bail!("{label} failed: {:?}", snapshot.error);
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for {label}; last state: {snapshot:#?}");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn write_short_wav() -> Result<PathBuf> {
    let path = env::temp_dir().join(format!("gxplayer-auto-next-{}.wav", std::process::id()));
    let sample_rate = 8_000u32;
    let channels = 2u16;
    let frames = 3_200u32;
    let bits_per_sample = 16u16;
    let block_align = channels * (bits_per_sample / 8);
    let byte_rate = sample_rate * block_align as u32;
    let data_size = frames * block_align as u32;
    let mut file = File::create(&path)?;
    file.write_all(b"RIFF")?;
    file.write_all(&(36 + data_size).to_le_bytes())?;
    file.write_all(b"WAVEfmt ")?;
    file.write_all(&16u32.to_le_bytes())?;
    file.write_all(&1u16.to_le_bytes())?;
    file.write_all(&channels.to_le_bytes())?;
    file.write_all(&sample_rate.to_le_bytes())?;
    file.write_all(&byte_rate.to_le_bytes())?;
    file.write_all(&block_align.to_le_bytes())?;
    file.write_all(&bits_per_sample.to_le_bytes())?;
    file.write_all(b"data")?;
    file.write_all(&data_size.to_le_bytes())?;
    for frame in 0..frames {
        let sample = ((frame as f32 * 440.0 * std::f32::consts::TAU / sample_rate as f32).sin()
            * i16::MAX as f32
            * 0.08) as i16;
        for _ in 0..channels {
            file.write_all(&sample.to_le_bytes())?;
        }
    }
    Ok(path)
}
