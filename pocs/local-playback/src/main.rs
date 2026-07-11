use std::env;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use gx_audio::{PlaybackOptions, decode_window, play_local_file, probe_local_file};

fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    let path = args
        .next()
        .map(PathBuf::from)
        .context("usage: local-playback <file> [start-seconds] [play-seconds]")?;
    let start_seconds = parse_optional_f64(args.next(), 0.0, "start-seconds")?;
    let play_seconds = parse_optional_f64(args.next(), 5.0, "play-seconds")?;
    if args.next().is_some() {
        bail!("usage: local-playback <file> [start-seconds] [play-seconds]");
    }

    let info = probe_local_file(&path)?;
    println!("probe: {info:#?}");

    let seek_probe = decode_window(&path, start_seconds, 2_048)?;
    if seek_probe.is_empty() {
        bail!("seek verification decoded no PCM samples");
    }
    let peak = seek_probe
        .iter()
        .fold(0.0f32, |current, sample| current.max(sample.abs()));
    println!(
        "seek verification: {} samples from {:.3}s, peak {:.6}",
        seek_probe.len(),
        start_seconds,
        peak
    );

    let report = play_local_file(
        &path,
        PlaybackOptions {
            start_seconds,
            max_seconds: Some(play_seconds),
            ..PlaybackOptions::default()
        },
    )?;
    println!("playback: {report:#?}");
    Ok(())
}

fn parse_optional_f64(value: Option<String>, default: f64, label: &str) -> Result<f64> {
    match value {
        Some(value) => value
            .parse::<f64>()
            .with_context(|| format!("invalid {label}: {value}")),
        None => Ok(default),
    }
}
