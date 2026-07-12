//! Headless native audio primitives used by the Phase -1 local playback PoC.

pub mod engine;

use std::fs::File;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample, Stream, StreamConfig};
use ringbuf::{HeapRb, traits::*};
use rubato::{FftFixedIn, Resampler};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_NULL, CodecParameters, Decoder, DecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::{MetadataOptions, StandardTagKey};
use symphonia::core::probe::Hint;

#[derive(Debug, Clone, PartialEq)]
pub struct LocalMediaInfo {
    pub path: PathBuf,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub sample_rate: u32,
    pub channels: u16,
    pub duration_seconds: Option<f64>,
}

/// Embedded cover art extracted from a local file (JPEG/PNG bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedCover {
    pub mime: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Copy)]
pub struct PlaybackOptions {
    pub start_seconds: f64,
    pub max_seconds: Option<f64>,
    pub ring_buffer_seconds: f64,
}

impl Default for PlaybackOptions {
    fn default() -> Self {
        Self {
            start_seconds: 0.0,
            max_seconds: None,
            ring_buffer_seconds: 2.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlaybackReport {
    pub device_name: String,
    pub source_sample_rate: u32,
    pub output_sample_rate: u32,
    pub channels: u16,
    pub decoded_frames: u64,
    pub underrun_callbacks: u64,
    pub started_at_seconds: f64,
    pub resampled: bool,
}

struct OpenedMedia {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    codec_params: CodecParameters,
    prefetched_samples: Vec<f32>,
}

pub fn probe_local_file(path: impl AsRef<Path>) -> Result<LocalMediaInfo> {
    let path = path.as_ref();
    let mut opened = open_media(path)?;
    let sample_rate = opened
        .codec_params
        .sample_rate
        .context("audio track does not declare a sample rate")?;
    let channels = opened
        .codec_params
        .channels
        .context("audio track does not declare a channel layout")?
        .count() as u16;
    let duration_seconds = opened
        .codec_params
        .n_frames
        .map(|frames| frames as f64 / sample_rate as f64);

    let metadata = opened.format.metadata().current().map(|revision| {
        let value = |key| {
            revision
                .tags()
                .iter()
                .find(|tag| tag.std_key == Some(key))
                .map(|tag| tag.value.to_string())
                .filter(|value| !value.trim().is_empty())
        };
        (
            value(StandardTagKey::TrackTitle),
            value(StandardTagKey::Artist),
            value(StandardTagKey::Album),
        )
    });
    let (title, artist, album) = metadata.unwrap_or_default();

    Ok(LocalMediaInfo {
        path: path.to_path_buf(),
        title,
        artist,
        album,
        sample_rate,
        channels,
        duration_seconds,
    })
}

/// Read the first embedded cover picture from a local audio file, if any.
pub fn extract_embedded_cover(path: impl AsRef<Path>) -> Result<Option<EmbeddedCover>> {
    use symphonia::core::meta::StandardVisualKey;
    let path = path.as_ref();
    let mut opened = open_media(path)?;
    // Prefer format-level metadata visuals.
    if let Some(revision) = opened.format.metadata().current()
        && let Some(cover) = pick_visual(revision.visuals())
    {
        return Ok(Some(cover));
    }
    // Some containers only expose visuals on the probe metadata queue.
    while !opened.format.metadata().is_latest() {
        opened.format.metadata().skip_to_latest();
        if let Some(revision) = opened.format.metadata().current()
            && let Some(cover) = pick_visual(revision.visuals())
        {
            return Ok(Some(cover));
        }
    }
    let _ = StandardVisualKey::FrontCover;
    Ok(None)
}

fn pick_visual(visuals: &[symphonia::core::meta::Visual]) -> Option<EmbeddedCover> {
    let preferred = visuals.iter().find(|visual| {
        visual
            .usage
            .is_some_and(|usage| usage == symphonia::core::meta::StandardVisualKey::FrontCover)
    });
    let visual = preferred.or_else(|| visuals.first())?;
    let mime = if visual.media_type.trim().is_empty() {
        "image/jpeg".into()
    } else {
        visual.media_type.clone()
    };
    if visual.data.is_empty() {
        return None;
    }
    Some(EmbeddedCover {
        mime,
        data: visual.data.to_vec(),
    })
}

/// Decode an interleaved f32 window without touching an audio device.
///
/// This provides a deterministic seek verification hook for Phase -1 and later PCM tests.
pub fn decode_window(
    path: impl AsRef<Path>,
    start_seconds: f64,
    max_frames: usize,
) -> Result<Vec<f32>> {
    let mut media = open_media(path.as_ref())?;
    seek_media(&mut media, start_seconds)?;

    let channels = media
        .codec_params
        .channels
        .context("audio track does not declare a channel layout")?
        .count();
    let wanted_samples = max_frames.saturating_mul(channels);
    let mut output = Vec::with_capacity(wanted_samples);

    decode_samples(&mut media, |samples| {
        let remaining = wanted_samples.saturating_sub(output.len());
        output.extend_from_slice(&samples[..samples.len().min(remaining)]);
        Ok(output.len() < wanted_samples)
    })?;

    Ok(output)
}

pub fn play_local_file(path: impl AsRef<Path>, options: PlaybackOptions) -> Result<PlaybackReport> {
    let path = path.as_ref();
    let info = probe_local_file(path)?;
    let mut media = open_media(path)?;
    seek_media(&mut media, options.start_seconds)?;

    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .context("no default audio output device is available")?;
    let device_name = device.name().unwrap_or_else(|_| "<unknown>".into());
    let supported = choose_output_config(&device, info.sample_rate, info.channels)?;
    let sample_format = supported.sample_format();
    let stream_config: StreamConfig = supported.into();
    let output_sample_rate = stream_config.sample_rate.0;
    let output_channels = stream_config.channels as usize;

    let capacity = ((output_sample_rate as f64
        * output_channels as f64
        * options.ring_buffer_seconds.max(0.25)) as usize)
        .max(4096);
    let ring = HeapRb::<f32>::new(capacity);
    let (mut producer, consumer) = ring.split();
    let queued_samples = Arc::new(AtomicUsize::new(0));
    let underrun_callbacks = Arc::new(AtomicU64::new(0));

    let stream = build_output_stream(
        &device,
        &stream_config,
        sample_format,
        consumer,
        Arc::clone(&queued_samples),
        Arc::clone(&underrun_callbacks),
    )?;
    stream
        .play()
        .context("failed to start audio output stream")?;

    let max_frames = options
        .max_seconds
        .map(|seconds| (seconds.max(0.0) * info.sample_rate as f64) as u64);
    let decoded_frames = Arc::new(AtomicU64::new(0));
    let decoded_frames_for_push = Arc::clone(&decoded_frames);
    let channels = info.channels as usize;
    let mut rate_adapter = RateAdapter::new(info.sample_rate, output_sample_rate, channels)?;

    decode_samples(&mut media, |samples| {
        let already_decoded = decoded_frames_for_push.load(Ordering::Relaxed);
        let remaining_samples = max_frames
            .map(|frames| frames.saturating_sub(already_decoded) as usize * channels)
            .unwrap_or(samples.len());
        let to_push = samples.len().min(remaining_samples);

        let prepared = rate_adapter.process(&samples[..to_push])?;
        let prepared = remap_channels(&prepared, channels, output_channels)?;
        push_samples(&mut producer, &queued_samples, &prepared);

        let frames = (to_push / channels) as u64;
        let total = decoded_frames_for_push.fetch_add(frames, Ordering::Relaxed) + frames;
        Ok(max_frames.is_none_or(|limit| total < limit))
    })?;
    let tail = rate_adapter.finish()?;
    let tail = remap_channels(&tail, channels, output_channels)?;
    push_samples(&mut producer, &queued_samples, &tail);

    let drain_deadline = Instant::now() + Duration::from_secs(10);
    while queued_samples.load(Ordering::Acquire) != 0 && Instant::now() < drain_deadline {
        thread::sleep(Duration::from_millis(5));
    }
    if queued_samples.load(Ordering::Acquire) != 0 {
        bail!("audio device did not drain the PCM ring buffer before timeout");
    }

    drop(stream);

    Ok(PlaybackReport {
        device_name,
        source_sample_rate: info.sample_rate,
        output_sample_rate,
        channels: info.channels,
        decoded_frames: decoded_frames.load(Ordering::Relaxed),
        underrun_callbacks: underrun_callbacks.load(Ordering::Relaxed),
        started_at_seconds: options.start_seconds,
        resampled: info.sample_rate != output_sample_rate,
    })
}

enum RateAdapter {
    Passthrough,
    Rubato {
        resampler: Box<FftFixedIn<f32>>,
        pending: Vec<Vec<f32>>,
        channels: usize,
    },
}

impl RateAdapter {
    fn new(input_rate: u32, output_rate: u32, channels: usize) -> Result<Self> {
        if input_rate == output_rate {
            return Ok(Self::Passthrough);
        }
        let resampler =
            FftFixedIn::<f32>::new(input_rate as usize, output_rate as usize, 1024, 2, channels)
                .context("failed to construct rubato FFT resampler")?;
        Ok(Self::Rubato {
            resampler: Box::new(resampler),
            pending: vec![Vec::new(); channels],
            channels,
        })
    }

    fn process(&mut self, interleaved: &[f32]) -> Result<Vec<f32>> {
        match self {
            Self::Passthrough => Ok(interleaved.to_vec()),
            Self::Rubato {
                resampler,
                pending,
                channels,
            } => {
                for frame in interleaved.chunks_exact(*channels) {
                    for (channel, sample) in frame.iter().enumerate() {
                        pending[channel].push(*sample);
                    }
                }

                let mut output = Vec::new();
                loop {
                    let needed = resampler.input_frames_next();
                    if pending[0].len() < needed {
                        break;
                    }
                    let input = pending
                        .iter_mut()
                        .map(|channel| channel.drain(..needed).collect::<Vec<_>>())
                        .collect::<Vec<_>>();
                    let planar = resampler
                        .process(&input, None)
                        .context("rubato failed to resample an audio block")?;
                    interleave_planar(&planar, &mut output);
                }
                Ok(output)
            }
        }
    }

    fn finish(&mut self) -> Result<Vec<f32>> {
        match self {
            Self::Passthrough => Ok(Vec::new()),
            Self::Rubato {
                resampler, pending, ..
            } => {
                if pending[0].is_empty() {
                    return Ok(Vec::new());
                }
                let planar = resampler
                    .process_partial(Some(pending), None)
                    .context("rubato failed to flush the final audio block")?;
                let mut output = Vec::new();
                interleave_planar(&planar, &mut output);
                pending.iter_mut().for_each(Vec::clear);
                Ok(output)
            }
        }
    }
}

fn interleave_planar(planar: &[Vec<f32>], output: &mut Vec<f32>) {
    let frames = planar.first().map_or(0, Vec::len);
    for frame in 0..frames {
        for channel in planar {
            output.push(channel[frame]);
        }
    }
}

fn push_samples<P>(producer: &mut P, queued_samples: &AtomicUsize, samples: &[f32])
where
    P: Producer<Item = f32>,
{
    for &sample in samples {
        let mut pending = sample;
        loop {
            queued_samples.fetch_add(1, Ordering::Release);
            match producer.try_push(pending) {
                Ok(()) => break,
                Err(returned) => {
                    queued_samples.fetch_sub(1, Ordering::Release);
                    pending = returned;
                    thread::sleep(Duration::from_millis(1));
                }
            }
        }
    }
}

fn open_media(path: &Path) -> Result<OpenedMedia> {
    let file = File::open(path)
        .with_context(|| format!("failed to open local media {}", path.display()))?;
    open_media_source(
        Box::new(file),
        path.extension().and_then(|value| value.to_str()),
        &format!("local media {}", path.display()),
    )
}

fn open_media_source(
    source: Box<dyn MediaSource>,
    extension: Option<&str>,
    description: &str,
) -> Result<OpenedMedia> {
    let stream = MediaSourceStream::new(source, Default::default());
    let mut hint = Hint::new();
    if let Some(extension) = extension {
        hint.with_extension(extension);
    }

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            stream,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .with_context(|| format!("failed to probe media format for {description}"))?;
    let mut format = probed.format;
    let track = format
        .default_track()
        .context("media contains no default audio track")?;
    if track.codec_params.codec == CODEC_TYPE_NULL {
        bail!("default track has no supported codec");
    }
    let track_id = track.id;
    let mut codec_params = track.codec_params.clone();
    let mut decoder = symphonia::default::get_codecs()
        .make(&codec_params, &DecoderOptions::default())
        .context("failed to create audio decoder")?;

    let mut prefetched_samples = Vec::new();
    if codec_params.sample_rate.is_none() || codec_params.channels.is_none() {
        loop {
            let packet = format
                .next_packet()
                .context("failed to inspect media audio parameters")?;
            if packet.track_id() != track_id {
                continue;
            }
            match decoder.decode(&packet) {
                Ok(decoded) => {
                    codec_params.sample_rate = Some(decoded.spec().rate);
                    codec_params.channels = Some(decoded.spec().channels);
                    let mut buffer =
                        SampleBuffer::<f32>::new(decoded.capacity() as u64, *decoded.spec());
                    buffer.copy_interleaved_ref(decoded);
                    prefetched_samples.extend_from_slice(buffer.samples());
                    break;
                }
                Err(SymphoniaError::DecodeError(_)) => continue,
                Err(error) => {
                    return Err(error).context("failed to inspect media audio parameters");
                }
            }
        }
    }

    Ok(OpenedMedia {
        format,
        decoder,
        track_id,
        codec_params,
        prefetched_samples,
    })
}

fn seek_media_coarse(media: &mut OpenedMedia, seconds: f64) -> Result<usize> {
    if seconds <= 0.0 {
        return Ok(0);
    }
    let seeked = media
        .format
        .seek(
            SeekMode::Coarse,
            SeekTo::Time {
                time: seconds.into(),
                track_id: Some(media.track_id),
            },
        )
        .with_context(|| format!("failed to coarse-seek to {seconds:.3}s"))?;
    media.decoder.reset();
    media.prefetched_samples.clear();
    Ok(seeked.required_ts.saturating_sub(seeked.actual_ts) as usize)
}

fn seek_media(media: &mut OpenedMedia, seconds: f64) -> Result<()> {
    if seconds <= 0.0 {
        return Ok(());
    }
    media
        .format
        .seek(
            SeekMode::Accurate,
            SeekTo::Time {
                time: seconds.into(),
                track_id: Some(media.track_id),
            },
        )
        .with_context(|| format!("failed to seek to {seconds:.3}s"))?;
    media.decoder.reset();
    media.prefetched_samples.clear();
    Ok(())
}

fn decode_samples(
    media: &mut OpenedMedia,
    mut consume: impl FnMut(&[f32]) -> Result<bool>,
) -> Result<()> {
    if !media.prefetched_samples.is_empty() {
        let prefetched = std::mem::take(&mut media.prefetched_samples);
        if !consume(&prefetched)? {
            return Ok(());
        }
    }
    let mut sample_buffer = None;

    loop {
        let packet = match media.format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(error)) if error.kind() == ErrorKind::UnexpectedEof => {
                break;
            }
            Err(error) => return Err(error).context("failed to read media packet"),
        };
        if packet.track_id() != media.track_id {
            continue;
        }

        let decoded = match media.decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(SymphoniaError::IoError(error)) if error.kind() == ErrorKind::UnexpectedEof => {
                break;
            }
            Err(error) => return Err(error).context("failed to decode media packet"),
        };

        if sample_buffer.is_none() {
            sample_buffer = Some(SampleBuffer::<f32>::new(
                decoded.capacity() as u64,
                *decoded.spec(),
            ));
        }
        let buffer = sample_buffer.as_mut().expect("sample buffer initialized");
        buffer.copy_interleaved_ref(decoded);
        if !consume(buffer.samples())? {
            break;
        }
    }
    Ok(())
}

fn choose_output_config(
    device: &cpal::Device,
    sample_rate: u32,
    channels: u16,
) -> Result<cpal::SupportedStreamConfig> {
    let default = device.default_output_config().ok();
    let default_channels = default.as_ref().map(|config| config.channels());
    let mut candidates = device
        .supported_output_configs()
        .context("failed to enumerate output configurations")?
        .filter_map(|config| {
            let format_rank = sample_format_rank(config.sample_format())?;
            let selected_rate =
                sample_rate.clamp(config.min_sample_rate().0, config.max_sample_rate().0);
            let score = (
                channel_config_rank(config.channels(), channels, default_channels),
                u8::from(selected_rate != sample_rate),
                selected_rate.abs_diff(sample_rate),
                format_rank,
            );
            Some((
                score,
                config.with_sample_rate(cpal::SampleRate(selected_rate)),
            ))
        })
        .collect::<Vec<_>>();

    candidates.sort_by_key(|(score, _)| *score);
    if let Some((_, best)) = candidates.into_iter().next() {
        return Ok(best);
    }

    if let Some(default) = default
        && sample_format_rank(default.sample_format()).is_some()
    {
        return Ok(default);
    }
    Err(anyhow!(
        "audio device exposes no output configuration with a supported sample format"
    ))
}

fn channel_config_rank(output: u16, requested: u16, default: Option<u16>) -> u16 {
    if output == requested {
        0
    } else if output == 2 {
        1
    } else if Some(output) == default {
        2
    } else if output == 1 {
        3
    } else {
        4 + output.abs_diff(requested)
    }
}

fn sample_format_rank(format: SampleFormat) -> Option<u8> {
    Some(match format {
        SampleFormat::F32 => 0,
        SampleFormat::F64 => 1,
        SampleFormat::I16 => 2,
        SampleFormat::I32 => 3,
        SampleFormat::U16 => 4,
        SampleFormat::U32 => 5,
        SampleFormat::I8 => 6,
        SampleFormat::U8 => 7,
        SampleFormat::I64 => 8,
        SampleFormat::U64 => 9,
        _ => return None,
    })
}

fn remap_channels(
    interleaved: &[f32],
    input_channels: usize,
    output_channels: usize,
) -> Result<Vec<f32>> {
    if input_channels == 0 || output_channels == 0 {
        bail!("channel counts must be non-zero");
    }
    if !interleaved.len().is_multiple_of(input_channels) {
        bail!(
            "PCM sample count {} is not divisible by {input_channels} source channels",
            interleaved.len()
        );
    }
    if input_channels == output_channels {
        return Ok(interleaved.to_vec());
    }

    let frames = interleaved.len() / input_channels;
    let mut output = Vec::with_capacity(frames.saturating_mul(output_channels));
    for frame in interleaved.chunks_exact(input_channels) {
        match (input_channels, output_channels) {
            (1, _) => output.extend(std::iter::repeat_n(frame[0], output_channels)),
            (_, 1) => output.push(frame.iter().copied().sum::<f32>() / input_channels as f32),
            (_, 2) => {
                let (left, right) = downmix_stereo(frame);
                output.extend_from_slice(&[left, right]);
            }
            (2, _) => {
                output.extend_from_slice(frame);
                output.extend(std::iter::repeat_n(0.0, output_channels - 2));
            }
            _ => {
                let copied = input_channels.min(output_channels);
                output.extend_from_slice(&frame[..copied]);
                output.extend(std::iter::repeat_n(0.0, output_channels - copied));
            }
        }
    }
    Ok(output)
}

fn downmix_stereo(frame: &[f32]) -> (f32, f32) {
    debug_assert!(frame.len() >= 2);
    let mut left = frame[0];
    let mut right = frame[1];
    match frame.len() {
        2 => {}
        3 => {
            left += frame[2] * std::f32::consts::FRAC_1_SQRT_2;
            right += frame[2] * std::f32::consts::FRAC_1_SQRT_2;
        }
        4 => {
            left += frame[2] * std::f32::consts::FRAC_1_SQRT_2;
            right += frame[3] * std::f32::consts::FRAC_1_SQRT_2;
        }
        _ => {
            // Common 5.0/5.1+ order: FL, FR, FC, [LFE], surrounds/back channels.
            left += frame[2] * std::f32::consts::FRAC_1_SQRT_2;
            right += frame[2] * std::f32::consts::FRAC_1_SQRT_2;
            let surround_start = if frame.len() >= 6 {
                left += frame[3] * 0.5;
                right += frame[3] * 0.5;
                4
            } else {
                3
            };
            for (index, sample) in frame[surround_start..].iter().enumerate() {
                if index % 2 == 0 {
                    left += *sample * std::f32::consts::FRAC_1_SQRT_2;
                } else {
                    right += *sample * std::f32::consts::FRAC_1_SQRT_2;
                }
            }
        }
    }
    (left.clamp(-1.0, 1.0), right.clamp(-1.0, 1.0))
}

fn build_output_stream<C>(
    device: &cpal::Device,
    config: &StreamConfig,
    sample_format: SampleFormat,
    consumer: C,
    queued_samples: Arc<AtomicUsize>,
    underrun_callbacks: Arc<AtomicU64>,
) -> Result<Stream>
where
    C: Consumer<Item = f32> + Send + 'static,
{
    match sample_format {
        SampleFormat::I8 => build_typed_output_stream::<i8, C>(
            device,
            config,
            consumer,
            queued_samples,
            underrun_callbacks,
        ),
        SampleFormat::F32 => build_typed_output_stream::<f32, C>(
            device,
            config,
            consumer,
            queued_samples,
            underrun_callbacks,
        ),
        SampleFormat::I16 => build_typed_output_stream::<i16, C>(
            device,
            config,
            consumer,
            queued_samples,
            underrun_callbacks,
        ),
        SampleFormat::U16 => build_typed_output_stream::<u16, C>(
            device,
            config,
            consumer,
            queued_samples,
            underrun_callbacks,
        ),
        SampleFormat::I32 => build_typed_output_stream::<i32, C>(
            device,
            config,
            consumer,
            queued_samples,
            underrun_callbacks,
        ),
        SampleFormat::I64 => build_typed_output_stream::<i64, C>(
            device,
            config,
            consumer,
            queued_samples,
            underrun_callbacks,
        ),
        SampleFormat::U8 => build_typed_output_stream::<u8, C>(
            device,
            config,
            consumer,
            queued_samples,
            underrun_callbacks,
        ),
        SampleFormat::U32 => build_typed_output_stream::<u32, C>(
            device,
            config,
            consumer,
            queued_samples,
            underrun_callbacks,
        ),
        SampleFormat::U64 => build_typed_output_stream::<u64, C>(
            device,
            config,
            consumer,
            queued_samples,
            underrun_callbacks,
        ),
        SampleFormat::F64 => build_typed_output_stream::<f64, C>(
            device,
            config,
            consumer,
            queued_samples,
            underrun_callbacks,
        ),
        other => bail!("unsupported output sample format: {other}"),
    }
}

fn build_typed_output_stream<T, C>(
    device: &cpal::Device,
    config: &StreamConfig,
    mut consumer: C,
    queued_samples: Arc<AtomicUsize>,
    underrun_callbacks: Arc<AtomicU64>,
) -> Result<Stream>
where
    T: Sample + SizedSample + FromSample<f32>,
    C: Consumer<Item = f32> + Send + 'static,
{
    let stream = device.build_output_stream(
        config,
        move |output: &mut [T], _| {
            let mut starved = false;
            for target in output {
                let sample = match consumer.try_pop() {
                    Some(value) => {
                        queued_samples.fetch_sub(1, Ordering::Release);
                        value
                    }
                    None => {
                        starved = true;
                        0.0
                    }
                };
                *target = T::from_sample(sample);
            }
            if starved {
                underrun_callbacks.fetch_add(1, Ordering::Relaxed);
            }
        },
        |error| eprintln!("audio output stream error: {error}"),
        None,
    )?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn wav_probe_and_accurate_seek_are_repeatable() {
        let path = temporary_wav_path();
        write_two_tone_wav(&path).unwrap();

        let info = probe_local_file(&path).unwrap();
        assert_eq!(info.sample_rate, 8_000);
        assert_eq!(info.channels, 2);

        let beginning = decode_window(&path, 0.05, 128).unwrap();
        let second_half = decode_window(&path, 1.05, 128).unwrap();
        assert_eq!(beginning.len(), 256);
        assert_eq!(second_half.len(), 256);
        assert_ne!(beginning, second_half);

        fs::remove_file(path).unwrap();
    }

    #[test]
    fn resampler_preserves_long_form_duration() {
        let input_rate = 44_100u32;
        let output_rate = 48_000u32;
        let channels = 2usize;
        let input_frames = input_rate as usize * 30;
        let mut adapter = RateAdapter::new(input_rate, output_rate, channels).unwrap();
        let mut output_samples = 0usize;
        let packet = vec![0.1f32; 1024 * channels];
        let mut remaining = input_frames;
        while remaining > 0 {
            let frames = remaining.min(1024);
            output_samples += adapter.process(&packet[..frames * channels]).unwrap().len();
            remaining -= frames;
        }
        output_samples += adapter.finish().unwrap().len();
        let output_frames = output_samples / channels;
        let expected = output_rate as usize * 30;
        assert!(
            output_frames.abs_diff(expected) < 2048,
            "resampler produced {output_frames} frames, expected approximately {expected}"
        );
    }

    #[test]
    fn channel_mapper_handles_mono_stereo_and_surround() {
        assert_eq!(
            remap_channels(&[0.25, -0.5], 1, 2).unwrap(),
            vec![0.25, 0.25, -0.5, -0.5]
        );
        assert_eq!(
            remap_channels(&[0.25, -0.5], 2, 6).unwrap(),
            vec![0.25, -0.5, 0.0, 0.0, 0.0, 0.0]
        );
        let surround = remap_channels(&[0.2, -0.2, 0.1, 0.05, 0.3, -0.3], 6, 2).unwrap();
        assert_eq!(surround.len(), 2);
        assert!(surround[0] > 0.2);
        assert!(surround[1] < -0.2);
    }

    #[test]
    fn channel_config_ranking_prefers_exact_then_stereo() {
        assert_eq!(channel_config_rank(1, 1, Some(2)), 0);
        assert!(channel_config_rank(2, 1, Some(6)) < channel_config_rank(6, 1, Some(6)));
        assert!(sample_format_rank(SampleFormat::F32) < sample_format_rank(SampleFormat::I16));
    }

    fn temporary_wav_path() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("gxplayer-audio-{nonce}.wav"))
    }

    fn write_two_tone_wav(path: &Path) -> Result<()> {
        let sample_rate = 8_000u32;
        let channels = 2u16;
        let seconds = 2u32;
        let frames = sample_rate * seconds;
        let bits_per_sample = 16u16;
        let block_align = channels * (bits_per_sample / 8);
        let byte_rate = sample_rate * block_align as u32;
        let data_size = frames * block_align as u32;

        let mut file = File::create(path)?;
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
            let frequency = if frame < sample_rate { 220.0 } else { 880.0 };
            let value = ((frame as f32 * frequency * std::f32::consts::TAU / sample_rate as f32)
                .sin()
                * i16::MAX as f32
                * 0.25) as i16;
            for _ in 0..channels {
                file.write_all(&value.to_le_bytes())?;
            }
        }
        Ok(())
    }
}
