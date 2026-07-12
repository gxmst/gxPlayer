use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample, Stream, StreamConfig};
use crossbeam_channel::{Receiver, Sender, TryRecvError, bounded};
use gx_contracts::{MediaType, PlaybackStatus, ResolvedMediaRequest};
use gx_dsp::{CrossfeedSettings, DspChain, DspSettings, HrtfSettings, LimiterSettings};
use gx_streaming::HttpMediaSource;
use ringbuf::{HeapCons, HeapProd, HeapRb, traits::*};
use serde::{Deserialize, Serialize};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::errors::Error as SymphoniaError;

use super::{
    OpenedMedia, RateAdapter, choose_output_config, open_media, open_media_source, seek_media,
    seek_media_coarse,
};

const COMMAND_CAPACITY: usize = 64;
const RING_SECONDS: f64 = 0.75;
const PREBUFFER_SECONDS: f64 = 0.5;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct QueueItem {
    pub location: String,
    pub title: String,
    pub duration_seconds: Option<f64>,
    pub online: bool,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AudioMode {
    #[default]
    Music,
    CinemaGame,
}

impl AudioMode {
    fn dsp_settings(self) -> DspSettings {
        match self {
            Self::Music => DspSettings::default(),
            Self::CinemaGame => DspSettings {
                enabled: true,
                eq_enabled: false,
                crossfeed: CrossfeedSettings {
                    enabled: true,
                    ..CrossfeedSettings::default()
                },
                hrtf: HrtfSettings {
                    enabled: true,
                    ..HrtfSettings::default()
                },
                limiter: LimiterSettings {
                    enabled: true,
                    ..LimiterSettings::default()
                },
                ..DspSettings::default()
            },
        }
    }
}

#[derive(Clone)]
enum PlaybackSource {
    Local(PathBuf),
    Online(ResolvedMediaRequest),
}

#[derive(Clone)]
struct EngineQueueItem {
    public: QueueItem,
    source: PlaybackSource,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EngineSnapshot {
    pub status: PlaybackStatus,
    pub queue: Vec<QueueItem>,
    pub queue_index: Option<usize>,
    pub position_seconds: f64,
    pub duration_seconds: Option<f64>,
    pub volume: f32,
    pub audio_mode: AudioMode,
    pub dsp_settings: DspSettings,
    pub generation: u64,
    pub underrun_callbacks: u64,
    pub output_sample_rate: Option<u32>,
    pub error: Option<String>,
    pub output_device: Option<String>,
}

impl Default for EngineSnapshot {
    fn default() -> Self {
        Self {
            status: PlaybackStatus::Idle,
            queue: Vec::new(),
            queue_index: None,
            position_seconds: 0.0,
            duration_seconds: None,
            volume: 1.0,
            audio_mode: AudioMode::Music,
            dsp_settings: DspSettings::default(),
            generation: 0,
            underrun_callbacks: 0,
            output_sample_rate: None,
            error: None,
            output_device: None,
        }
    }
}

enum EngineCommand {
    Load(Vec<EngineQueueItem>),
    Play,
    Pause,
    Seek(f64),
    SetVolume(f32),
    SetAudioMode(AudioMode),
    SetDspSettings(DspSettings),
    SetOutputDevice(Option<String>),
    Next,
    Previous,
    Shutdown,
}

pub struct LocalAudioEngine {
    commands: Sender<EngineCommand>,
    snapshot: Arc<Mutex<EngineSnapshot>>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl LocalAudioEngine {
    pub fn new() -> Result<Self> {
        let (commands, receiver) = bounded(COMMAND_CAPACITY);
        let snapshot = Arc::new(Mutex::new(EngineSnapshot::default()));
        let snapshot_for_worker = Arc::clone(&snapshot);
        let worker = thread::Builder::new()
            .name("gx-local-audio-engine".into())
            .spawn(move || run_worker(receiver, snapshot_for_worker))
            .context("failed to spawn local audio engine worker")?;
        Ok(Self {
            commands,
            snapshot,
            worker: Mutex::new(Some(worker)),
        })
    }

    pub fn load(&self, paths: Vec<PathBuf>) -> Result<()> {
        if paths.is_empty() {
            bail!("at least one local audio path is required");
        }
        self.send(EngineCommand::Load(
            paths.into_iter().map(local_queue_item).collect(),
        ))
    }

    pub fn load_resolved(&self, request: ResolvedMediaRequest, title: String) -> Result<()> {
        if request.media_type == MediaType::Hls {
            bail!("HLS playback is not supported in v1");
        }
        let location = request.redacted_for_log();
        self.send(EngineCommand::Load(vec![EngineQueueItem {
            public: QueueItem {
                location,
                title,
                duration_seconds: None,
                online: true,
            },
            source: PlaybackSource::Online(request),
        }]))
    }

    pub fn play(&self) -> Result<()> {
        self.send(EngineCommand::Play)
    }

    pub fn pause(&self) -> Result<()> {
        self.send(EngineCommand::Pause)
    }

    pub fn seek(&self, seconds: f64) -> Result<()> {
        if !seconds.is_finite() || seconds < 0.0 {
            bail!("seek position must be a finite non-negative number");
        }
        self.send(EngineCommand::Seek(seconds))
    }

    pub fn set_volume(&self, volume: f32) -> Result<()> {
        if !volume.is_finite() {
            bail!("volume must be finite");
        }
        self.send(EngineCommand::SetVolume(volume.clamp(0.0, 1.0)))
    }

    pub fn set_dsp_settings(&self, settings: DspSettings) -> Result<()> {
        DspChain::new(48_000, 2, settings.clone())?;
        self.send(EngineCommand::SetDspSettings(settings))
    }

    pub fn set_audio_mode(&self, mode: AudioMode) -> Result<()> {
        let settings = mode.dsp_settings();
        DspChain::new(48_000, 2, settings.clone())?;
        self.send(EngineCommand::SetAudioMode(mode))
    }

    pub fn next(&self) -> Result<()> {
        self.send(EngineCommand::Next)
    }

    pub fn previous(&self) -> Result<()> {
        self.send(EngineCommand::Previous)
    }

    pub fn snapshot(&self) -> EngineSnapshot {
        self.snapshot.lock().unwrap().clone()
    }

    pub fn output_devices(&self) -> Result<Vec<String>> {
        let host = cpal::default_host();
        let mut names = host
            .output_devices()
            .context("failed to enumerate output devices")?
            .filter_map(|device| device.name().ok())
            .collect::<Vec<_>>();
        names.sort();
        names.dedup();
        Ok(names)
    }

    pub fn set_output_device(&self, name: Option<String>) -> Result<()> {
        if name.as_ref().is_some_and(|name| name.len() > 500) {
            bail!("output device name exceeds the size limit");
        }
        self.send(EngineCommand::SetOutputDevice(name))
    }

    fn send(&self, command: EngineCommand) -> Result<()> {
        self.commands
            .send(command)
            .map_err(|_| anyhow!("local audio engine is not running"))
    }
}

impl Drop for LocalAudioEngine {
    fn drop(&mut self) {
        let _ = self.commands.send(EngineCommand::Shutdown);
        if let Some(worker) = self.worker.lock().unwrap().take() {
            let _ = worker.join();
        }
    }
}

struct WorkerModel {
    queue: Vec<EngineQueueItem>,
    index: Option<usize>,
    status: PlaybackStatus,
    intent_playing: bool,
    reload_requested: bool,
    start_seconds: f64,
    volume: f32,
    audio_mode: AudioMode,
    dsp_settings: DspSettings,
    generation: u64,
    error: Option<String>,
    output_device: Option<String>,
}

impl Default for WorkerModel {
    fn default() -> Self {
        Self {
            queue: Vec::new(),
            index: None,
            status: PlaybackStatus::Idle,
            intent_playing: false,
            reload_requested: false,
            start_seconds: 0.0,
            volume: 1.0,
            audio_mode: AudioMode::Music,
            dsp_settings: DspSettings::default(),
            generation: 0,
            error: None,
            output_device: None,
        }
    }
}

fn run_worker(commands: Receiver<EngineCommand>, shared_snapshot: Arc<Mutex<EngineSnapshot>>) {
    let mut model = WorkerModel::default();
    let mut session: Option<PlaybackSession> = None;

    loop {
        let mut backpressured = false;
        let mut shutdown = false;
        loop {
            match commands.try_recv() {
                Ok(command) => {
                    if handle_command(command, &mut model, &mut session) {
                        shutdown = true;
                        break;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    shutdown = true;
                    break;
                }
            }
        }
        if shutdown {
            break;
        }

        if model.reload_requested {
            session = None;
            model.reload_requested = false;
            if let Some(item) = model.index.and_then(|index| model.queue.get(index)) {
                model.status = PlaybackStatus::Loading;
                publish_snapshot(&model, None, &shared_snapshot);
                match PlaybackSession::new(
                    &item.source,
                    model.start_seconds,
                    model.volume,
                    model.dsp_settings.clone(),
                    model.output_device.as_deref(),
                ) {
                    Ok(mut next_session) => {
                        if !model.intent_playing {
                            next_session.pause();
                            model.status = PlaybackStatus::Paused;
                        }
                        session = Some(next_session);
                        model.error = None;
                    }
                    Err(error) => {
                        model.status = PlaybackStatus::Failed;
                        model.error = Some(error.to_string());
                    }
                }
            }
        }

        if let Some(active) = session.as_mut() {
            if model.intent_playing {
                active.resume();
            } else {
                active.pause();
            }

            match active.pump() {
                Ok(result @ (PumpResult::Progress | PumpResult::Backpressure)) => {
                    backpressured = matches!(result, PumpResult::Backpressure);
                    model.status = if model.intent_playing && active.has_started() {
                        PlaybackStatus::Playing
                    } else if model.intent_playing {
                        PlaybackStatus::Loading
                    } else {
                        PlaybackStatus::Paused
                    };
                }
                Ok(PumpResult::Ended) => {
                    if let Some(index) = model.index {
                        if index + 1 < model.queue.len() {
                            model.index = Some(index + 1);
                            model.start_seconds = 0.0;
                            model.generation += 1;
                            model.reload_requested = true;
                            session = None;
                        } else {
                            model.status = PlaybackStatus::Stopped;
                            model.intent_playing = false;
                            model.start_seconds = active.position_seconds();
                            session = None;
                        }
                    }
                }
                Err(error) => {
                    model.status = PlaybackStatus::Failed;
                    model.error = Some(error.to_string());
                    session = None;
                }
            }
        }

        publish_snapshot(&model, session.as_ref(), &shared_snapshot);

        if session.is_some() {
            if backpressured {
                if let Ok(command) = commands.recv_timeout(Duration::from_millis(4))
                    && handle_command(command, &mut model, &mut session)
                {
                    break;
                }
            } else if let Ok(command) = commands.try_recv()
                && handle_command(command, &mut model, &mut session)
            {
                break;
            }
        } else {
            match commands.recv_timeout(Duration::from_millis(50)) {
                Ok(command) => {
                    if handle_command(command, &mut model, &mut session) {
                        break;
                    }
                }
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            }
        }
    }
}

fn handle_command(
    command: EngineCommand,
    model: &mut WorkerModel,
    session: &mut Option<PlaybackSession>,
) -> bool {
    match command {
        EngineCommand::Load(items) => {
            model.queue = items;
            model.index = (!model.queue.is_empty()).then_some(0);
            model.start_seconds = 0.0;
            model.intent_playing = true;
            model.reload_requested = model.index.is_some();
            model.status = PlaybackStatus::Loading;
            model.error = None;
            model.generation += 1;
            *session = None;
        }
        EngineCommand::Play => {
            if model.index.is_some() {
                if model.status == PlaybackStatus::Stopped {
                    model.start_seconds = 0.0;
                    model.status = PlaybackStatus::Loading;
                    model.generation += 1;
                }
                model.intent_playing = true;
                if session.is_none() {
                    model.reload_requested = true;
                }
            }
        }
        EngineCommand::Pause => {
            model.intent_playing = false;
            if let Some(active) = session.as_mut() {
                active.pause();
            }
            if model.index.is_some() {
                model.status = PlaybackStatus::Paused;
            }
        }
        EngineCommand::Seek(seconds) => {
            if let Some(index) = model.index {
                let duration = model.queue[index].public.duration_seconds;
                model.start_seconds = duration.map_or(seconds, |value| seconds.min(value));
                model.reload_requested = true;
                model.status = PlaybackStatus::Loading;
                model.error = None;
                model.generation += 1;
                *session = None;
            }
        }
        EngineCommand::SetVolume(volume) => {
            apply_volume_change(
                model,
                session.as_ref().map(|active| active.volume_bits.as_ref()),
                volume,
            );
        }
        EngineCommand::SetAudioMode(mode) => {
            let settings = mode.dsp_settings();
            if apply_dsp_change(model, session.as_mut(), settings) {
                model.audio_mode = mode;
            }
        }
        EngineCommand::SetDspSettings(settings) => {
            apply_dsp_change(model, session.as_mut(), settings);
        }
        EngineCommand::SetOutputDevice(name) => {
            model.output_device = name;
            if model.index.is_some() {
                if let Some(active) = session.as_ref() {
                    model.start_seconds = active.position_seconds();
                }
                model.reload_requested = true;
                model.status = PlaybackStatus::Loading;
                model.error = None;
                model.generation += 1;
                *session = None;
            }
        }
        EngineCommand::Next => {
            if let Some(index) = model.index
                && index + 1 < model.queue.len()
            {
                model.index = Some(index + 1);
                model.start_seconds = 0.0;
                model.reload_requested = true;
                model.status = PlaybackStatus::Loading;
                model.error = None;
                model.generation += 1;
                *session = None;
            }
        }
        EngineCommand::Previous => {
            if let Some(index) = model.index {
                if index > 0 {
                    model.index = Some(index - 1);
                    model.start_seconds = 0.0;
                } else {
                    model.start_seconds = 0.0;
                }
                model.reload_requested = true;
                model.status = PlaybackStatus::Loading;
                model.error = None;
                model.generation += 1;
                *session = None;
            }
        }
        EngineCommand::Shutdown => return true,
    }
    false
}

fn apply_volume_change(model: &mut WorkerModel, volume_bits: Option<&AtomicU32>, volume: f32) {
    model.volume = volume;
    if let Some(volume_bits) = volume_bits {
        volume_bits.store(volume.to_bits(), Ordering::Relaxed);
    }
}

fn apply_dsp_change(
    model: &mut WorkerModel,
    session: Option<&mut PlaybackSession>,
    settings: DspSettings,
) -> bool {
    if let Some(active) = session
        && let Err(error) = active.set_dsp_settings(settings.clone())
    {
        model.error = Some(format!("failed to update DSP settings: {error}"));
        return false;
    }
    model.dsp_settings = settings;
    model.error = None;
    true
}

fn local_queue_item(path: PathBuf) -> EngineQueueItem {
    let fallback_title = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("Untitled")
        .to_owned();
    let info = super::probe_local_file(&path).ok();
    let title = info
        .as_ref()
        .and_then(|info| info.title.clone())
        .unwrap_or(fallback_title);
    let duration_seconds = info.and_then(|info| info.duration_seconds);
    EngineQueueItem {
        public: QueueItem {
            location: path.display().to_string(),
            title,
            duration_seconds,
            online: false,
        },
        source: PlaybackSource::Local(path),
    }
}

fn publish_snapshot(
    model: &WorkerModel,
    session: Option<&PlaybackSession>,
    destination: &Mutex<EngineSnapshot>,
) {
    let duration_seconds = model
        .index
        .and_then(|index| model.queue.get(index))
        .and_then(|item| item.public.duration_seconds)
        .or_else(|| session.and_then(PlaybackSession::duration_seconds));
    let position_seconds = session
        .map(PlaybackSession::position_seconds)
        .unwrap_or(model.start_seconds);
    let underruns = session.map_or(0, PlaybackSession::underruns);
    let output_sample_rate = session.map(|session| session.output_sample_rate);
    *destination.lock().unwrap() = EngineSnapshot {
        status: model.status,
        queue: model.queue.iter().map(|item| item.public.clone()).collect(),
        queue_index: model.index,
        position_seconds,
        duration_seconds,
        volume: model.volume,
        audio_mode: model.audio_mode,
        dsp_settings: model.dsp_settings.clone(),
        generation: model.generation,
        underrun_callbacks: underruns,
        output_sample_rate,
        error: model.error.clone(),
        output_device: model.output_device.clone(),
    };
}

enum PumpResult {
    Progress,
    Backpressure,
    Ended,
}

struct PlaybackSession {
    media: OpenedMedia,
    rate_adapter: RateAdapter,
    dsp_chain: DspChain,
    sample_buffer: Option<SampleBuffer<f32>>,
    producer: HeapProd<f32>,
    stream: Stream,
    played_samples: Arc<AtomicU64>,
    underrun_callbacks: Arc<AtomicU64>,
    callback_enabled: Arc<AtomicBool>,
    volume_bits: Arc<AtomicU32>,
    source_channels: usize,
    output_sample_rate: u32,
    start_seconds: f64,
    duration_seconds: Option<f64>,
    prebuffer_samples: usize,
    pending: Vec<f32>,
    pending_offset: usize,
    eof: bool,
    flushed: bool,
    intent_playing: bool,
    stream_playing: bool,
    seek_discard_frames: usize,
}

impl PlaybackSession {
    fn new(
        source: &PlaybackSource,
        start_seconds: f64,
        volume: f32,
        dsp_settings: DspSettings,
        output_device_name: Option<&str>,
    ) -> Result<Self> {
        let (mut media, seek_discard_frames) = match source {
            PlaybackSource::Local(path) => {
                let mut media = open_media(path)?;
                seek_media(&mut media, start_seconds)?;
                (media, 0)
            }
            PlaybackSource::Online(request) => {
                let extension = media_extension(&request.media_type);
                let description = request.redacted_for_log();
                let http = HttpMediaSource::new(request.clone())?;
                let mut media = open_media_source(Box::new(http), extension, &description)?;
                let discard = seek_media_coarse(&mut media, start_seconds)?;
                (media, discard)
            }
        };
        let sample_rate = media
            .codec_params
            .sample_rate
            .context("audio track does not declare a sample rate")?;
        let channels = media
            .codec_params
            .channels
            .context("audio track does not declare a channel layout")?
            .count();
        let duration_seconds = media
            .codec_params
            .n_frames
            .map(|frames| frames as f64 / sample_rate as f64);

        let host = cpal::default_host();
        let device = match output_device_name {
            Some(name) => host
                .output_devices()
                .context("failed to enumerate output devices")?
                .find(|device| device.name().ok().as_deref() == Some(name))
                .with_context(|| format!("output device '{name}' is unavailable"))?,
            None => host
                .default_output_device()
                .context("no default audio output device is available")?,
        };
        let supported = choose_output_config(&device, sample_rate, channels as u16)?;
        let sample_format = supported.sample_format();
        let config: StreamConfig = supported.into();
        let output_sample_rate = config.sample_rate.0;
        let ring_capacity =
            ((output_sample_rate as f64 * channels as f64 * RING_SECONDS) as usize).max(4096);
        let ring = HeapRb::<f32>::new(ring_capacity);
        let (producer, consumer) = ring.split();
        let played_samples = Arc::new(AtomicU64::new(0));
        let underrun_callbacks = Arc::new(AtomicU64::new(0));
        let callback_enabled = Arc::new(AtomicBool::new(false));
        let volume_bits = Arc::new(AtomicU32::new(volume.to_bits()));
        let stream = build_engine_output_stream(
            &device,
            &config,
            sample_format,
            consumer,
            OutputCallbackCounters {
                played_samples: Arc::clone(&played_samples),
                underruns: Arc::clone(&underrun_callbacks),
                enabled: Arc::clone(&callback_enabled),
                volume_bits: Arc::clone(&volume_bits),
            },
        )?;
        let mut rate_adapter = RateAdapter::new(sample_rate, output_sample_rate, channels)?;
        let mut dsp_chain = DspChain::new(output_sample_rate, channels, dsp_settings)?;
        let prefetched = std::mem::take(&mut media.prefetched_samples);
        let mut pending = if prefetched.is_empty() {
            Vec::new()
        } else {
            rate_adapter.process(&prefetched)?
        };
        dsp_chain.process_interleaved_in_place(&mut pending)?;

        Ok(Self {
            media,
            rate_adapter,
            dsp_chain,
            sample_buffer: None,
            producer,
            stream,
            played_samples,
            underrun_callbacks,
            callback_enabled,
            volume_bits,
            source_channels: channels,
            output_sample_rate,
            start_seconds,
            duration_seconds,
            prebuffer_samples: (output_sample_rate as f64 * channels as f64 * PREBUFFER_SECONDS)
                as usize,
            pending,
            pending_offset: 0,
            eof: false,
            flushed: false,
            intent_playing: true,
            stream_playing: false,
            seek_discard_frames,
        })
    }

    fn pump(&mut self) -> Result<PumpResult> {
        if self.pending_offset < self.pending.len() {
            while self.pending_offset < self.pending.len() {
                let sample = self.pending[self.pending_offset];
                match self.producer.try_push(sample) {
                    Ok(()) => self.pending_offset += 1,
                    Err(_) => {
                        self.maybe_start()?;
                        return Ok(PumpResult::Backpressure);
                    }
                }
            }
            self.pending.clear();
            self.pending_offset = 0;
            self.maybe_start()?;
            return Ok(PumpResult::Progress);
        }

        if self.eof {
            if !self.flushed {
                self.pending = self.rate_adapter.finish()?;
                self.dsp_chain
                    .process_interleaved_in_place(&mut self.pending)?;
                self.flushed = true;
                if !self.pending.is_empty() {
                    return Ok(PumpResult::Progress);
                }
            }
            self.maybe_start()?;
            if self.producer.is_empty() {
                return Ok(PumpResult::Ended);
            }
            return Ok(PumpResult::Backpressure);
        }

        let packet = match self.media.format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(error)) if error.kind() == ErrorKind::UnexpectedEof => {
                self.eof = true;
                return Ok(PumpResult::Progress);
            }
            Err(error) => return Err(error).context("failed to read media packet"),
        };
        if packet.track_id() != self.media.track_id {
            return Ok(PumpResult::Progress);
        }
        let decoded = match self.media.decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(SymphoniaError::DecodeError(_)) => return Ok(PumpResult::Progress),
            Err(error) => return Err(error).context("failed to decode media packet"),
        };
        if self.sample_buffer.is_none() {
            self.sample_buffer = Some(SampleBuffer::<f32>::new(
                decoded.capacity() as u64,
                *decoded.spec(),
            ));
        }
        let buffer = self
            .sample_buffer
            .as_mut()
            .expect("sample buffer initialized");
        buffer.copy_interleaved_ref(decoded);
        let mut samples = buffer.samples();
        if self.seek_discard_frames > 0 {
            let frames = samples.len() / self.source_channels;
            let discard = frames.min(self.seek_discard_frames);
            samples = &samples[discard * self.source_channels..];
            self.seek_discard_frames -= discard;
            if samples.is_empty() {
                return Ok(PumpResult::Progress);
            }
        }
        self.pending = self.rate_adapter.process(samples)?;
        self.dsp_chain
            .process_interleaved_in_place(&mut self.pending)?;
        Ok(PumpResult::Progress)
    }

    fn maybe_start(&mut self) -> Result<()> {
        let enough = self.producer.occupied_len() >= self.prebuffer_samples;
        if self.intent_playing && !self.stream_playing && (enough || self.eof) {
            self.callback_enabled.store(true, Ordering::Release);
            self.stream.play()?;
            self.stream_playing = true;
        }
        Ok(())
    }

    fn pause(&mut self) {
        self.intent_playing = false;
        if self.stream_playing {
            self.callback_enabled.store(false, Ordering::Release);
            let _ = self.stream.pause();
            self.stream_playing = false;
        }
    }

    fn resume(&mut self) {
        self.intent_playing = true;
        let _ = self.maybe_start();
    }

    fn has_started(&self) -> bool {
        self.stream_playing
    }

    fn set_dsp_settings(&mut self, settings: DspSettings) -> Result<()> {
        self.dsp_chain.set_settings(settings)?;
        Ok(())
    }

    fn position_seconds(&self) -> f64 {
        let played_frames =
            self.played_samples.load(Ordering::Relaxed) as f64 / self.source_channels as f64;
        self.start_seconds + played_frames / self.output_sample_rate as f64
    }

    fn duration_seconds(&self) -> Option<f64> {
        self.duration_seconds
    }

    fn underruns(&self) -> u64 {
        self.underrun_callbacks.load(Ordering::Relaxed)
    }
}

fn media_extension(media_type: &MediaType) -> Option<&'static str> {
    match media_type {
        MediaType::Mp3 => Some("mp3"),
        MediaType::Flac => Some("flac"),
        MediaType::Aac => Some("aac"),
        MediaType::Ogg => Some("ogg"),
        MediaType::Wav => Some("wav"),
        MediaType::Hls | MediaType::Unknown => None,
    }
}

#[derive(Clone)]
struct OutputCallbackCounters {
    played_samples: Arc<AtomicU64>,
    underruns: Arc<AtomicU64>,
    enabled: Arc<AtomicBool>,
    volume_bits: Arc<AtomicU32>,
}

fn build_engine_output_stream(
    device: &cpal::Device,
    config: &StreamConfig,
    sample_format: SampleFormat,
    consumer: HeapCons<f32>,
    counters: OutputCallbackCounters,
) -> Result<Stream> {
    match sample_format {
        SampleFormat::F32 => build_typed_engine_stream::<f32>(device, config, consumer, counters),
        SampleFormat::I16 => build_typed_engine_stream::<i16>(device, config, consumer, counters),
        SampleFormat::U16 => build_typed_engine_stream::<u16>(device, config, consumer, counters),
        other => bail!("unsupported output sample format: {other}"),
    }
}

fn build_typed_engine_stream<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    mut consumer: HeapCons<f32>,
    counters: OutputCallbackCounters,
) -> Result<Stream>
where
    T: Sample + SizedSample + FromSample<f32>,
{
    let stream = device.build_output_stream(
        config,
        move |output: &mut [T], _| render_output_callback(output, &mut consumer, &counters),
        |error| eprintln!("audio output stream error: {error}"),
        None,
    )?;
    Ok(stream)
}

#[inline]
fn render_output_callback<T>(
    output: &mut [T],
    consumer: &mut HeapCons<f32>,
    counters: &OutputCallbackCounters,
) where
    T: Sample + SizedSample + FromSample<f32>,
{
    let enabled = counters.enabled.load(Ordering::Acquire);
    let volume = f32::from_bits(counters.volume_bits.load(Ordering::Relaxed));
    let mut starved = false;
    let mut consumed = 0u64;
    for target in output {
        let sample = match consumer.try_pop() {
            Some(value) => {
                consumed += 1;
                value * volume
            }
            None => {
                starved = enabled;
                0.0
            }
        };
        *target = T::from_sample(sample);
    }
    counters
        .played_samples
        .fetch_add(consumed, Ordering::Relaxed);
    if starved {
        counters.underruns.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    use super::*;

    thread_local! {
        static TRACK_ALLOCATIONS: Cell<bool> = const { Cell::new(false) };
        static ALLOCATION_COUNT: Cell<usize> = const { Cell::new(0) };
    }

    struct CountingAllocator;

    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            TRACK_ALLOCATIONS.with(|enabled| {
                if enabled.get() {
                    ALLOCATION_COUNT.with(|count| count.set(count.get() + 1));
                }
            });
            unsafe { System.alloc(layout) }
        }

        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            unsafe { System.dealloc(pointer, layout) };
        }

        unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            TRACK_ALLOCATIONS.with(|enabled| {
                if enabled.get() {
                    ALLOCATION_COUNT.with(|count| count.set(count.get() + 1));
                }
            });
            unsafe { System.realloc(pointer, layout, new_size) }
        }
    }

    #[global_allocator]
    static TEST_ALLOCATOR: CountingAllocator = CountingAllocator;

    #[test]
    fn volume_hot_update_changes_atomic_without_reloading() {
        let mut model = WorkerModel {
            status: PlaybackStatus::Playing,
            generation: 7,
            ..WorkerModel::default()
        };
        let volume_bits = AtomicU32::new(1.0f32.to_bits());

        apply_volume_change(&mut model, Some(&volume_bits), 0.35);

        assert_eq!(model.volume, 0.35);
        assert_eq!(f32::from_bits(volume_bits.load(Ordering::Relaxed)), 0.35);
        assert!(!model.reload_requested);
        assert_eq!(model.status, PlaybackStatus::Playing);
        assert_eq!(model.generation, 7);
    }

    #[test]
    fn stopped_play_restarts_from_zero() {
        let mut model = WorkerModel {
            queue: vec![EngineQueueItem {
                public: QueueItem {
                    location: "dummy.wav".into(),
                    title: "Dummy".into(),
                    duration_seconds: Some(120.0),
                    online: false,
                },
                source: PlaybackSource::Local(PathBuf::from("dummy.wav")),
            }],
            index: Some(0),
            status: PlaybackStatus::Stopped,
            start_seconds: 120.0,
            generation: 4,
            ..WorkerModel::default()
        };
        let mut session = None;

        assert!(!handle_command(
            EngineCommand::Play,
            &mut model,
            &mut session
        ));

        assert_eq!(model.start_seconds, 0.0);
        assert_eq!(model.status, PlaybackStatus::Loading);
        assert!(model.intent_playing);
        assert!(model.reload_requested);
        assert_eq!(model.generation, 5);
    }

    #[test]
    fn audio_callback_path_allocates_nothing_and_uses_only_atomics() {
        let ring = HeapRb::<f32>::new(256);
        let (mut producer, mut consumer) = ring.split();
        for _ in 0..128 {
            producer.try_push(1.0).unwrap();
        }
        let volume_bits = Arc::new(AtomicU32::new(0.5f32.to_bits()));
        let counters = OutputCallbackCounters {
            played_samples: Arc::new(AtomicU64::new(0)),
            underruns: Arc::new(AtomicU64::new(0)),
            enabled: Arc::new(AtomicBool::new(true)),
            volume_bits: Arc::clone(&volume_bits),
        };
        let mut output = [0.0f32; 128];
        ALLOCATION_COUNT.with(|count| count.set(0));
        TRACK_ALLOCATIONS.with(|enabled| enabled.set(true));
        render_output_callback(&mut output, &mut consumer, &counters);
        for sample in &output {
            assert_eq!(*sample, 0.5);
        }
        for _ in 0..128 {
            producer.try_push(1.0).unwrap();
        }
        volume_bits.store(0.25f32.to_bits(), Ordering::Relaxed);
        render_output_callback(&mut output, &mut consumer, &counters);
        TRACK_ALLOCATIONS.with(|enabled| enabled.set(false));
        assert_eq!(ALLOCATION_COUNT.with(Cell::get), 0);
        for sample in &output {
            assert_eq!(*sample, 0.25);
        }
        assert_eq!(counters.played_samples.load(Ordering::Relaxed), 256);
        assert_eq!(counters.underruns.load(Ordering::Relaxed), 0);
    }
}
